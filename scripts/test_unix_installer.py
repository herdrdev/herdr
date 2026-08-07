from __future__ import annotations

import hashlib
import json
import os
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[1]
INSTALLER = REPO_ROOT / "website" / "install.sh"
REQUIRED_COMMANDS = ("awk", "cat", "chmod", "cp", "mkdir", "mktemp", "mv", "rm")


class UnixInstallerTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temp_dir = tempfile.TemporaryDirectory(prefix="herdr-installer-test-")
        self.root = Path(self.temp_dir.name)
        self.bin_dir = self.root / "bin"
        self.bin_dir.mkdir()
        self.install_dir = self.root / "install"
        self.payload = self.root / "payload"
        self.payload.write_bytes(b"fake-herdr-binary\n")
        self.expected_sha256 = hashlib.sha256(self.payload.read_bytes()).hexdigest()

        for command in REQUIRED_COMMANDS:
            path = shutil.which(command)
            if path is None:
                self.fail(f"test host is missing required command: {command}")
            (self.bin_dir / command).symlink_to(path)

        self._write_executable(
            "uname",
            """#!/bin/sh
case "$1" in
  -s) echo Linux ;;
  -m) echo x86_64 ;;
  *) exit 1 ;;
esac
""",
        )
        self._write_executable(
            "curl",
            """#!/bin/sh
out=""
previous=""
for argument in "$@"; do
  if [ "$previous" = "-o" ]; then
    out="$argument"
    break
  fi
  previous="$argument"
done
if [ -n "$out" ]; then
  cp "$FAKE_PAYLOAD" "$out"
else
  cat "$FAKE_MANIFEST"
fi
""",
        )

    def tearDown(self) -> None:
        self.temp_dir.cleanup()

    def _write_executable(self, name: str, content: str) -> None:
        path = self.bin_dir / name
        path.write_text(content, encoding="utf-8")
        path.chmod(0o755)

    def _select_checksum_tool(self, tool: str) -> None:
        if tool == "sha256sum":
            path = shutil.which("sha256sum")
            if path is None:
                self.fail("test host is missing sha256sum")
            (self.bin_dir / "sha256sum").symlink_to(path)
            return

        if tool == "shasum":
            sha256sum = shutil.which("sha256sum")
            if sha256sum is None:
                self.fail("test host is missing sha256sum for the shasum fixture")
            self._write_executable(
                "shasum",
                f"""#!/bin/sh
[ "$1" = "-a" ] && [ "$2" = "256" ] || exit 2
shift 2
exec {sha256sum} "$@"
""",
            )
            return

        if tool == "openssl":
            path = shutil.which("openssl")
            if path is None:
                self.fail("test host is missing openssl")
            (self.bin_dir / "openssl").symlink_to(path)
            return

        self.fail(f"unknown checksum tool fixture: {tool}")

    def _write_manifest(self, checksum: str | None) -> Path:
        manifest: dict[str, object] = {
            "version": "9.9.9",
            "assets": {
                "linux-x86_64": "https://example.invalid/herdr-linux-x86_64"
            },
        }
        if checksum is not None:
            manifest["sha256"] = {"linux-x86_64": checksum}
        path = self.root / "latest.json"
        path.write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")
        return path

    def _run_installer(self, checksum: str | None, tool: str = "sha256sum") -> subprocess.CompletedProcess[str]:
        self._select_checksum_tool(tool)
        manifest = self._write_manifest(checksum)
        env = {
            **os.environ,
            "PATH": str(self.bin_dir),
            "FAKE_MANIFEST": str(manifest),
            "FAKE_PAYLOAD": str(self.payload),
            "HERDR_INSTALL_DIR": str(self.install_dir),
        }
        return subprocess.run(
            ["/bin/sh", str(INSTALLER)],
            env=env,
            capture_output=True,
            text=True,
            check=False,
        )

    def test_valid_download_uses_each_supported_checksum_tool(self) -> None:
        for tool in ("sha256sum", "shasum", "openssl"):
            with self.subTest(tool=tool):
                shutil.rmtree(self.install_dir, ignore_errors=True)
                for name in ("sha256sum", "shasum", "openssl"):
                    (self.bin_dir / name).unlink(missing_ok=True)

                result = self._run_installer(self.expected_sha256.upper(), tool)

                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual((self.install_dir / "herdr").read_bytes(), self.payload.read_bytes())

    def test_checksum_mismatch_does_not_replace_existing_binary(self) -> None:
        self.install_dir.mkdir()
        installed = self.install_dir / "herdr"
        installed.write_bytes(b"existing-herdr\n")

        result = self._run_installer("0" * 64)

        self.assertNotEqual(result.returncode, 0)
        self.assertIn("checksum did not match", result.stderr)
        self.assertEqual(installed.read_bytes(), b"existing-herdr\n")

    def test_missing_checksum_fails_without_replacing_existing_binary(self) -> None:
        self.install_dir.mkdir()
        installed = self.install_dir / "herdr"
        installed.write_bytes(b"existing-herdr\n")

        result = self._run_installer(None)

        self.assertNotEqual(result.returncode, 0)
        self.assertIn("valid SHA-256 checksum", result.stderr)
        self.assertEqual(installed.read_bytes(), b"existing-herdr\n")


if __name__ == "__main__":
    unittest.main()
