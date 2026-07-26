#!/usr/bin/env python3
from __future__ import annotations

import argparse
import hashlib
import json
import shutil
import struct
import tempfile
import urllib.request
import xml.etree.ElementTree as ET
import zipfile
from pathlib import Path, PurePosixPath
from typing import Any

PROJECT_ROOT = Path(__file__).resolve().parent.parent
DEFAULT_METADATA = PROJECT_ROOT / "packaging" / "windows" / "conpty.json"
MARKER_PATH = PurePosixPath("conpty/herdr-conpty.json")
DOWNLOAD_TIMEOUT_SECONDS = 60


def sha256_bytes(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def load_metadata(path: Path) -> dict[str, Any]:
    data = json.loads(path.read_text(encoding="utf-8"))
    if data.get("schema_version") != 1:
        raise ValueError("unsupported ConPTY metadata schema")
    return data


def pe_machine(data: bytes) -> int:
    if len(data) < 0x40 or data[:2] != b"MZ":
        raise ValueError("file is not a PE image")
    pe_offset = struct.unpack_from("<I", data, 0x3C)[0]
    if pe_offset + 6 > len(data) or data[pe_offset : pe_offset + 4] != b"PE\0\0":
        raise ValueError("file has an invalid PE header")
    return struct.unpack_from("<H", data, pe_offset + 4)[0]


def validate_nuspec(archive: zipfile.ZipFile, package: dict[str, Any]) -> None:
    nuspec_name = f"{package['id']}.nuspec"
    try:
        root = ET.fromstring(archive.read(nuspec_name).decode("utf-8-sig"))
    except KeyError as error:
        raise ValueError(f"package is missing {nuspec_name}") from error
    namespace = {"n": "http://schemas.microsoft.com/packaging/2011/08/nuspec.xsd"}
    metadata = root.find("n:metadata", namespace)
    if metadata is None:
        raise ValueError("package NuSpec is missing metadata")
    expected = {
        "id": package["id"],
        "version": package["version"],
        "license": package["license"],
    }
    for field, expected_value in expected.items():
        element = metadata.find(f"n:{field}", namespace)
        actual = element.text.strip() if element is not None and element.text else None
        if actual != expected_value:
            raise ValueError(
                f"package NuSpec {field} mismatch: expected {expected_value!r}, got {actual!r}"
            )


def acquire_package(package: dict[str, Any], package_path: Path) -> None:
    package_path.parent.mkdir(parents=True, exist_ok=True)
    if not package_path.exists():
        with urllib.request.urlopen(
            package["url"], timeout=DOWNLOAD_TIMEOUT_SECONDS
        ) as response, package_path.open("wb") as output:
            shutil.copyfileobj(response, output)
    actual = sha256_file(package_path)
    if actual != package["sha256"]:
        raise ValueError(
            f"ConPTY package hash mismatch: expected {package['sha256']}, got {actual}"
        )


def marker_data(metadata: dict[str, Any], architecture: str) -> bytes:
    bundle = metadata["bundles"][architecture]
    marker = {
        "schema_version": 1,
        "package": metadata["package"]["id"],
        "version": metadata["package"]["version"],
        "architecture": architecture,
        "files": {
            item["destination"]: item["sha256"] for item in bundle["files"]
        },
    }
    return (json.dumps(marker, indent=2, sort_keys=True) + "\n").encode()


def stage_bundle(
    metadata_path: Path,
    architecture: str,
    package_path: Path,
    herdr_exe: Path,
    output_dir: Path,
) -> None:
    metadata = load_metadata(metadata_path)
    if architecture not in metadata["bundles"]:
        raise ValueError(f"unsupported Windows architecture: {architecture}")
    if output_dir.exists():
        raise ValueError(f"output directory already exists: {output_dir}")
    if not herdr_exe.is_file():
        raise ValueError(f"Herdr executable does not exist: {herdr_exe}")

    acquire_package(metadata["package"], package_path)
    bundle = metadata["bundles"][architecture]
    metadata_root = metadata_path.resolve().parent

    with zipfile.ZipFile(package_path) as archive, tempfile.TemporaryDirectory(
        prefix="herdr-conpty-stage-", dir=output_dir.parent
    ) as temporary:
        validate_nuspec(archive, metadata["package"])
        staging = Path(temporary) / "bundle"
        staging.mkdir()
        shutil.copy2(herdr_exe, staging / "herdr.exe")

        for item in bundle["files"]:
            try:
                payload = archive.read(item["source"])
            except KeyError as error:
                raise ValueError(f"package is missing {item['source']}") from error
            actual_hash = sha256_bytes(payload)
            if actual_hash != item["sha256"]:
                raise ValueError(
                    f"hash mismatch for {item['source']}: expected {item['sha256']}, got {actual_hash}"
                )
            actual_machine = pe_machine(payload)
            expected_machine = int(item["pe_machine"], 16)
            if actual_machine != expected_machine:
                raise ValueError(
                    f"PE machine mismatch for {item['source']}: "
                    f"expected {item['pe_machine']}, got 0x{actual_machine:04x}"
                )
            destination = staging / PurePosixPath(item["destination"])
            destination.parent.mkdir(parents=True, exist_ok=True)
            destination.write_bytes(payload)

        marker = staging / MARKER_PATH
        marker.parent.mkdir(parents=True, exist_ok=True)
        marker.write_bytes(marker_data(metadata, architecture))

        for notice in metadata["notices"]:
            source = (metadata_root.parent.parent / notice["source"]).resolve()
            actual_hash = sha256_file(source)
            if actual_hash != notice["sha256"]:
                raise ValueError(
                    f"notice hash mismatch for {source}: expected {notice['sha256']}, got {actual_hash}"
                )
            destination = staging / PurePosixPath(notice["destination"])
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(source, destination)

        staging.rename(output_dir)


def expected_stage_files(metadata: dict[str, Any], architecture: str) -> set[str]:
    files = {"herdr.exe", MARKER_PATH.as_posix()}
    files.update(item["destination"] for item in metadata["bundles"][architecture]["files"])
    files.update(item["destination"] for item in metadata["notices"])
    return files


def validate_stage(metadata_path: Path, architecture: str, stage_dir: Path) -> None:
    metadata = load_metadata(metadata_path)
    actual = {
        path.relative_to(stage_dir).as_posix()
        for path in stage_dir.rglob("*")
        if path.is_file()
    }
    expected = expected_stage_files(metadata, architecture)
    if actual != expected:
        raise ValueError(
            f"bundle layout mismatch; missing={sorted(expected - actual)}, unexpected={sorted(actual - expected)}"
        )
    if (stage_dir / MARKER_PATH).read_bytes() != marker_data(metadata, architecture):
        raise ValueError("bundle marker does not match pinned ConPTY metadata")
    for item in metadata["bundles"][architecture]["files"]:
        path = stage_dir / PurePosixPath(item["destination"])
        actual_hash = sha256_file(path)
        if actual_hash != item["sha256"]:
            raise ValueError(f"staged file hash mismatch for {path}")
    for notice in metadata["notices"]:
        path = stage_dir / PurePosixPath(notice["destination"])
        actual_hash = sha256_file(path)
        if actual_hash != notice["sha256"]:
            raise ValueError(f"staged notice hash mismatch for {path}")


def archive_bundle(
    metadata_path: Path, architecture: str, stage_dir: Path, output_path: Path
) -> None:
    validate_stage(metadata_path, architecture, stage_dir)
    output_path.parent.mkdir(parents=True, exist_ok=True)
    with zipfile.ZipFile(output_path, "w", compression=zipfile.ZIP_DEFLATED, compresslevel=9) as archive:
        for path in sorted(stage_dir.rglob("*")):
            if path.is_file():
                archive.write(path, path.relative_to(stage_dir).as_posix())


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Package Herdr with Microsoft's pinned ConPTY runtime")
    parser.add_argument("--metadata", type=Path, default=DEFAULT_METADATA)
    subparsers = parser.add_subparsers(dest="command", required=True)

    stage = subparsers.add_parser("stage")
    stage.add_argument("--architecture", choices=("x86_64",), default="x86_64")
    stage.add_argument("--package", type=Path, required=True)
    stage.add_argument("--herdr-exe", type=Path, required=True)
    stage.add_argument("--output-dir", type=Path, required=True)

    archive = subparsers.add_parser("archive")
    archive.add_argument("--architecture", choices=("x86_64",), default="x86_64")
    archive.add_argument("--stage-dir", type=Path, required=True)
    archive.add_argument("--output", type=Path, required=True)
    return parser.parse_args()


def main() -> None:
    args = parse_args()
    if args.command == "stage":
        stage_bundle(
            args.metadata,
            args.architecture,
            args.package,
            args.herdr_exe,
            args.output_dir,
        )
    else:
        archive_bundle(args.metadata, args.architecture, args.stage_dir, args.output)


if __name__ == "__main__":
    main()
