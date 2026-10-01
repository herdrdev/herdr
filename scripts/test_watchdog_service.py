from __future__ import annotations

import os
import plistlib
import subprocess
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "scripts" / "watchdog_service.sh"


class WatchdogServiceTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory(prefix="herdr-watchdog-service-")
        self.root = Path(self.temp.name)
        self.home = self.root / "home"
        self.home.mkdir()
        self.binary = self.root / "bin" / "herdr"
        self.binary.parent.mkdir()
        self.binary.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
        self.binary.chmod(0o755)

    def tearDown(self) -> None:
        self.temp.cleanup()

    def render(self, os_name: str, *options: str) -> str:
        env = os.environ.copy()
        env.update(HOME=str(self.home), WATCHDOG_SERVICE_OS=os_name)
        result = subprocess.run(
            ["bash", str(SCRIPT), "install", "--herdr-bin", str(self.binary), "--print", *options],
            env=env,
            text=True,
            capture_output=True,
            check=False,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        return result.stdout

    def test_linux_renderer_defaults_and_options(self) -> None:
        default = self.render("linux")
        self.assertIn("--no-model", default)
        self.assertNotIn("--dry-run", default)
        self.assertIn("Restart=on-failure", default)
        self.assertIn("RestartSec=10", default)
        self.assertIn("StartLimitIntervalSec=600", default)
        self.assertIn("StartLimitBurst=5", default)
        # systemd rejects a quoted append: target as a relative path.
        self.assertRegex(default, r"(?m)^StandardOutput=append:/\S+/watchdog\.log$")
        self.assertRegex(default, r"(?m)^StandardError=append:/\S+/watchdog\.log$")
        self.assertIn("watchdog.log", default)
        self.assertIn("watchdog-status.log", default)
        selected = self.render("linux", "--with-model", "--dry-run")
        self.assertNotIn("--no-model", selected)
        self.assertIn("--dry-run", selected)

    def test_launchd_renderer_defaults_and_options(self) -> None:
        default = self.render("darwin")
        plist = plistlib.loads(default.encode("utf-8"))
        self.assertIn("--no-model", default)
        self.assertNotIn("<string>--dry-run</string>", default)
        self.assertIn("<key>SuccessfulExit</key><false/>", default)
        self.assertIn("<key>RunAtLoad</key><true/>", default)
        self.assertIn("<key>ThrottleInterval</key><integer>10</integer>", default)
        self.assertIn("herdr-watchdog.log", default)
        self.assertIn("watchdog-status.log", default)
        self.assertEqual(plist["KeepAlive"], {"SuccessfulExit": False})
        self.assertEqual(plist["StandardOutPath"], str(self.home / "Library/Logs/herdr-watchdog.log"))
        selected = self.render("darwin", "--with-model", "--dry-run")
        self.assertNotIn("--no-model", selected)
        self.assertIn("<string>--dry-run</string>", selected)

    def test_missing_binary_fails(self) -> None:
        env = os.environ.copy()
        env.update(HOME=str(self.home), WATCHDOG_SERVICE_OS="linux")
        result = subprocess.run(
            ["bash", str(SCRIPT), "install", "--print", "--herdr-bin", str(self.root / "missing")],
            env=env,
            text=True,
            capture_output=True,
            check=False,
        )
        self.assertNotEqual(result.returncode, 0)


if __name__ == "__main__":
    unittest.main()
