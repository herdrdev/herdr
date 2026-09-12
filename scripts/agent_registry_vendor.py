#!/usr/bin/env python3
"""Explicit, offline agent-registry vendoring (never invoked by a build).

Sync snapshots LOCALDIR/agents, pins the exact UTF-8 bytes, and writes the
checked-in Rust include index. The lock intentionally uses a content digest,
not a fabricated Git revision: uncommitted source trees are supported. Package
semantics are validated by the Rust registry CLI, not duplicated here.

The aggregate SHA-256 covers sorted UTF-8 lines: '<file sha256>  <path>\n'.
Only this maintenance command writes vendor files; --check never writes or
consults the source repository, Git, or the network. Python 3.11+ is required.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import stat
import subprocess
import sys
import tempfile


PROJECT_ROOT = Path(__file__).resolve().parents[1]
REPOSITORY = "https://github.com/herdrdev/agent-registry"
VENDOR_PATH = Path("vendor/agent-registry")
INDEX_PATH = Path("src/agents/bundled.rs")
MAX_SNAPSHOT_BYTES = 64 * 1024 * 1024
MAX_FILES = 4096
MAX_ENTRIES = 8192
MAX_FILE_BYTES = 1024 * 1024
MAX_TOTAL_BYTES = 32 * 1024 * 1024
MAX_METADATA_BYTES = 4 * 1024 * 1024
MAX_PATH_BYTES = 240
MAX_DEPTH = 12
COMPONENT_RE = re.compile(r"[A-Za-z0-9_][A-Za-z0-9_.-]*\Z")
RESERVED = {"CON", "PRN", "AUX", "NUL"} | {
    f"{prefix}{number}" for prefix in ("COM", "LPT") for number in range(1, 10)
}


class VendorError(ValueError):
    pass


def validate_path(path: str) -> None:
    """Use portable relative paths, also safe as Rust string literals."""
    parts = path.split("/")
    if len(path.encode("utf-8")) > MAX_PATH_BYTES or len(parts) > MAX_DEPTH:
        raise VendorError(f"path exceeds limits: {path!r}")
    for part in parts:
        if (
            not COMPONENT_RE.fullmatch(part)
            or part.endswith(".")
            or part.split(".")[0].upper() in RESERVED
        ):
            raise VendorError(f"unsafe path: {path!r}")


def reject_symlink_ancestors(path: Path) -> None:
    for candidate in (path, *path.parents):
        if candidate.is_symlink():
            raise VendorError(f"symlink is not allowed: {candidate}")


def read_regular(path: Path, limit: int | None = None) -> bytes:
    if limit is None:
        limit = MAX_FILE_BYTES
    mode = path.lstat().st_mode
    if not stat.S_ISREG(mode):
        raise VendorError(f"not a regular file (symlinks are forbidden): {path}")
    with path.open("rb") as stream:
        content = stream.read(limit + 1)
    if len(content) > limit:
        raise VendorError(f"file exceeds byte limit: {path}")
    try:
        text = content.decode("utf-8")
    except UnicodeDecodeError as exc:
        raise VendorError(f"not UTF-8: {path}") from exc
    if any(ord(char) < 32 and char not in "\t\n\r" for char in text) or "\x7f" in text:
        raise VendorError(f"binary/control bytes are forbidden: {path}")
    return content


def snapshot_agents(root: Path) -> dict[str, bytes]:
    agents = root / "agents"
    reject_symlink_ancestors(agents)
    if not agents.is_dir():
        raise VendorError(f"missing agents directory: {agents}")
    files: dict[str, bytes] = {}
    seen: set[str] = set()
    total_bytes = 0
    entries = 0

    def walk(directory: Path) -> None:
        nonlocal entries, total_bytes
        # Iterate rather than materializing an unbounded directory listing.
        with os.scandir(directory) as children:
            for child in children:
                entries += 1
                if entries > MAX_ENTRIES:
                    raise VendorError("tree exceeds entry count limit")
                path = Path(child.path)
                relative = path.relative_to(root).as_posix()
                validate_path(relative)
                folded = relative.casefold()
                if folded in seen:
                    raise VendorError(f"casefold path collision: {relative}")
                seen.add(folded)
                if child.is_symlink():
                    raise VendorError(f"symlink is not allowed: {path}")
                if child.is_dir(follow_symlinks=False):
                    walk(path)
                else:
                    if len(files) >= MAX_FILES:
                        raise VendorError("tree exceeds file count limit")
                    content = read_regular(path)
                    total_bytes += len(content)
                    if total_bytes > MAX_TOTAL_BYTES:
                        raise VendorError("tree exceeds total byte limit")
                    files[relative] = content

    walk(agents)
    if not files:
        raise VendorError("agents tree has no files")
    return dict(sorted(files.items()))


def lock_bytes(files: dict[str, bytes]) -> bytes:
    records = [
        {"path": path, "sha256": hashlib.sha256(files[path]).hexdigest()}
        for path in sorted(files)
    ]
    aggregate = "".join(f"{item['sha256']}  {item['path']}\n" for item in records)
    lock = {
        "schema": 1,
        "repository": REPOSITORY,
        "sha256": hashlib.sha256(aggregate.encode("utf-8")).hexdigest(),
        "files": records,
    }
    return (json.dumps(lock, indent=2) + "\n").encode("utf-8")


def index_bytes(files: dict[str, bytes]) -> bytes:
    lines = [
        "// Generated by scripts/agent_registry_vendor.py; do not edit.",
        "#[rustfmt::skip]",
        "pub(super) const FILES: &[(&str, &str)] = &[",
    ]
    for path in sorted(files):
        lines.append(
            f'    ("{path}", include_str!(concat!(env!("CARGO_MANIFEST_DIR"), '
            f'"/vendor/agent-registry/{path}"))),'
        )
    lines.extend(["];", ""])
    return "\n".join(lines).encode("utf-8")


def check_paths(vendor: Path, index: Path) -> None:
    reject_symlink_ancestors(vendor)
    reject_symlink_ancestors(index)
    if not vendor.is_dir():
        raise VendorError(f"missing vendored registry: {vendor}")
    with os.scandir(vendor) as children:
        for child in children:
            if child.name not in {"agents", "lock.json"}:
                raise VendorError(f"unexpected vendored entry: {child.name}")
    files = snapshot_agents(vendor)
    # Canonical byte equality checks the exact sorted file set, every file hash,
    # aggregate digest, repository URL, schema, and rejects duplicate JSON keys.
    if read_regular(vendor / "lock.json", MAX_METADATA_BYTES) != lock_bytes(files):
        raise VendorError("lock.json does not match the exact vendored content")
    if read_regular(index, MAX_METADATA_BYTES) != index_bytes(files):
        raise VendorError("generated bundled.rs is out of date")


def check(project_root: Path = PROJECT_ROOT) -> None:
    check_paths(project_root / VENDOR_PATH, project_root / INDEX_PATH)


def sync(source: Path, project_root: Path = PROJECT_ROOT) -> None:
    # Validate and snapshot ALL input before touching the existing vendor/index.
    files = snapshot_agents(source)
    vendor = project_root / VENDOR_PATH
    index = project_root / INDEX_PATH
    reject_symlink_ancestors(vendor)
    reject_symlink_ancestors(index)
    if vendor.exists() and not vendor.is_dir():
        raise VendorError(f"vendor destination is not a directory: {vendor}")
    if index.exists() and not index.is_file():
        raise VendorError(f"index destination is not a file: {index}")
    vendor.parent.mkdir(parents=True, exist_ok=True)
    index.parent.mkdir(parents=True, exist_ok=True)
    # Same filesystem as the destination for rename; backups survive until both
    # replacements succeed. An index replacement failure rolls the vendor back.
    with tempfile.TemporaryDirectory(prefix=".agent-registry-", dir=vendor.parent) as tmp:
        stage = Path(tmp)
        staged_vendor = stage / "registry"
        for path, content in files.items():
            target = staged_vendor / path
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes(content)
        (staged_vendor / "lock.json").write_bytes(lock_bytes(files))
        staged_index = stage / "bundled.rs"
        staged_index.write_bytes(index_bytes(files))
        check_paths(staged_vendor, staged_index)
        backup = stage / "previous"
        had_vendor = vendor.exists()
        if had_vendor:
            os.replace(vendor, backup)
        installed = False
        try:
            os.replace(staged_vendor, vendor)
            installed = True
            os.replace(staged_index, index)
        except OSError:
            if installed:
                shutil.rmtree(vendor)
            if had_vendor:
                os.replace(backup, vendor)
            raise


def _unique_object(pairs: list[tuple[str, object]]) -> dict:
    result = {}
    for key, value in pairs:
        if key in result:
            raise VendorError(f"duplicate JSON field: {key}")
        result[key] = value
    return result


def snapshot_files(raw: bytes) -> dict[str, bytes]:
    """Integrity/framing only; the supplied Herdr binary owns semantics."""
    if not 0 < len(raw) <= MAX_SNAPSHOT_BYTES:
        raise VendorError("snapshot JSON byte limit")
    try:
        value = json.loads(raw.decode("utf-8"), object_pairs_hook=_unique_object)
        if set(value) != {"schema", "compatibility", "source", "content_sha256", "files"} or type(value["schema"]) is not int or value["schema"] != 1:
            raise VendorError("unsupported snapshot fields/schema")
        compatibility = value["compatibility"]
        if compatibility != {"registry_api": 1, "min_detection_engine": 3} or any(type(v) is not int for v in compatibility.values()):
            raise VendorError("unsupported snapshot compatibility")
        source = value["source"]
        if set(source) != {"repository", "commit", "agents_tree", "dirty"} or source["repository"] != REPOSITORY or source["dirty"] is not False:
            raise VendorError("snapshot must have clean committed source provenance")
        for field in ("commit", "agents_tree"):
            if not re.fullmatch(r"(?:[0-9a-f]{40}|[0-9a-f]{64})", source[field]):
                raise VendorError(f"invalid source {field}")
        records = value["files"]
        if not isinstance(records, list) or not 0 < len(records) <= MAX_FILES:
            raise VendorError("snapshot file count limit")
        files = {}
        previous = ""
        total = 0
        for item in records:
            if set(item) != {"path", "bytes", "sha256", "text"}:
                raise VendorError("invalid file record fields")
            path = item["path"]
            validate_path(path)
            if not path.startswith("agents/") or path <= previous:
                raise VendorError("snapshot paths must be sorted and unique under agents/")
            previous = path
            content = item["text"].encode("utf-8")
            total += len(content)
            limit = 256 * 1024 if path.endswith(".toml") else MAX_FILE_BYTES
            if len(content) > limit or total > MAX_TOTAL_BYTES:
                raise VendorError("snapshot decoded byte limit")
            if type(item["bytes"]) is not int or item["bytes"] != len(content) or item["sha256"] != hashlib.sha256(content).hexdigest():
                raise VendorError("snapshot file length/hash mismatch")
            files[path] = content
        if value["content_sha256"] != json.loads(lock_bytes(files))["sha256"]:
            raise VendorError("snapshot inventory hash mismatch")
        return files
    except (UnicodeError, ValueError, TypeError, KeyError, AttributeError) as exc:
        raise VendorError(f"invalid snapshot: {exc}") from exc


def sync_snapshot(snapshot: Path, digest: str, validator: Path, project_root: Path = PROJECT_ROOT) -> None:
    """Offline immutable import, with exact-byte validation before any mutation.

    Keep the existing lock/index format and ownership. The lock's inventory
    digest pins source bytes; reviewed remote digest/commit belong in release
    review, not fabricated Git provenance for ordinary dirty local syncs.
    """
    if not re.fullmatch(r"[0-9a-f]{64}", digest):
        raise VendorError("expected a reviewed lowercase SHA-256")
    if not validator.is_absolute():
        raise VendorError("validator must be an absolute executable path")
    reject_symlink_ancestors(snapshot)
    raw = read_regular(snapshot, MAX_SNAPSHOT_BYTES)
    if hashlib.sha256(raw).hexdigest() != digest:
        raise VendorError("snapshot SHA-256 mismatch")
    files = snapshot_files(raw)
    with tempfile.TemporaryDirectory(prefix="herdr-vendor-snapshot-") as tmp:
        # Our OS-provided temp root can use a symlink alias, such as macOS /var.
        stage = Path(tmp).resolve()
        exact = stage / "snapshot.json"
        exact.write_bytes(raw)
        exact.chmod(0o400)
        try:
            subprocess.run([str(validator), "registry", "validate-snapshot", str(exact), "--runtime-compatible"], check=True, timeout=120)
        except (subprocess.SubprocessError, OSError) as exc:
            raise VendorError(f"Herdr snapshot validation failed: {exc}") from exc
        if read_regular(exact, MAX_SNAPSHOT_BYTES) != raw:
            raise VendorError("validator modified snapshot bytes")
        source = stage / "source"
        for path, content in files.items():
            target = source / path
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes(content)
        # Reuse existing portable-path/collision/control checks and rollback.
        sync(source, project_root)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", nargs="?", choices=["sync", "sync-snapshot"])
    parser.add_argument("--source", type=Path, help="local source repository (sync only)")
    parser.add_argument("--check", action="store_true", help="verify the checked-in snapshot offline")
    parser.add_argument("--snapshot", type=Path, help="verified local immutable snapshot JSON")
    parser.add_argument("--sha256", help="reviewed exact snapshot SHA-256")
    parser.add_argument("--validator", type=Path, help="absolute matching Herdr executable")
    args = parser.parse_args(argv)
    immutable_args = (args.snapshot, args.sha256, args.validator)
    if args.command == "sync-snapshot":
        if args.check or args.source is not None or not all(immutable_args):
            parser.error("sync-snapshot requires --snapshot FILE --sha256 DIGEST --validator /absolute/herdr")
    elif any(immutable_args):
        parser.error("snapshot arguments require sync-snapshot")
    if args.command == "sync":
        if args.check or args.source is None:
            parser.error("sync requires --source LOCALDIR and cannot use --check")
    elif args.command != "sync-snapshot" and (not args.check or args.source is not None):
        parser.error("use sync --source LOCALDIR or --check")
    try:
        if args.command == "sync-snapshot":
            sync_snapshot(args.snapshot, args.sha256, args.validator)
        elif args.command == "sync":
            sync(args.source)
        else:
            check()
    except (OSError, VendorError) as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 1
    print("agent registry snapshot ok")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
