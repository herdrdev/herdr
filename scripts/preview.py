#!/usr/bin/env python3
from __future__ import annotations

import argparse
import json
import re
import subprocess
from pathlib import Path
from typing import Any

ASSET_TARGETS = (
    "linux-x86_64",
    "linux-aarch64",
    "macos-x86_64",
    "macos-aarch64",
    "windows-x86_64",
)
EXPECTED_ASSET_NAMES = {
    **{target: f"herdr-{target}" for target in ASSET_TARGETS},
    "windows-x86_64": "herdr-windows-x86_64.zip",
}
ENDPOINT_PROTOCOL_SOURCE_PATH = Path("src/protocol/endpoint.rs")


def run_git(args: list[str]) -> str:
    return subprocess.check_output(["git", *args], text=True).strip()


def normalize_version(version: str) -> str:
    return version.strip().removeprefix("v")


def read_endpoint_protocol_generation(
    source_path: Path = ENDPOINT_PROTOCOL_SOURCE_PATH,
) -> int:
    content = source_path.read_text(encoding="utf-8")
    match = re.search(r"pub const ENDPOINT_PROTOCOL_GENERATION: u32 = (\d+);", content)
    if not match:
        raise ValueError(
            f"could not read ENDPOINT_PROTOCOL_GENERATION from {source_path}"
        )
    return int(match.group(1))


def latest_stable_tag(ref: str | None = None) -> str:
    args = ["describe", "--tags", "--match", "v[0-9]*", "--abbrev=0"]
    if ref:
        args.append(ref)
    return run_git(args)


def git_is_ancestor(ancestor: str, descendant: str) -> bool:
    result = subprocess.run(
        ["git", "merge-base", "--is-ancestor", ancestor, descendant],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        check=False,
    )
    return result.returncode == 0


def read_json(path: Path) -> dict[str, Any] | None:
    if not path.exists():
        return None
    return json.loads(path.read_text(encoding="utf-8"))


def previous_preview_commit(path: Path) -> str | None:
    data = read_json(path)
    if not data:
        return None
    commit = data.get("commit")
    return commit if isinstance(commit, str) and commit.strip() else None


def preview_range_base(previous: str, commit: str) -> str:
    try:
        stable = latest_stable_tag(commit)
    except subprocess.CalledProcessError:
        return previous
    if not git_is_ancestor(previous, commit):
        return stable
    if git_is_ancestor(previous, stable) and git_is_ancestor(stable, commit):
        return stable
    return previous


def build_notes(previous: str, commit: str, build_id: str, repo: str) -> str:
    compare = f"https://github.com/{repo}/compare/{previous}...{commit}"
    return f"Preview build {build_id}\n\n[View changes]({compare})\n"


def default_asset_urls(repo: str, tag: str) -> dict[str, str]:
    return {
        target: f"https://github.com/{repo}/releases/download/{tag}/{EXPECTED_ASSET_NAMES[target]}"
        for target in ASSET_TARGETS
    }


def read_sha_file(path: Path | None) -> dict[str, str]:
    if path is None:
        return {}
    data = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(data, dict):
        raise SystemExit("sha file must be a JSON object")
    return {str(key): str(value) for key, value in data.items()}


def asset_objects(urls: dict[str, str], shas: dict[str, str]) -> dict[str, dict[str, str]]:
    assets: dict[str, dict[str, str]] = {}
    for target in ASSET_TARGETS:
        url = urls[target]
        entry = {"url": url}
        sha = shas.get(target)
        if sha:
            entry["sha256"] = sha
        if target.startswith("windows-"):
            if not sha or not re.fullmatch(r"[0-9a-fA-F]{64}", sha):
                raise ValueError(f"{target} requires a SHA-256 digest")
            entry["format"] = "zip"
        assets[target] = entry
    return assets


def build_manifest(
    output: Path,
    repo: str,
    tag: str,
    build_id: str,
    commit: str,
    built_at: str,
    base_version: str,
    protocol: int,
    notes: str,
    shas: dict[str, str],
    retain: int,
    endpoint_generation: int | None = None,
) -> str:
    urls = default_asset_urls(repo, tag)
    assets = asset_objects(urls, shas)
    current = read_json(output) or {}
    builds = current.get("builds") if isinstance(current.get("builds"), dict) else {}
    builds = dict(builds)
    if endpoint_generation is None:
        endpoint_generation = read_endpoint_protocol_generation()
    builds[build_id] = {
        "base_version": normalize_version(base_version),
        "commit": commit,
        "built_at": built_at,
        "protocol": protocol,
        "endpoint_generation": endpoint_generation,
        "tag": tag,
        "assets": assets,
    }
    ordered_builds = {
        key: builds[key]
        for key in sorted(
            builds,
            key=lambda key: str(builds[key].get("built_at", "")),
            reverse=True,
        )[:retain]
    }
    manifest = {
        "schema_version": 1,
        "channel": "preview",
        "base_version": normalize_version(base_version),
        "build_id": build_id,
        "commit": commit,
        "built_at": built_at,
        "protocol": protocol,
        "endpoint_generation": endpoint_generation,
        "notes": notes.strip(),
        "assets": assets,
        "builds": ordered_builds,
    }
    return json.dumps(manifest, indent=2) + "\n"


def cmd_notes(args: argparse.Namespace) -> int:
    previous = args.previous or previous_preview_commit(Path(args.manifest)) or latest_stable_tag()
    notes = build_notes(previous, args.commit, args.build_id, args.repo)
    Path(args.output).write_text(notes, encoding="utf-8")
    return 0


def cmd_manifest(args: argparse.Namespace) -> int:
    notes = Path(args.notes).read_text(encoding="utf-8")
    shas = read_sha_file(Path(args.sha_file) if args.sha_file else None)
    content = build_manifest(
        output=Path(args.output),
        repo=args.repo,
        tag=args.tag,
        build_id=args.build_id,
        commit=args.commit,
        built_at=args.built_at,
        base_version=args.base_version,
        protocol=args.protocol,
        notes=notes,
        shas=shas,
        retain=args.retain,
        endpoint_generation=args.endpoint_generation,
    )
    Path(args.output).write_text(content, encoding="utf-8")
    return 0


def cmd_current_commit(args: argparse.Namespace) -> int:
    commit = previous_preview_commit(Path(args.manifest))
    if commit:
        print(commit)
    return 0


def cmd_range_base(args: argparse.Namespace) -> int:
    print(preview_range_base(args.previous, args.commit))
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description="Preview channel release helpers")
    sub = parser.add_subparsers(required=True)

    notes = sub.add_parser("notes")
    notes.add_argument("--manifest", default="distribution/preview.json")
    notes.add_argument("--previous")
    notes.add_argument("--commit", required=True)
    notes.add_argument("--build-id", required=True)
    notes.add_argument("--repo", default="herdrdev/herdr")
    notes.add_argument("--output", required=True)
    notes.set_defaults(func=cmd_notes)

    manifest = sub.add_parser("manifest")
    manifest.add_argument("--output", default="distribution/preview.json")
    manifest.add_argument("--repo", default="herdrdev/herdr")
    manifest.add_argument("--tag", required=True)
    manifest.add_argument("--build-id", required=True)
    manifest.add_argument("--commit", required=True)
    manifest.add_argument("--built-at", required=True)
    manifest.add_argument("--base-version", required=True)
    manifest.add_argument("--protocol", required=True, type=int)
    manifest.add_argument("--endpoint-generation", required=True, type=int)
    manifest.add_argument("--notes", required=True)
    manifest.add_argument("--sha-file")
    manifest.add_argument("--retain", type=int, default=30)
    manifest.set_defaults(func=cmd_manifest)

    current = sub.add_parser("current-commit")
    current.add_argument("--manifest", default="distribution/preview.json")
    current.set_defaults(func=cmd_current_commit)

    range_base = sub.add_parser("range-base")
    range_base.add_argument("--previous", required=True)
    range_base.add_argument("--commit", required=True)
    range_base.set_defaults(func=cmd_range_base)

    args = parser.parse_args()
    return args.func(args)


if __name__ == "__main__":
    raise SystemExit(main())
