#!/usr/bin/env python3
"""Install the closing-block adapters as one verified, recoverable bundle."""

from __future__ import annotations

import argparse
import errno
import hashlib
import json
import os
import re
import shutil
import sys
import tempfile
import time
import tomllib
from contextlib import contextmanager
from datetime import datetime, timezone
from pathlib import Path


RUNTIME_FILES = (
    "closing_block.py",
    "closing_block_record.py",
    "herdr_status.py",
    "herdr-closing-block.py",
    "herdr-codex-notify.py",
)
DATA_FILES = ("closing_block_schema.json",)
VERSION_RE = re.compile(r"^# HERDR_INTEGRATION_VERSION=(?P<version>\d+)$", re.MULTILINE)


class BundleValidationError(ValueError):
    pass


def bundle_manifest(source_dir: Path) -> dict[str, dict[str, str | int]]:
    manifest: dict[str, dict[str, str | int]] = {}
    versions: set[int] = set()
    for name in (*RUNTIME_FILES, *DATA_FILES):
        path = source_dir / name
        if not path.is_file() or path.is_symlink():
            raise BundleValidationError(f"missing regular source file: {name}")
        try:
            data = path.read_bytes()
            text = data.decode("utf-8")
            if name in RUNTIME_FILES:
                compile(text, str(path), "exec")
            elif name == "closing_block_schema.json":
                json.loads(text)
        except (OSError, UnicodeDecodeError, SyntaxError, ValueError) as error:
            raise BundleValidationError(f"invalid source file {name}: {error}") from error
        matches = VERSION_RE.findall(text) if name in RUNTIME_FILES else []
        if name in RUNTIME_FILES:
            if len(matches) != 1:
                raise BundleValidationError(f"expected one integration version in {name}")
            version = int(matches[0])
            versions.add(version)
            manifest[name] = {"version": version, "sha256": hashlib.sha256(data).hexdigest()}
        else:
            manifest[name] = {"sha256": hashlib.sha256(data).hexdigest()}
    if len(versions) != 1:
        raise BundleValidationError("runtime module integration versions do not match")
    return manifest


def _verify_bundle(
    bundle_dir: Path, expected: dict[str, dict[str, str | int]]
) -> None:
    actual = bundle_manifest(bundle_dir)
    if actual != expected:
        raise BundleValidationError("installed bundle hash or version mismatch")


def _backup_path(target: Path) -> Path:
    timestamp = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    candidate = target.parent / f"{target.name}.backup-{timestamp}"
    suffix = 1
    while candidate.exists():
        candidate = target.parent / f"{target.name}.backup-{timestamp}-{suffix}"
        suffix += 1
    return candidate


def _validate_target(target: Path) -> None:
    if target.is_symlink():
        raise BundleValidationError(f"install target must not be a symlink: {target}")
    if target.exists() and not target.is_dir():
        raise BundleValidationError(f"install target is not a directory: {target}")


@contextmanager
def _exclusive_install_lock(target: Path):
    # Keep this file in place so every installer locks the same inode.
    lock_path = target.parent / f".{target.name}.install.lock"
    with lock_path.open("a+b") as lock:
        if os.name == "nt":
            import msvcrt

            lock.seek(0, os.SEEK_END)
            if lock.tell() == 0:
                lock.write(b"\0")
                lock.flush()
            lock.seek(0)
            while True:
                try:
                    msvcrt.locking(lock.fileno(), msvcrt.LK_NBLCK, 1)
                    break
                except OSError as error:
                    if error.errno not in (errno.EACCES, errno.EAGAIN, errno.EDEADLK):
                        raise
                    time.sleep(0.05)
            try:
                yield
            finally:
                lock.seek(0)
                msvcrt.locking(lock.fileno(), msvcrt.LK_UNLCK, 1)
        else:
            import fcntl

            fcntl.flock(lock.fileno(), fcntl.LOCK_EX)
            try:
                yield
            finally:
                fcntl.flock(lock.fileno(), fcntl.LOCK_UN)


def _file_identity(path: Path) -> tuple[int, int]:
    stat = path.stat()
    return stat.st_dev, stat.st_ino


def _replace_runtime_files(
    source: Path, target: Path, written: list[tuple[Path, tuple[int, int]]]
) -> None:
    target.mkdir(parents=True, exist_ok=True)
    for name in (*RUNTIME_FILES, *DATA_FILES):
        staged_file = source / name
        destination = target / name
        written.append((destination, _file_identity(staged_file)))
        os.replace(staged_file, destination)


def _restore_runtime_files(backup: Path, target: Path) -> None:
    restore = Path(tempfile.mkdtemp(prefix=f".{target.name}.restore-", dir=target.parent))
    try:
        for name in (*RUNTIME_FILES, *DATA_FILES):
            source = backup / name
            destination = target / name
            if not source.exists() and not source.is_symlink():
                if destination.exists() or destination.is_symlink():
                    destination.unlink()
                continue
            if source.is_symlink():
                os.symlink(os.readlink(source), restore / name)
            else:
                shutil.copy2(source, restore / name)
            os.replace(restore / name, destination)
    finally:
        shutil.rmtree(restore, ignore_errors=True)


def _rollback_runtime_files(
    backup: Path | None,
    target: Path,
    written: list[tuple[Path, tuple[int, int]]],
) -> None:
    if backup and backup.exists():
        _restore_runtime_files(backup, target)
        return
    for path, identity in written:
        try:
            if _file_identity(path) == identity:
                path.unlink()
        except FileNotFoundError:
            pass
    try:
        target.rmdir()
    except FileNotFoundError:
        pass
    except OSError as error:
        if error.errno not in (errno.ENOTEMPTY, errno.EEXIST):
            raise


def _codex_config_paths() -> list[Path]:
    """Return existing Codex configs once, following profile symlinks."""
    home = Path.home()
    candidates = [home / ".codex" / "config.toml"]
    profiles = home / ".codex-profiles"
    if profiles.is_dir():
        candidates.extend(
            profile / "config.toml"
            for profile in sorted(profiles.iterdir())
            if profile.is_dir()
        )
    paths: list[Path] = []
    seen: set[Path] = set()
    for candidate in candidates:
        if not candidate.is_file():
            continue
        resolved = candidate.resolve()
        if resolved not in seen:
            seen.add(resolved)
            paths.append(resolved)
    return paths


def _notify_values(value: object) -> list[str]:
    if isinstance(value, str):
        return [value]
    if isinstance(value, list):
        return [item for item in value if isinstance(item, str)]
    return []


def _notify_is_herdr(values: list[str], target: Path) -> bool:
    handlers = {
        str(target / "herdr-codex-notify.py"),
        str(target / "codex-notify-chain.sh"),
    }
    return any(Path(value).expanduser().absolute().as_posix() in handlers for value in values)


def _config_backup_path(config: Path) -> Path:
    timestamp = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    candidate = config.parent / f"{config.name}.backup-{timestamp}"
    suffix = 1
    while candidate.exists():
        candidate = config.parent / f"{config.name}.backup-{timestamp}-{suffix}"
        suffix += 1
    return candidate


def _wire_codex_notify(target: Path, *, dry_run: bool) -> dict[str, list[str]]:
    result = {
        "wired": [],
        "already_wired": [],
        "notify_conflict": [],
        "skipped": [],
    }
    for config in _codex_config_paths():
        try:
            data = config.read_bytes()
            parsed = tomllib.loads(data.decode("utf-8"))
        except (OSError, UnicodeDecodeError, tomllib.TOMLDecodeError):
            result["skipped"].append(str(config))
            continue

        if "notify" in parsed:
            values = _notify_values(parsed["notify"])
            if _notify_is_herdr(values, target):
                result["already_wired"].append(str(config))
            else:
                result["notify_conflict"].append(str(config))
            continue

        result["wired"].append(str(config))
        if dry_run:
            continue
        mode = config.stat().st_mode & 0o7777
        backup = _config_backup_path(config)
        shutil.copy2(config, backup)
        content = data.decode("utf-8")
        notify = json.dumps(["python3", str(target / "herdr-codex-notify.py")])
        updated = f"notify = {notify}\n" + content
        fd, temporary = tempfile.mkstemp(prefix=f".{config.name}.", dir=config.parent)
        temporary_path = Path(temporary)
        try:
            os.fchmod(fd, mode)
            with os.fdopen(fd, "w", encoding="utf-8", newline="") as stream:
                stream.write(updated)
                stream.flush()
                os.fsync(stream.fileno())
            os.replace(temporary_path, config)
        except BaseException:
            try:
                os.close(fd)
            except OSError:
                pass
            temporary_path.unlink(missing_ok=True)
            raise
    return result


def install_bundle(source_dir: Path, target: Path, *, dry_run: bool) -> dict:
    source_dir = Path(source_dir).resolve()
    target = Path(target).expanduser().absolute()
    expected = bundle_manifest(source_dir)
    if dry_run:
        _validate_target(target)
        backup = _backup_path(target) if target.exists() else None
        result = {
            "mode": "dry-run",
            "target": str(target),
            "backup": backup.name if backup else None,
            "files": expected,
        }
        result["codex_notify"] = _wire_codex_notify(target, dry_run=True)
        return result

    target.parent.mkdir(parents=True, exist_ok=True)
    with _exclusive_install_lock(target):
        _validate_target(target)
        bundle_needs_update = not target.exists()
        if target.exists():
            try:
                bundle_needs_update = bundle_manifest(target) != expected
            except BundleValidationError:
                bundle_needs_update = True
        backup = _backup_path(target) if target.exists() and bundle_needs_update else None
        result = {
            "mode": "installed",
            "target": str(target),
            "backup": backup.name if backup else None,
            "files": expected,
        }
        if not bundle_needs_update:
            result["codex_notify"] = _wire_codex_notify(target, dry_run=False)
            return result

        stage = Path(
            tempfile.mkdtemp(prefix=f".{target.name}.stage-", dir=target.parent)
        )
        try:
            if target.exists():
                shutil.copytree(target, stage, dirs_exist_ok=True, symlinks=True)
            for name in (*RUNTIME_FILES, *DATA_FILES):
                staged_file = stage / name
                if staged_file.exists() or staged_file.is_symlink():
                    staged_file.unlink()
                shutil.copy2(source_dir / name, staged_file)
            _verify_bundle(stage, expected)

            if backup:
                shutil.copytree(target, backup, symlinks=True)
            written: list[tuple[Path, tuple[int, int]]] = []
            try:
                _replace_runtime_files(stage, target, written)
            except OSError:
                _rollback_runtime_files(backup, target, written)
                raise

            try:
                _verify_bundle(target, expected)
            except (BundleValidationError, OSError):
                _rollback_runtime_files(backup, target, written)
                raise
            result["codex_notify"] = _wire_codex_notify(target, dry_run=False)
            return result
        finally:
            if stage.exists():
                shutil.rmtree(stage)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--target",
        type=Path,
        default=Path.home() / ".local" / "share" / "herdr-closing-block",
    )
    parser.add_argument("--dry-run", action="store_true")
    args = parser.parse_args(argv)
    try:
        result = install_bundle(Path(__file__).parent, args.target, dry_run=args.dry_run)
    except (BundleValidationError, OSError) as error:
        print(f"closing-block install failed: {error}", file=sys.stderr)
        return 1
    print(json.dumps(result, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
