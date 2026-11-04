import hashlib
import json
from pathlib import Path
import stat
import struct
import tempfile
import unittest
import zipfile

from scripts import package_pretorian_candidate as candidate


class PretorianCandidateTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)

    def notices(self, target, *, altered=False):
        path = self.root / (target + ".zip")
        text = b"Original license\r\n"
        digest = hashlib.sha256(text).hexdigest()
        name = f"textos/{digest}.txt"
        manifest = {"alvo": target, "fonte_commit": "a" * 40, "pacotes": [{"pacote": "fixture", "versao": "1",
                    "textos": [{"arquivo": name, "sha256": digest, "fonte": "fixture/LICENSE"}]}]}
        with zipfile.ZipFile(path, "w") as archive:
            archive.writestr("origem.json", json.dumps(manifest))
            archive.writestr(name, b"Altered" if altered else text)
        return path

    def test_unix_candidates_preserve_files_and_are_not_approved(self):
        for platform, header in (
            ("macos-arm64", struct.pack("<II", 0xFEEDFACF, 0x0100000C)),
            ("linux-x64", b"\x7fELF\x02\x01" + bytes(12) + struct.pack("<H", 62)),
        ):
            with self.subTest(platform=platform):
                source = self.root / platform
                source.write_bytes(header)
                files = candidate.collect(platform, source, self.notices(candidate.TARGETS[platform]), "a" * 40, toolchain_notices=self.toolchain(target=candidate.TARGETS[platform]))
                self.assertEqual(files["licencas/TOOLCHAIN-LICENSES.zip"], (self.root / "toolchain.zip").read_bytes())
                output = self.root / (platform + "-output")
                candidate.package(platform, files, "a" * 40, output)
                metadata = json.loads((output / "candidato.json").read_text())
                self.assertEqual(metadata["estado"], "candidato")
                item = metadata["artefatos"][platform]
                archive_path = output / item["arquivo"]
                self.assertEqual(hashlib.sha256(archive_path.read_bytes()).hexdigest(), item["sha256"])
                with zipfile.ZipFile(archive_path) as archive:
                    self.assertEqual(set(archive.namelist()), set(files))
                    self.assertFalse(any(info.is_dir() for info in archive.infolist()))
                    for name, original in files.items():
                        self.assertEqual(archive.read(name), original)
                        self.assertEqual(hashlib.sha256(original).hexdigest(), item["arquivos"][name])
                    self.assertEqual(stat.S_IMODE(archive.getinfo("herdr-pretorian").external_attr >> 16), 0o755)
                with self.assertRaisesRegex(ValueError, "already exists"):
                    candidate.package(platform, files, "a" * 40, output)

    def test_notices_for_another_target_are_rejected(self):
        with self.assertRaisesRegex(ValueError, "target mismatch"):
            candidate.validate_rust_notices(self.notices("aarch64-apple-darwin"), "x86_64-pc-windows-msvc", "a" * 40)

    def test_altered_license_text_is_rejected(self):
        target = "aarch64-apple-darwin"
        with self.assertRaisesRegex(ValueError, "hash mismatch"):
            candidate.validate_rust_notices(self.notices(target, altered=True), target, "a" * 40)

    def test_wrong_executable_architecture_is_rejected(self):
        source = self.root / "wrong-binary"
        source.write_bytes(b"not an ARM64 executable")
        with self.assertRaisesRegex(ValueError, "architecture"):
            candidate.collect("macos-arm64", source, self.notices("aarch64-apple-darwin"), "a" * 40, toolchain_notices=self.toolchain())

    def test_build_record_binds_binary_and_notices_to_source_and_target(self):
        files = {"herdr-pretorian": b"binary", "licencas/RUST-LICENSES.zip": b"notices", "licencas/TOOLCHAIN-LICENSES.zip": b"tools"}
        record = ("commit=" + "a" * 40 + "\ntarget=aarch64-apple-darwin\n"
                  + "sha256=" + hashlib.sha256(b"binary").hexdigest() + "  herdr-macos-aarch64\n"
                  + "sha256=" + hashlib.sha256(b"notices").hexdigest() + "  RUST-LICENSES.zip\n"
                  + "sha256=" + hashlib.sha256(b"tools").hexdigest() + "  TOOLCHAIN-LICENSES.zip\n")
        candidate.verify_build(files, "macos-arm64", "herdr-macos-aarch64", record, "a" * 40)
        for name in files:
            changed = dict(files)
            changed[name] = b"from another build"
            with self.subTest(name=name), self.assertRaisesRegex(ValueError, "not bound"):
                candidate.verify_build(changed, "macos-arm64", "herdr-macos-aarch64", record, "a" * 40)
        with self.assertRaisesRegex(ValueError, "source or target mismatch"):
            candidate.verify_build(files, "macos-arm64", "herdr-macos-aarch64", record, "b" * 40)
        with self.assertRaisesRegex(ValueError, "source or target mismatch"):
            candidate.verify_build(files, "linux-x64", "herdr-macos-aarch64", record, "a" * 40)

    def test_notices_from_another_commit_are_rejected(self):
        target = "aarch64-apple-darwin"
        with self.assertRaisesRegex(ValueError, "source commit mismatch"):
            candidate.validate_rust_notices(self.notices(target), target, "b" * 40)

    def toolchain(self, *, changed=False, commit="a" * 40, target="aarch64-apple-darwin"):
        path = self.root / "toolchain.zip"
        files = {"rust/COPYRIGHT-library.html": b"Rust original", "zig/LICENSE": b"Zig original",
                 "zig/musl/COPYRIGHT": b"musl original"}
        metadata = {"alvo": target, "fonte_commit": commit, "zig_version": "0.16.0",
                    "rustc_vV": "rustc 1.96.1 (31fca3adb 2026-06-26)\nrelease: 1.96.1",
                    "arquivos": {n: hashlib.sha256(v).hexdigest() for n, v in files.items()}}
        with zipfile.ZipFile(path, "w") as archive:
            archive.writestr("origem.json", json.dumps(metadata))
            for name, data in files.items():
                archive.writestr(name, b"changed" if changed else data)
        return path

    def test_toolchain_notices_require_same_build_and_original_bytes(self):
        path = self.toolchain()
        self.assertEqual(candidate.validate_toolchain_notices(path, "aarch64-apple-darwin", "a" * 40), path.read_bytes())
        for options in ({"changed": True}, {"commit": "b" * 40}, {"target": "x86_64-unknown-linux-musl"}):
            with self.subTest(options=options), self.assertRaises(ValueError):
                candidate.validate_toolchain_notices(self.toolchain(**options), "aarch64-apple-darwin", "a" * 40)
