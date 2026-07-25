from __future__ import annotations

import hashlib
import io
import json
import struct
import tempfile
import unittest
import zipfile
from pathlib import Path
from unittest import mock

from scripts import package_windows_conpty as package


class WindowsConptyPackageTests(unittest.TestCase):
    def test_pinned_metadata_and_notices_are_consistent(self) -> None:
        metadata = package.load_metadata(package.DEFAULT_METADATA)
        self.assertEqual(metadata["package"]["id"], "Microsoft.Windows.Console.ConPTY")
        self.assertEqual(metadata["package"]["version"], "1.24.260710001")
        self.assertEqual(
            {item["destination"] for item in metadata["bundles"]["x86_64"]["files"]},
            {
                "conpty/conpty.dll",
                "conpty/x64/OpenConsole.exe",
                "conpty/arm64/OpenConsole.exe",
            },
        )
        loader = (
            package.PROJECT_ROOT / "vendor/portable-pty/src/win/psuedocon.rs"
        ).read_text(encoding="utf-8")
        installer = (package.PROJECT_ROOT / "website/install.ps1").read_text(
            encoding="utf-8"
        )
        for item in metadata["bundles"]["x86_64"]["files"]:
            self.assertIn(item["sha256"], loader)
            self.assertNotIn(item["sha256"], installer)
        self.assertIn('Get-Content -LiteralPath $markerPath -Raw', installer)
        self.assertIn('$filesProperty.Value.PSObject.Properties[$relative]', installer)
        for notice in metadata["notices"]:
            source = package.PROJECT_ROOT / notice["source"]
            self.assertEqual(package.sha256_file(source), notice["sha256"])

    def test_powershell_wrapper_verifies_package_and_signatures(self) -> None:
        wrapper = (package.PROJECT_ROOT / "scripts/package_windows_conpty.ps1").read_text(
            encoding="utf-8"
        )
        self.assertIn('"nuget", "verify", "--all"', wrapper)
        self.assertIn("Get-AuthenticodeSignature", wrapper)
        self.assertIn('conpty\\arm64\\OpenConsole.exe', wrapper)
        self.assertIn('conpty\\x64\\OpenConsole.exe', wrapper)
        self.assertIn('conpty\\conpty.dll', wrapper)
        self.assertIn('"*Microsoft Corporation*"', wrapper)

    def test_package_download_has_a_finite_timeout(self) -> None:
        payload = b"package"
        with tempfile.TemporaryDirectory() as temporary:
            destination = Path(temporary) / "conpty.nupkg"
            metadata = {
                "url": "https://example.invalid/conpty.nupkg",
                "sha256": hashlib.sha256(payload).hexdigest(),
            }
            with mock.patch.object(
                package.urllib.request, "urlopen", return_value=io.BytesIO(payload)
            ) as urlopen:
                package.acquire_package(metadata, destination)

            urlopen.assert_called_once_with(
                metadata["url"], timeout=package.DOWNLOAD_TIMEOUT_SECONDS
            )

    def test_stage_and_archive_validate_exact_package(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            dll = self._pe(0x8664, b"dll")
            x64_host = self._pe(0x8664, b"x64")
            arm64_host = self._pe(0xAA64, b"arm64")
            files = [
                self._file("runtimes/win-x64/native/conpty.dll", "conpty/conpty.dll", dll, "0x8664"),
                self._file(
                    "build/native/runtimes/x64/OpenConsole.exe",
                    "conpty/x64/OpenConsole.exe",
                    x64_host,
                    "0x8664",
                ),
                self._file(
                    "build/native/runtimes/arm64/OpenConsole.exe",
                    "conpty/arm64/OpenConsole.exe",
                    arm64_host,
                    "0xaa64",
                ),
            ]
            nupkg = root / "conpty.nupkg"
            self._write_package(nupkg, files, {item["source"]: data for item, data in zip(files, (dll, x64_host, arm64_host))})
            metadata_path = root / "conpty.json"
            metadata_path.write_text(
                json.dumps(
                    {
                        "schema_version": 1,
                        "package": {
                            "id": "Microsoft.Windows.Console.ConPTY",
                            "version": "1.24.260710001",
                            "url": nupkg.as_uri(),
                            "sha256": package.sha256_file(nupkg),
                            "license": "MIT",
                        },
                        "bundles": {"x86_64": {"files": files}},
                        "notices": [],
                    }
                ),
                encoding="utf-8",
            )
            herdr = root / "input-herdr.exe"
            herdr.write_bytes(b"herdr")
            stage = root / "stage"
            package.stage_bundle(metadata_path, "x86_64", nupkg, herdr, stage)
            package.validate_stage(metadata_path, "x86_64", stage)

            output = root / "herdr.zip"
            package.archive_bundle(metadata_path, "x86_64", stage, output)
            with zipfile.ZipFile(output) as archive:
                self.assertEqual(
                    set(archive.namelist()),
                    package.expected_stage_files(
                        package.load_metadata(metadata_path), "x86_64"
                    ),
                )

            (stage / "conpty" / "conpty.dll").write_bytes(b"tampered")
            with self.assertRaisesRegex(ValueError, "staged file hash mismatch"):
                package.validate_stage(metadata_path, "x86_64", stage)

    @staticmethod
    def _pe(machine: int, payload: bytes) -> bytes:
        data = bytearray(0x80)
        data[:2] = b"MZ"
        struct.pack_into("<I", data, 0x3C, 0x40)
        data[0x40:0x44] = b"PE\0\0"
        struct.pack_into("<H", data, 0x44, machine)
        return bytes(data) + payload

    @staticmethod
    def _file(source: str, destination: str, data: bytes, machine: str) -> dict[str, str]:
        return {
            "source": source,
            "destination": destination,
            "sha256": hashlib.sha256(data).hexdigest(),
            "pe_machine": machine,
        }

    @staticmethod
    def _write_package(
        path: Path, files: list[dict[str, str]], payloads: dict[str, bytes]
    ) -> None:
        nuspec = """<?xml version="1.0" encoding="utf-8"?>
<package xmlns="http://schemas.microsoft.com/packaging/2011/08/nuspec.xsd">
  <metadata>
    <id>Microsoft.Windows.Console.ConPTY</id>
    <version>1.24.260710001</version>
    <license type="expression">MIT</license>
  </metadata>
</package>
"""
        with zipfile.ZipFile(path, "w") as archive:
            archive.writestr("Microsoft.Windows.Console.ConPTY.nuspec", nuspec)
            for item in files:
                archive.writestr(item["source"], payloads[item["source"]])


if __name__ == "__main__":
    unittest.main()
