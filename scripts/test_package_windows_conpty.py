from __future__ import annotations

import hashlib
import io
import json
import struct
import tempfile
import unittest
import urllib.error
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
        installer = (package.PROJECT_ROOT / "distribution/install.ps1").read_text(
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

    def test_package_download_retries_server_errors_with_a_finite_timeout(self) -> None:
        payload = b"package"
        with tempfile.TemporaryDirectory() as temporary:
            destination = Path(temporary) / "conpty.nupkg"
            metadata = {
                "url": "https://example.invalid/conpty.nupkg",
                "sha256": hashlib.sha256(payload).hexdigest(),
            }
            server_error = urllib.error.HTTPError(
                metadata["url"], 504, "Gateway Time-out", {}, None
            )
            with (
                mock.patch.object(
                    package.urllib.request,
                    "urlopen",
                    side_effect=[server_error, io.BytesIO(payload)],
                ) as urlopen,
                mock.patch.object(package.time, "sleep") as sleep,
            ):
                package.acquire_package(metadata, destination)

            self.assertEqual(
                urlopen.call_args_list,
                [
                    mock.call(
                        metadata["url"], timeout=package.DOWNLOAD_TIMEOUT_SECONDS
                    ),
                    mock.call(
                        metadata["url"], timeout=package.DOWNLOAD_TIMEOUT_SECONDS
                    ),
                ],
            )
            sleep.assert_called_once_with(1)

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
            herdr.write_bytes(self._pe_with_imports(0x8664, ["KERNEL32.dll"]))
            stage = root / "stage"
            package.stage_bundle(metadata_path, "x86_64", nupkg, herdr, stage)
            package.validate_stage(metadata_path, "x86_64", stage)

            (stage / "herdr.exe").write_bytes(
                self._pe_with_imports(0x8664, ["MSVCP140D.dll"])
            )
            with self.assertRaisesRegex(ValueError, "dynamic Microsoft"):
                package.validate_stage(metadata_path, "x86_64", stage)
            (stage / "herdr.exe").write_bytes(
                self._pe_with_imports(0x8664, ["KERNEL32.dll"])
            )

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

    def test_pe_imported_dlls_reads_import_and_delay_import_tables(self) -> None:
        data = self._pe_with_imports(
            0x8664,
            ["KERNEL32.dll", "api-ms-win-core-synch-l1-2-0.dll"],
            ["USER32.dll"],
        )
        self.assertEqual(
            set(package.pe_imported_dlls(data)),
            {"KERNEL32.dll", "api-ms-win-core-synch-l1-2-0.dll", "USER32.dll"},
        )
        pe32 = self._pe_with_imports(0x8664, ["VCRUNTIME140.dll"], magic=0x10B)
        self.assertEqual(package.pe_imported_dlls(pe32), ["VCRUNTIME140.dll"])

    def test_pe_imported_dlls_handles_va_based_delay_imports(self) -> None:
        # VA-based delay-import descriptors predate the RVA flag and are only
        # representable with a 32-bit image base.
        data = self._pe_with_imports(
            0x8664,
            ["KERNEL32.dll"],
            ["MSVCP140.dll"],
            magic=0x10B,
            delay_imports_use_va=True,
        )
        self.assertEqual(
            set(package.pe_imported_dlls(data)),
            {"KERNEL32.dll", "MSVCP140.dll"},
        )
        with self.assertRaisesRegex(ValueError, "dynamic Microsoft"):
            package.validate_static_msvc_runtime(data, "herdr.exe")

    def test_pe_imported_dlls_respects_declared_directory_size(self) -> None:
        # The second descriptor is outside the declared import-directory size,
        # so it must not be treated as an import even though it names a
        # dynamic runtime.
        outside = self._pe_with_imports(
            0x8664,
            ["KERNEL32.dll", "VCRUNTIME140.dll"],
            import_directory_size=20,
        )
        self.assertEqual(package.pe_imported_dlls(outside), ["KERNEL32.dll"])
        package.validate_static_msvc_runtime(outside, "herdr.exe")

        inside = self._pe_with_imports(0x8664, ["KERNEL32.dll", "VCRUNTIME140.dll"])
        with self.assertRaisesRegex(ValueError, "dynamic Microsoft"):
            package.validate_static_msvc_runtime(inside, "herdr.exe")

    def test_dynamic_msvc_runtime_imports_are_rejected(self) -> None:
        for dll in (
            "VCRUNTIME140.dll",
            "vcruntime140_1.dll",
            "vcruntime140d.dll",
            "MSVCP140.dll",
            "MSVCP140D.dll",
            "MSVCR120.dll",
            "MSVCP120.dll",
            "concrt140d.dll",
            "ucrtbase.dll",
            "ucrtbased.dll",
            "api-ms-win-crt-runtime-l1-1-0.dll",
        ):
            with self.subTest(dll=dll):
                with self.assertRaisesRegex(ValueError, "dynamic Microsoft"):
                    package.validate_static_msvc_runtime(
                        self._pe_with_imports(0x8664, [dll]), "herdr.exe"
                    )

    def test_static_crt_imports_are_accepted(self) -> None:
        data = self._pe_with_imports(
            0x8664,
            [
                "KERNEL32.dll",
                "ntdll.dll",
                "msvcrt.dll",
                "api-ms-win-core-synch-l1-2-0.dll",
            ],
            ["USER32.dll"],
        )
        package.validate_static_msvc_runtime(data, "herdr.exe")
        self.assertFalse(package.is_dynamic_msvc_runtime("msvcrt.dll"))

    def test_stage_rejects_dynamic_crt_executable(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            metadata_path = root / "conpty.json"
            metadata_path.write_text(
                json.dumps(
                    {"schema_version": 1, "bundles": {"x86_64": {}}, "notices": []}
                ),
                encoding="utf-8",
            )
            herdr = root / "herdr.exe"
            herdr.write_bytes(self._pe_with_imports(0x8664, ["VCRUNTIME140.dll"]))
            with self.assertRaisesRegex(ValueError, "dynamic Microsoft"):
                package.stage_bundle(
                    metadata_path,
                    "x86_64",
                    root / "missing.nupkg",
                    herdr,
                    root / "stage",
                )

    @staticmethod
    def _pe(machine: int, payload: bytes) -> bytes:
        data = bytearray(0x80)
        data[:2] = b"MZ"
        struct.pack_into("<I", data, 0x3C, 0x40)
        data[0x40:0x44] = b"PE\0\0"
        struct.pack_into("<H", data, 0x44, machine)
        return bytes(data) + payload

    @staticmethod
    def _pe_with_imports(
        machine: int,
        imports: list[str],
        delay_imports: list[str] | None = None,
        magic: int = 0x20B,
        delay_imports_use_va: bool = False,
        import_directory_size: int | None = None,
    ) -> bytes:
        delay_imports = delay_imports or []
        pe_offset = 0x40
        optional_size = 0xF0 if magic == 0x20B else 0xE0
        data_directory_offset = 112 if magic == 0x20B else 96
        image_base = 0x140000000 if magic == 0x20B else 0x400000
        section_rva = 0x1000
        section_raw = 0x400

        import_size = 20 * (len(imports) + 1)
        delay_size = 32 * (len(delay_imports) + 1) if delay_imports else 0
        section = bytearray(import_size + delay_size)
        name_rvas: dict[str, int] = {}
        for name in [*imports, *delay_imports]:
            if name not in name_rvas:
                name_rvas[name] = section_rva + len(section)
                section += name.encode("ascii") + b"\0"
        for index, name in enumerate(imports):
            struct.pack_into("<I", section, index * 20 + 12, name_rvas[name])
        for index, name in enumerate(delay_imports):
            offset = import_size + index * 32
            if delay_imports_use_va:
                # Attributes leave the RVA flag unset, so the name is a VA.
                struct.pack_into("<I", section, offset + 4, image_base + name_rvas[name])
            else:
                struct.pack_into("<I", section, offset, 1)
                struct.pack_into("<I", section, offset + 4, name_rvas[name])

        data = bytearray(section_raw + len(section))
        data[:2] = b"MZ"
        struct.pack_into("<I", data, 0x3C, pe_offset)
        data[pe_offset : pe_offset + 4] = b"PE\0\0"
        struct.pack_into("<H", data, pe_offset + 4, machine)
        struct.pack_into("<H", data, pe_offset + 6, 1)
        struct.pack_into("<H", data, pe_offset + 20, optional_size)
        optional_offset = pe_offset + 24
        struct.pack_into("<H", data, optional_offset, magic)
        if magic == 0x20B:
            struct.pack_into("<Q", data, optional_offset + 24, image_base)
        else:
            struct.pack_into("<I", data, optional_offset + 28, image_base)
        struct.pack_into("<I", data, optional_offset + data_directory_offset - 4, 16)
        struct.pack_into(
            "<II",
            data,
            optional_offset + data_directory_offset + 8,
            section_rva,
            import_size if import_directory_size is None else import_directory_size,
        )
        if delay_imports:
            struct.pack_into(
                "<II",
                data,
                optional_offset + data_directory_offset + 13 * 8,
                section_rva + import_size,
                delay_size,
            )
        section_header_offset = pe_offset + 24 + optional_size
        data[section_header_offset : section_header_offset + 8] = b".rdata\0\0"
        struct.pack_into("<I", data, section_header_offset + 8, len(section))
        struct.pack_into("<I", data, section_header_offset + 12, section_rva)
        struct.pack_into("<I", data, section_header_offset + 16, len(section))
        struct.pack_into("<I", data, section_header_offset + 20, section_raw)
        data[section_raw : section_raw + len(section)] = section
        return bytes(data)

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
