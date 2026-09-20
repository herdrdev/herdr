import json
import os
import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest import mock

from scripts import release


class ReleaseTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="herdr-release-", dir="/var/tmp" if os.name != "nt" else None)
        self.addCleanup(self.temp.cleanup)
        self.previous_cwd = Path.cwd()
        os.chdir(self.temp.name)
        self.addCleanup(os.chdir, self.previous_cwd)
        self.git("init", "-q", "-b", "master")
        self.git("config", "user.name", "Release Test")
        self.git("config", "user.email", "release@example.invalid")
        self.put("Cargo.toml", '[package]\nname = "herdr"\nversion = "1.0.0"\n[dependencies]\nserde = "1"\n')
        self.put("Cargo.lock", 'version = 4\n[[package]]\nname = "herdr"\nversion = "1.0.0"\n[[package]]\nname = "serde"\nversion = "1.0.0"\n')
        self.put("src/main.rs", "fn main() {}\n")
        self.put("docs/next/CHANGELOG.md", "# Changelog\n")
        self.put("skills/herdr/SKILL.md", "Stable skill\n")
        self.put("docs/next/website/src/content/docs/obsolete.mdx", "Old page\n")
        self.put("distribution/latest.json", json.dumps({"version": "1.0.0"}))
        self.put("scripts/release.py", "# promotion tooling\n")
        self.put(".github/workflows/release.yml", "run: python3 scripts/release.py check-tag\n")
        self.put(".github/workflows/preview.yml", 'on:\n  push:\n    tags:\n      - "preview-*"\n')
        self.commit("stable")
        self.git("tag", "v1.0.0")
        self.put("src/main.rs", "fn main() { println!(\"preview\"); }\n")
        self.preview = self.commit("preview")
        self.git("tag", "preview-test")
        self.git("update-ref", "refs/remotes/origin/master", "HEAD")

    def git(self, *args):
        return subprocess.check_output(["git", *args], text=True, stderr=subprocess.STDOUT).strip()

    def put(self, path, text):
        target = Path(path)
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(text, encoding="utf-8")

    def commit(self, message):
        self.git("add", ".")
        self.git("commit", "-qm", message)
        return self.git("rev-parse", "HEAD")

    def prepare(self):
        for path in ("Cargo.toml", "Cargo.lock"):
            self.put(path, Path(path).read_text().replace('version = "1.0.0"', 'version = "1.0.1"', 1))
        self.put("CHANGELOG.md", "# 1.0.1\nFixed the bug.\n")
        return self.commit("release preparation")

    def test_promotion_excludes_newer_master_and_records_explicit_previous_stable(self):
        self.put("src/new-feature.rs", "untested feature\n")
        master = self.commit("newer master")
        self.git("update-ref", "refs/remotes/origin/master", master)
        self.git("checkout", "-q", "-b", "release/1.0.1", self.preview)
        candidate = self.prepare()
        self.git("tag", "-a", "v1.0.1", "-m", "v1.0.1\n\nPreview: preview-test\nPrevious-Stable: v1.0.0")
        self.assertEqual(release.tag_metadata("v1.0.1"), ("preview-test", "v1.0.0"))
        with mock.patch.object(release, "published_preview", return_value=self.preview):
            self.assertEqual(release.validate_release("preview-test", candidate, "1.0.1", "v1.0.0", "test/repo"), self.preview)
        self.assertFalse(Path("src/new-feature.rs").exists())
        self.git("checkout", "-q", "master")
        with self.assertRaisesRegex(ValueError, "unpreviewed change"):
            release.validate_diff(self.preview, master)

    def test_code_dependencies_schema_and_build_changes_are_rejected(self):
        cases = {
            "src/main.rs": "fn main() { panic!(); }\n",
            "Cargo.toml": '[package]\nname = "herdr"\nversion = "1.0.1"\n[dependencies]\nserde = "2"\n',
            "Cargo.lock": Path("Cargo.lock").read_text().replace('name = "serde"\nversion = "1.0.0"', 'name = "serde"\nversion = "2.0.0"'),
            "docs/next/api/herdr-api.schema.json": "{}\n",
            ".github/workflows/release.yml": "changed\n",
        }
        for path, content in cases.items():
            with self.subTest(path=path):
                self.git("reset", "--hard", self.preview)
                self.put(path, content)
                candidate = self.commit("not release preparation")
                with self.assertRaises(ValueError):
                    release.validate_diff(self.preview, candidate)

    def test_release_docs_and_skill_are_allowed_but_symlinks_are_not(self):
        for path in release.RELEASE_FILES | {"docs/next/website/src/content/docs/index.mdx"}:
            self.put(path, "release prose\n")
        candidate = self.prepare()
        release.validate_diff(self.preview, candidate, "1.0.1")
        if os.name != "nt":
            Path("CHANGELOG.md").unlink()
            Path("CHANGELOG.md").symlink_to("src/main.rs")
            with self.assertRaisesRegex(ValueError, "regular files"):
                release.validate_diff(self.preview, self.commit("symlink"))

    def test_required_release_files_cannot_be_deleted_but_website_pages_can(self):
        for path in ("docs/next/CHANGELOG.md", "skills/herdr/SKILL.md"):
            with self.subTest(path=path):
                self.git("reset", "--hard", self.preview)
                self.git("rm", path)
                candidate = self.commit("delete required release file")
                with self.assertRaisesRegex(ValueError, "regular files"):
                    release.validate_diff(self.preview, candidate)
        self.git("reset", "--hard", self.preview)
        self.git("rm", "docs/next/website/src/content/docs/obsolete.mdx")
        release.validate_diff(self.preview, self.commit("remove obsolete website page"))

    def test_preview_source_accepts_master_history_and_rejects_unpublished_branch(self):
        self.assertEqual(release.select_preview(self.preview), self.preview)
        self.assertEqual(release.select_preview("v1.0.0"), self.git("rev-parse", "v1.0.0"))
        self.put("src/main.rs", "unpublished work\n")
        candidate = self.commit("unpublished")
        with self.assertRaisesRegex(ValueError, "preview source must"):
            release.select_preview(candidate)

    def test_preview_source_rejects_legacy_dispatch_even_on_master(self):
        self.put(".github/workflows/preview.yml", "on:\n  workflow_dispatch:\n")
        legacy = self.commit("legacy preview workflow")
        self.git("update-ref", "refs/remotes/origin/master", legacy)
        with self.assertRaisesRegex(ValueError, "predates tag-triggered previews"):
            release.select_preview(legacy)

    def test_wrong_ancestry_and_mismatched_version_fail(self):
        candidate = self.prepare()
        with self.assertRaisesRegex(ValueError, "must descend"):
            release.validate_diff(candidate, self.preview)
        with self.assertRaisesRegex(ValueError, "version must match"):
            release.validate_diff(self.preview, candidate, "1.0.2")

    def test_published_preview_requires_immutable_complete_release_and_matching_remote_tag(self):
        payload = {"tag_name": "preview-test", "draft": False, "prerelease": True,
                   "immutable": True, "assets": [{"name": name} for name in release.ASSETS]}
        real_git = release.git

        def git(*args):
            if args[0] == "ls-remote":
                return f"{self.preview}\trefs/tags/preview-test"
            return real_git(*args)

        real_output = subprocess.check_output

        def output(args, **kwargs):
            return json.dumps(payload) if args[0] == "gh" else real_output(args, **kwargs)

        with mock.patch.object(release, "git", side_effect=git), mock.patch.object(release.subprocess, "check_output", side_effect=output):
            self.assertEqual(release.published_preview("preview-test", "test/repo"), self.preview)
            for key, value in (("draft", True), ("prerelease", False), ("immutable", False), ("assets", [])):
                with self.subTest(key=key), mock.patch.dict(payload, {key: value}):
                    with self.assertRaisesRegex(ValueError, "immutable published preview"):
                        release.published_preview("preview-test", "test/repo")
            self.git("tag", "-f", "preview-test", "v1.0.0")
            with self.assertRaisesRegex(ValueError, "does not match"):
                release.published_preview("preview-test", "test/repo")

    def test_missing_provenance_or_stale_previous_release_fails(self):
        self.git("tag", "v1.0.1")
        with self.assertRaisesRegex(ValueError, "annotated"):
            release.tag_metadata("v1.0.1")
        self.git("tag", "-af", "v1.0.1", "-m", "v1.0.1")
        with self.assertRaisesRegex(ValueError, "Preview"):
            release.tag_metadata("v1.0.1")
        candidate = self.prepare()
        with mock.patch.object(release, "published_preview", return_value=self.preview):
            with self.assertRaisesRegex(ValueError, "currently published"):
                release.validate_release("preview-test", candidate, "1.0.1", "v0.9.0", "test/repo")
            with self.assertRaisesRegex(ValueError, "must increase"):
                release.validate_release("preview-test", candidate, "1.0.1", "v1.0.1", "test/repo")

    def test_hotfix_isolated_from_master_and_metadata_sync_preserves_master_code(self):
        self.put("src/feature.rs", "b and c\n")
        master = self.commit("features b and c")
        self.git("checkout", "-q", "-b", "release/hotfix", "v1.0.0")
        self.put("src/main.rs", "fn main() { /* fix d */ }\n")
        hotfix = self.commit("fix d")
        self.git("update-ref", "refs/remotes/origin/release/hotfix", hotfix)
        self.assertEqual(release.select_hotfix("release/hotfix", "v1.0.0"), hotfix)
        self.assertEqual(release.select_preview(hotfix), hotfix)
        self.git("rm", "scripts/release.py")
        legacy = self.commit("legacy release tooling")
        self.git("update-ref", "refs/remotes/origin/release/hotfix", legacy)
        with self.assertRaisesRegex(ValueError, "predates preview promotion"):
            release.select_hotfix("release/hotfix", "v1.0.0")
        self.git("reset", "--hard", hotfix)
        self.git("update-ref", "refs/remotes/origin/release/hotfix", hotfix)
        with self.assertRaisesRegex(ValueError, "release/\\*"):
            release.select_hotfix("feature/unreviewed", "v1.0.0")
        self.git("tag", "v2.0.0", master)
        with self.assertRaisesRegex(ValueError, "must descend"):
            release.select_hotfix("release/hotfix", "v2.0.0")
        candidate = self.prepare()
        release.validate_diff(hotfix, candidate, "1.0.1")
        patch = subprocess.check_output(["git", "diff", "--binary", hotfix, candidate])
        self.git("checkout", "-q", "master")
        subprocess.run(["git", "apply", "--3way", "--index"], input=patch, check=True)
        self.assertEqual(Path("src/feature.rs").read_text(), "b and c\n")
        self.assertNotIn("fix d", Path("src/main.rs").read_text())
        self.assertEqual(release.normalized_cargo(Path("Cargo.toml").read_text(), "Cargo.toml")[1], "1.0.1")


if __name__ == "__main__":
    unittest.main()
