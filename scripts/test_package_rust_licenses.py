import hashlib
import json
from pathlib import Path
import tempfile
import unittest
import zipfile

from scripts.package_rust_licenses import collect, write_archive


class RustLicensePackageTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.supplements = self.root / "packaging/licenses/rust"
        self.supplements.mkdir(parents=True)
        (self.supplements / "fontes-complementares.json").write_text('{"arquivos": []}')
        (self.root / "LICENSE").write_bytes(b"Herdr license\n")
        self.crate = self.root / "dependency"
        self.crate.mkdir()
        self.original = b"Copyright Example\r\nOriginal license\r\n"
        (self.crate / "LICENSE").write_bytes(self.original)
        package = {"name": "example", "version": "1.0.0", "license": "MIT",
                   "manifest_path": str(self.crate / "Cargo.toml")}
        self.report = {"crates": [{"package": package}], "licenses": [
            {"source_path": str(self.crate / "LICENSE"), "text": self.original.decode(),
             "used_by": [{"crate": package}]}]}

    def test_original_bytes_and_reproducible_archive_without_local_paths(self):
        files, inventory = collect(self.report, self.root)
        first, second = self.root / "first.zip", self.root / "second.zip"
        for output in (first, second):
            write_archive(output, "aarch64-apple-darwin", files, inventory)
        self.assertEqual(first.read_bytes(), second.read_bytes())
        with zipfile.ZipFile(first) as archive:
            manifest = archive.read("origem.json")
            self.assertNotIn(str(self.root).encode(), manifest)
            record = json.loads(manifest)["pacotes"][0]["textos"][0]
            self.assertEqual(archive.read(record["arquivo"]), self.original)
            self.assertEqual(record["sha256"], hashlib.sha256(self.original).hexdigest())
        before = first.read_bytes()
        with self.assertRaises(FileExistsError):
            write_archive(first, "aarch64-apple-darwin", files, inventory)
        self.assertEqual(first.read_bytes(), before)

    def test_missing_original_does_not_use_synthesized_text(self):
        self.report["licenses"][0]["source_path"] = None
        self.report["licenses"][0]["text"] = "Generic text without attribution"
        with self.assertRaisesRegex(ValueError, "Original license text missing"):
            collect(self.report, self.root)

    def test_altered_supplement_is_rejected(self):
        (self.supplements / "LICENSE").write_bytes(b"altered")
        (self.supplements / "fontes-complementares.json").write_text(json.dumps({"arquivos": [
            {"arquivo": "LICENSE", "sha256": hashlib.sha256(b"original").hexdigest(), "fontes": []}]}))
        with self.assertRaisesRegex(ValueError, "hash mismatch"):
            collect(self.report, self.root)

    def test_source_outside_dependencies_is_rejected(self):
        self.report["licenses"][0]["source_path"] = str(self.root / "LICENSE")
        with self.assertRaisesRegex(ValueError, "outside the dependency sources"):
            collect(self.report, self.root)

    def test_source_changed_after_report_is_rejected(self):
        (self.crate / "LICENSE").write_bytes(b"Changed after harvesting")
        with self.assertRaisesRegex(ValueError, "changed since the report"):
            collect(self.report, self.root)

    def test_project_manifest_does_not_allow_reading_unrelated_root_files(self):
        self.report["crates"][0]["package"]["manifest_path"] = str(self.root / "Cargo.toml")
        secret = self.root / ".env"
        secret.write_bytes(self.original)
        self.report["licenses"][0]["source_path"] = str(secret)
        with self.assertRaisesRegex(ValueError, "outside the dependency sources"):
            collect(self.report, self.root)
