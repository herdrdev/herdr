from __future__ import annotations

import argparse
import json
import shutil
import subprocess
import sys
import tarfile
import tempfile
from dataclasses import asdict, dataclass
from pathlib import Path


@dataclass
class VendorMetadata:
    source_commit: str
    dist_archive: str
    extracted_dir: str


def load_patch_series(patch_dir: Path) -> list[Path]:
    series_path = patch_dir / "series"
    if not series_path.exists():
        raise FileNotFoundError(f"missing patch series {series_path}")

    names = [
        line.strip()
        for line in series_path.read_text().splitlines()
        if line.strip() and not line.lstrip().startswith("#")
    ]
    if len(names) != len(set(names)):
        raise ValueError(f"duplicate patch in {series_path}")
    for name in names:
        if Path(name).name != name or not name.endswith(".patch"):
            raise ValueError(f"invalid patch name in {series_path}: {name}")

    listed = {patch_dir / name for name in names}
    available = set(patch_dir.glob("*.patch"))
    unlisted = sorted(path.name for path in available - listed)
    missing = sorted(path.name for path in listed - available)
    if unlisted:
        raise ValueError(f"unlisted patch in {patch_dir}: {', '.join(unlisted)}")
    if missing:
        raise ValueError(f"missing patch in {patch_dir}: {', '.join(missing)}")
    return [patch_dir / name for name in names]


def apply_patch_series(project_root: Path, patch_dir: Path) -> None:
    for patch in load_patch_series(patch_dir):
        subprocess.run(
            ["git", "apply", "--whitespace=nowarn", str(patch.resolve())],
            cwd=project_root,
            check=True,
        )


def parse_archive_root(archive: Path) -> str:
    with tarfile.open(archive, "r:gz") as tar:
        roots = {
            member.name.split("/", 1)[0]
            for member in tar.getmembers()
            if member.name and member.name != "."
        }
    if len(roots) != 1:
        raise ValueError(f"expected exactly one archive root in {archive}, found {sorted(roots)}")
    return next(iter(roots))


def git_head(repo: Path) -> str:
    return subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=repo, text=True).strip()


def require_clean_checkout(repo: Path) -> None:
    status = subprocess.check_output(
        ["git", "status", "--porcelain", "--untracked-files=all"],
        cwd=repo,
        text=True,
    ).strip()
    if status:
        raise ValueError(f"refusing to vendor from dirty checkout {repo}:\n{status}")


def ensure_dist_archive(source_repo: Path) -> Path:
    require_clean_checkout(source_repo)
    head = git_head(source_repo)[:9]
    subprocess.run(
        ["zig", "build", "dist", "-Demit-lib-vt", "-Doptimize=ReleaseFast"],
        cwd=source_repo,
        check=True,
    )
    require_clean_checkout(source_repo)
    dist_dir = source_repo / "zig-out" / "dist"
    archives = sorted(dist_dir.glob(f"libghostty-vt-*+{head}.tar.gz"))
    if not archives:
        raise FileNotFoundError(
            f"no libghostty-vt dist archive for HEAD {head} found in {dist_dir}"
        )
    return archives[-1]


def vendor_libghostty_vt(
    source_repo: Path,
    destination: Path,
    patch_dir: Path,
) -> VendorMetadata:
    archive = ensure_dist_archive(source_repo)
    root = parse_archive_root(archive)

    with tempfile.TemporaryDirectory() as temp_dir:
        temp_dir_path = Path(temp_dir)
        with tarfile.open(archive, "r:gz") as tar:
            tar.extractall(temp_dir_path)

        extracted = temp_dir_path / root
        if not extracted.exists():
            raise FileNotFoundError(f"expected extracted root {extracted}")

        staged_project = temp_dir_path / "staged-project"
        staged_vendor = staged_project / "vendor" / "libghostty-vt"
        staged_vendor.parent.mkdir(parents=True)
        shutil.copytree(extracted, staged_vendor)
        apply_patch_series(staged_project, patch_dir)

        destination.parent.mkdir(parents=True, exist_ok=True)
        replacement_parent = Path(
            tempfile.mkdtemp(prefix=f".{destination.name}.new-", dir=destination.parent)
        )
        backup_parent = Path(
            tempfile.mkdtemp(prefix=f".{destination.name}.old-", dir=destination.parent)
        )
        replacement = replacement_parent / destination.name
        backup = backup_parent / destination.name
        preserve_backup = False
        try:
            shutil.copytree(staged_vendor, replacement)
            if destination.exists():
                destination.rename(backup)
            try:
                replacement.rename(destination)
            except Exception as install_error:
                if backup.exists():
                    try:
                        backup.rename(destination)
                    except Exception as rollback_error:
                        preserve_backup = True
                        raise RuntimeError(
                            f"failed to install vendor tree ({install_error}); rollback failed; "
                            f"backup preserved at {backup}"
                        ) from rollback_error
                raise
            if backup.exists():
                try:
                    shutil.rmtree(backup)
                except OSError as cleanup_error:
                    preserve_backup = True
                    print(
                        f"warning: could not remove previous vendor tree at {backup}: "
                        f"{cleanup_error}",
                        file=sys.stderr,
                    )
        finally:
            shutil.rmtree(replacement_parent, ignore_errors=True)
            if not preserve_backup:
                shutil.rmtree(backup_parent, ignore_errors=True)

    return VendorMetadata(
        source_commit=git_head(source_repo),
        dist_archive=archive.name,
        extracted_dir=root,
    )


def main() -> None:
    parser = argparse.ArgumentParser(description="Vendor the pinned libghostty-vt source dist into herdr")
    parser.add_argument(
        "--source-repo",
        default="/home/can/Projects/ghostty",
        help="Path to a local ghostty checkout",
    )
    parser.add_argument(
        "--destination",
        default="vendor/libghostty-vt",
        help="Destination directory for the extracted libghostty-vt source dist",
    )
    parser.add_argument(
        "--metadata",
        default="vendor/libghostty-vt.vendor.json",
        help="Path to write vendoring metadata JSON",
    )
    parser.add_argument(
        "--patch-dir",
        default="vendor/patches/libghostty-vt",
        help="Directory containing the ordered local patch series",
    )
    args = parser.parse_args()

    repo = Path(args.source_repo).resolve()
    destination = Path(args.destination).resolve()
    metadata_path = Path(args.metadata).resolve()
    patch_dir = Path(args.patch_dir).resolve()

    metadata = vendor_libghostty_vt(repo, destination, patch_dir)
    metadata_path.parent.mkdir(parents=True, exist_ok=True)
    metadata_path.write_text(json.dumps(asdict(metadata), indent=2) + "\n")

    print(f"vendored {metadata.extracted_dir} from {metadata.source_commit} into {destination}")


if __name__ == "__main__":
    main()
