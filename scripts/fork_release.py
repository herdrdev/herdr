#!/usr/bin/env python3
"""Validate fork release versions and destinations without changing repository state."""

import argparse
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tomllib

REPOSITORY = "trungnt13/herdr"
BASE_PATTERN = r"(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)"
VERSION_PATTERN = re.compile(rf"({BASE_PATTERN})\+fork\.([1-9][0-9]*)")


def validate_version(version: str, cargo_version: str, exact: bool = False) -> None:
    match = VERSION_PATTERN.fullmatch(version)
    cargo_match = VERSION_PATTERN.fullmatch(cargo_version)
    base = cargo_match[1] if cargo_match else cargo_version
    if not match or not re.fullmatch(BASE_PATTERN, base) or match[1] != base:
        raise ValueError("version must be MAJOR.MINOR.PATCH+fork.N (N positive, no leading zeros), using the Cargo base version")
    if not exact and cargo_match and int(match[2]) <= int(cargo_match[2]):
        raise ValueError("fork revision must exceed the current Cargo fork revision")
    if exact and version != cargo_version:
        raise ValueError("release tag must match the Cargo version exactly")


def validate_remote(url: str) -> None:
    if not re.fullmatch(r"(?:https://github\.com/|git@github\.com:|ssh://git@github\.com/)trungnt13/herdr(?:\.git)?", url):
        raise ValueError(f"origin must target github.com/{REPOSITORY}, got {url!r}")


def read_command(*args: str) -> str:
    return subprocess.check_output(args, text=True, timeout=120).strip()


def validate_destination() -> None:
    for args in (("git", "remote", "get-url", "--all", "origin"),
                 ("git", "remote", "get-url", "--push", "--all", "origin")):
        urls = read_command(*args).splitlines()
        if not urls:
            raise ValueError("origin has no URL")
        for url in urls:
            validate_remote(url)
    if read_command("gh", "api", "--hostname", "github.com", "user", "--jq", ".login") != "trungnt13":
        raise ValueError("authenticated GitHub account must be trungnt13")
    repository = json.loads(read_command("gh", "api", "--hostname", "github.com", f"repos/{REPOSITORY}"))
    if repository.get("full_name") != REPOSITORY or repository.get("permissions", {}).get("push") is not True:
        raise ValueError("GitHub API must confirm fork identity and write permission")


def validate_reservations(version: str) -> None:
    tag = f"v{version}"
    tags = read_command("git", "ls-remote", "--tags", "origin")
    release_tags = read_command("gh", "api", "--hostname", "github.com", "--paginate", "--jq", ".[].tag_name", f"repos/{REPOSITORY}/releases?per_page=100").splitlines()
    reserved_tags = [line.split()[1].removeprefix("refs/tags/") for line in tags.splitlines()]
    reserved_tags.extend(release_tags)
    requested = VERSION_PATTERN.fullmatch(version)
    for reserved in reserved_tags:
        if reserved == tag:
            raise ValueError(f"remote tag, release or draft already reserves {tag}")
        match = VERSION_PATTERN.fullmatch(reserved.removeprefix("v"))
        if match and match[1] == requested[1] and int(match[2]) >= int(requested[2]):
            raise ValueError(f"fork revision must exceed reserved {reserved}")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("mode", choices=("prepare", "publish", "ci"))
    parser.add_argument("version", nargs="?")
    args = parser.parse_args()
    cargo_version = tomllib.loads(Path("Cargo.toml").read_text())["package"]["version"]
    version = args.version
    if args.mode == "ci":
        if os.environ.get("GITHUB_REPOSITORY") != REPOSITORY or os.environ.get("GITHUB_REF_TYPE") != "tag":
            raise ValueError("release CI requires a fork repository tag")
        tag = os.environ.get("GITHUB_REF_NAME", "")
        if not tag.startswith("v"):
            raise ValueError("release tag must start with v")
        version = tag[1:]
    if version is None:
        raise ValueError("release version is required")
    validate_version(version, cargo_version, exact=args.mode != "prepare")
    if args.mode != "ci":
        validate_destination()
        validate_reservations(version)


if __name__ == "__main__":
    try:
        main()
    except (ValueError, subprocess.SubprocessError, OSError) as error:
        sys.exit(f"error: {error}")
