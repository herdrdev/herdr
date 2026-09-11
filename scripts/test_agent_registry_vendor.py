from __future__ import annotations

import contextlib
import hashlib
import io
import json
import os
import subprocess
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

from scripts import agent_registry_vendor as vendor


class AgentRegistryVendorTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()
        self.source = self.root / "source"
        self.project = self.root / "project"
        self.project.mkdir()
        self.put("agents/zeta/agent.toml", b'schema = 1\nid = "zeta"\n')
        self.put("agents/alpha/assets/hook.sh", "#!/bin/sh\r\n# café\r\n".encode())
        self.put("agents/alpha/agent.toml", b'schema = 1\nid = "alpha"\n')
        self.put("README.md", b"Not part of the bundled agents tree\n")

    def put(self, name: str, content: bytes) -> Path:
        path = self.source / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(content)
        return path

    @property
    def vendored(self) -> Path:
        return self.project / vendor.VENDOR_PATH

    @property
    def index(self) -> Path:
        return self.project / vendor.INDEX_PATH

    def sync(self) -> None:
        vendor.sync(self.source, self.project)

    def snapshot(self) -> dict[str, bytes]:
        return {
            path.relative_to(self.project).as_posix(): path.read_bytes()
            for path in self.project.rglob("*") if path.is_file()
        }

    def test_sync_is_deterministic_pins_exact_bytes_and_checks_offline(self) -> None:
        self.sync()
        before = self.snapshot()
        self.sync()
        self.assertEqual(before, self.snapshot())
        lock = json.loads((self.vendored / "lock.json").read_bytes())
        self.assertEqual(lock["repository"], "https://github.com/herdrdev/agent-registry")
        self.assertNotIn("revision", lock)
        self.assertNotIn("commit", lock)
        paths = [item["path"] for item in lock["files"]]
        self.assertEqual(paths, sorted(paths))
        self.assertEqual(len(paths), 3)
        self.assertNotIn("README.md", paths)
        for item in lock["files"]:
            content = (self.source / item["path"]).read_bytes()
            self.assertEqual((self.vendored / item["path"]).read_bytes(), content)
            self.assertEqual(item["sha256"], hashlib.sha256(content).hexdigest())
        aggregate = "".join(f"{item['sha256']}  {item['path']}\n" for item in lock["files"])
        self.assertEqual(lock["sha256"], hashlib.sha256(aggregate.encode()).hexdigest())
        index = self.index.read_text()
        self.assertIn("pub(super) const FILES: &[(&str, &str)] = &[", index)
        for path in paths:
            self.assertIn(
                f'("{path}", include_str!(concat!(env!("CARGO_MANIFEST_DIR"), '
                f'"/vendor/agent-registry/{path}")))', index,
            )
        self.source.rename(self.root / "source-unavailable")
        vendor.check(self.project)
        self.assertEqual(before, self.snapshot())

    def test_sync_removes_stale_files(self) -> None:
        self.sync()
        (self.source / "agents/zeta/agent.toml").unlink()
        self.sync()
        self.assertFalse((self.vendored / "agents/zeta/agent.toml").exists())
        self.assertNotIn("zeta", self.index.read_text())
        vendor.check(self.project)

    def test_check_rejects_changed_missing_extra_and_renamed_files(self) -> None:
        for mutation in ("changed", "missing", "extra", "renamed", "root-extra"):
            with self.subTest(mutation=mutation):
                self.sync()
                path = self.vendored / "agents/alpha/agent.toml"
                if mutation == "changed":
                    path.write_bytes(path.read_bytes() + b"# drift\n")
                elif mutation == "missing":
                    path.unlink()
                elif mutation == "extra":
                    (path.parent / "extra.txt").write_text("extra")
                elif mutation == "renamed":
                    path.rename(path.with_name("other.toml"))
                else:
                    (self.vendored / "extra.txt").write_text("extra")
                before = self.snapshot()
                with self.assertRaises(vendor.VendorError):
                    vendor.check(self.project)
                self.assertEqual(before, self.snapshot())

    def test_check_rejects_lock_and_generated_index_drift(self) -> None:
        for field in ("sha256", "repository", "schema", "files", "index", "json"):
            with self.subTest(field=field):
                self.sync()
                path = self.vendored / "lock.json"
                lock = json.loads(path.read_bytes())
                if field == "index":
                    self.index.write_text("// stale index\n")
                elif field == "json":
                    path.write_text("{broken")
                else:
                    lock[field] = [] if field == "files" else "incorrect"
                    path.write_text(json.dumps(lock, indent=2) + "\n")
                with self.assertRaises(vendor.VendorError):
                    vendor.check(self.project)

    def test_input_validation_failure_preserves_existing_snapshot(self) -> None:
        self.sync()
        before = self.snapshot()
        for content in (b"\xff", b"\x00binary", b"bad\x01text"):
            with self.subTest(content=content):
                self.put("agents/alpha/assets/bad.bin", content)
                with self.assertRaises(vendor.VendorError):
                    self.sync()
                self.assertEqual(before, self.snapshot())

    def test_empty_or_missing_source_preserves_snapshot(self) -> None:
        self.sync()
        before = self.snapshot()
        for source in (self.root / "missing", self.root / "empty"):
            if source.name == "empty":
                (source / "agents").mkdir(parents=True)
            with self.assertRaises(vendor.VendorError):
                vendor.sync(source, self.project)
            self.assertEqual(before, self.snapshot())

    def test_limits_are_enforced_before_replacement(self) -> None:
        self.sync()
        before = self.snapshot()
        for limit in ("MAX_FILES", "MAX_ENTRIES", "MAX_FILE_BYTES", "MAX_TOTAL_BYTES", "MAX_PATH_BYTES", "MAX_DEPTH"):
            with self.subTest(limit=limit), patch.object(vendor, limit, 1):
                with self.assertRaises(vendor.VendorError):
                    self.sync()
                self.assertEqual(before, self.snapshot())

    def test_rejects_unsafe_portable_paths(self) -> None:
        for path in (
            "../escape", "/absolute", "agents//a", "agents/./a", "agents/a/../b",
            "agents/a/back\\slash", 'agents/a/quote"', "agents/a/a:b", "agents/a/a.",
            "agents/a/NUL.txt", "agents/CON/hook", "agents/a/com1.sh", "agents/a/LPT9",
            "agents/a/a\nline", "agents/a/sp ace", "agents/a/é", "C:/agents/a",
        ):
            with self.subTest(path=path), self.assertRaises(vendor.VendorError):
                vendor.validate_path(path)
        vendor.validate_path("agents/a/assets/__init__.py")

    def test_rejects_casefold_file_and_directory_collisions(self) -> None:
        if (self.source / "agents/ALPHA").exists():
            self.skipTest("filesystem cannot represent casefold collisions")
        self.sync()
        before = self.snapshot()
        for name in ("agents/alpha/AGENT.toml", "agents/ALPHA/other.toml"):
            with self.subTest(name=name):
                path = self.put(name, b"collision")
                with self.assertRaisesRegex(vendor.VendorError, "collision"):
                    self.sync()
                self.assertEqual(before, self.snapshot())
                path.unlink()
                if path.parent.name == "ALPHA":
                    path.parent.rmdir()

    def symlink(self, target: Path, link: Path) -> None:
        try:
            link.symlink_to(target, target_is_directory=target.is_dir())
        except (OSError, NotImplementedError) as exc:
            self.skipTest(f"symlinks unavailable: {exc}")

    def test_source_symlinks_are_rejected(self) -> None:
        self.sync()
        before = self.snapshot()
        for target in (self.source / "README.md", self.source / "agents/alpha"):
            link = self.source / "agents/link"
            self.symlink(target, link)
            with self.assertRaisesRegex(vendor.VendorError, "symlink"):
                self.sync()
            self.assertEqual(before, self.snapshot())
            link.unlink()
        link = self.root / "linked-source"
        self.symlink(self.source, link)
        with self.assertRaisesRegex(vendor.VendorError, "symlink"):
            vendor.sync(link, self.project)

    def test_check_rejects_symlink_even_with_identical_contents(self) -> None:
        self.sync()
        path = self.vendored / "agents/alpha/agent.toml"
        path.unlink()
        self.symlink(self.source / "agents/alpha/agent.toml", path)
        with self.assertRaisesRegex(vendor.VendorError, "symlink"):
            vendor.check(self.project)

    @unittest.skipUnless(hasattr(os, "mkfifo"), "requires FIFO support")
    def test_nonregular_files_rejected_without_opening_them(self) -> None:
        os.mkfifo(self.source / "agents/pipe")
        with self.assertRaisesRegex(vendor.VendorError, "regular file"):
            self.sync()

    def test_index_replacement_failure_rolls_back_vendor(self) -> None:
        self.sync()
        before = self.snapshot()
        self.put("agents/alpha/agent.toml", b"new contents")
        replace = os.replace

        def fail_index(source: Path, destination: Path) -> None:
            if destination == self.index:
                raise OSError("simulated index replacement failure")
            replace(source, destination)

        with patch.object(vendor.os, "replace", side_effect=fail_index):
            with self.assertRaisesRegex(OSError, "simulated"):
                self.sync()
        self.assertEqual(before, self.snapshot())
        vendor.check(self.project)

    def test_cli_modes_are_explicit_and_exclusive(self) -> None:
        for args in ([], ["sync"], ["--source", "."], ["sync", "--source", ".", "--check"], ["--check", "--source", "."]):
            with self.subTest(args=args), contextlib.redirect_stderr(io.StringIO()):
                with self.assertRaises(SystemExit):
                    vendor.main(args)
        with patch.object(vendor, "check") as check, contextlib.redirect_stdout(io.StringIO()):
            self.assertEqual(vendor.main(["--check"]), 0)
            check.assert_called_once_with()
        with patch.object(vendor, "sync") as sync, contextlib.redirect_stdout(io.StringIO()):
            self.assertEqual(vendor.main(["sync", "--source", "/local/source"]), 0)
            sync.assert_called_once_with(Path("/local/source"))


class ImmutableSnapshotTests(AgentRegistryVendorTests):
    def setUp(self) -> None:
        super().setUp()
        self.raw = (Path(__file__).parent / "fixtures/agent-registry-snapshot-v1.json").read_bytes()
        self.snapshot_path = self.root / "snapshot.json"
        self.snapshot_path.write_bytes(self.raw)
        self.digest = hashlib.sha256(self.raw).hexdigest()
        self.validator = self.root / "validator with spaces"
        def validate(argv, *, check, timeout):
            self.assertEqual(argv[:3], [str(self.validator), "registry", "validate-snapshot"])
            self.assertEqual(argv[4:], ["--runtime-compatible"])
            self.assertTrue(check)
            self.assertEqual(timeout, 120)
            self.assertEqual(Path(argv[3]).read_bytes(), self.snapshot_path.read_bytes())
        self.validation_patch = patch.object(vendor.subprocess, "run", side_effect=validate)
        self.validation = self.validation_patch.start()
        self.addCleanup(self.validation_patch.stop)

    def import_snapshot(self) -> None:
        vendor.sync_snapshot(self.snapshot_path, self.digest, self.validator, self.project)

    def test_golden_exact_digest_and_offline_import(self) -> None:
        self.assertEqual(len(self.raw), 638)
        self.assertEqual(self.digest, "9ae1f4c37154a28f38bee9048e85e5c3d457b7838c6ae8baa3753f6dc02130ea")
        self.import_snapshot()
        vendor.check(self.project)
        files = vendor.snapshot_files(self.raw)
        self.assertEqual((self.vendored / "agents/example/agent.toml").read_bytes(), files["agents/example/agent.toml"])
        self.assertEqual(json.loads((self.vendored / "lock.json").read_bytes())["sha256"], json.loads(self.raw)["content_sha256"])

    def test_import_canonicalizes_its_own_temporary_directory_alias(self) -> None:
        temporary = self.root / "temporary"
        temporary.mkdir()
        alias = self.root / "temporary-alias"
        self.symlink(temporary, alias)
        with patch.object(vendor.tempfile, "tempdir", str(alias)):
            self.import_snapshot()
        vendor.check(self.project)
        self.assertEqual(list(temporary.iterdir()), [])
        self.assertTrue(alias.is_symlink())

    def test_snapshot_invalid_contract_rejected_without_vendor_changes(self) -> None:
        self.sync()
        before = self.snapshot()
        for mutation in ("dirty", "commit", "unknown", "schema-bool", "compat-bool", "length", "hash", "inventory", "traversal", "duplicate", "order", "file-limit"):
            with self.subTest(mutation=mutation):
                value = json.loads(self.raw)
                if mutation == "dirty": value["source"]["dirty"] = True
                elif mutation == "commit": value["source"]["commit"] = "not-a-commit"
                elif mutation == "unknown": value["extra"] = 1
                elif mutation == "schema-bool": value["schema"] = True
                elif mutation == "compat-bool": value["compatibility"]["registry_api"] = True
                elif mutation == "length": value["files"][0]["bytes"] += 1
                elif mutation == "hash": value["files"][0]["sha256"] = "0" * 64
                elif mutation == "inventory": value["content_sha256"] = "0" * 64
                elif mutation == "traversal": value["files"][0]["path"] = "agents/../escape"
                elif mutation == "order": value["files"] *= 2
                elif mutation == "file-limit": value["files"][0]["text"] = " " * (256 * 1024 + 1)
                raw = json.dumps(value).encode()
                if mutation == "duplicate": raw = raw.replace(b'"schema": 1', b'"schema": 1,"schema": 1')
                self.snapshot_path.write_bytes(raw)
                self.digest = hashlib.sha256(raw).hexdigest()
                with self.assertRaises(vendor.VendorError): self.import_snapshot()
                self.assertEqual(self.snapshot(), before)

    def test_reviewed_hash_validator_failure_and_mutation_preserve_vendor(self) -> None:
        self.sync()
        before = self.snapshot()
        self.digest = "0" * 64
        with self.assertRaisesRegex(vendor.VendorError, "SHA-256 mismatch"):
            self.import_snapshot()
        self.digest = hashlib.sha256(self.raw).hexdigest()
        def mutate(argv, **kwargs):
            exact = Path(argv[3])
            exact.chmod(0o600)
            exact.write_text("mutated")
        for failure in (subprocess.CalledProcessError(1, str(self.validator)), mutate):
            self.validation.side_effect = failure
            with self.assertRaises(vendor.VendorError): self.import_snapshot()
            self.assertEqual(self.snapshot(), before)

    def test_immutable_import_uses_existing_index_rollback(self) -> None:
        self.sync()
        before = self.snapshot()
        replace = os.replace
        def fail_index(source, destination):
            if destination == self.index: raise OSError("simulated index failure")
            return replace(source, destination)
        with patch.object(vendor.os, "replace", side_effect=fail_index):
            with self.assertRaisesRegex(OSError, "simulated"):
                self.import_snapshot()
        self.assertEqual(before, self.snapshot())

    @unittest.skipUnless(os.environ.get("HERDR_VALIDATOR"), "set HERDR_VALIDATOR to a matching absolute Herdr executable")
    def test_real_validator_import_and_semantic_rejection(self) -> None:
        # Parent builds/provides the executable. No Cargo, Git, or network here;
        # all vendor/index writes stay in this test's temporary project.
        self.validation_patch.stop()
        self.validator = Path(os.environ["HERDR_VALIDATOR"])
        self.import_snapshot()
        vendor.check(self.project)
        before = self.snapshot()
        value = json.loads(self.raw)
        item = value["files"][0]
        item["text"] = item["text"].split("\n[launch]")[0] + "\n"
        content = item["text"].encode("utf-8")
        item["bytes"] = len(content)
        item["sha256"] = hashlib.sha256(content).hexdigest()
        value["content_sha256"] = json.loads(vendor.lock_bytes({item["path"]: content}))["sha256"]
        raw = (json.dumps(value, ensure_ascii=False, separators=(",", ":")) + "\n").encode("utf-8")
        self.snapshot_path.write_bytes(raw)
        self.digest = hashlib.sha256(raw).hexdigest()
        # Framing and integrity still pass. Actual Herdr must reject the absent
        # launch table even though startable=false, without replacing LKG.
        vendor.snapshot_files(raw)
        with self.assertRaisesRegex(vendor.VendorError, "Herdr snapshot validation failed"):
            self.import_snapshot()
        self.assertEqual(self.snapshot(), before)

    def test_snapshot_cli_is_explicit(self) -> None:
        with patch.object(vendor, "sync_snapshot") as sync, contextlib.redirect_stdout(io.StringIO()):
            self.assertEqual(vendor.main(["sync-snapshot", "--snapshot", str(self.snapshot_path), "--sha256", self.digest, "--validator", str(self.validator)]), 0)
            sync.assert_called_once_with(self.snapshot_path, self.digest, self.validator)
        for args in (["sync-snapshot"], ["--check", "--snapshot", "x"], ["sync", "--source", ".", "--validator", "/x"]):
            with contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit):
                vendor.main(args)


if __name__ == "__main__":
    unittest.main()
