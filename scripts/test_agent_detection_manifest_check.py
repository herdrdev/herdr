import hashlib
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from scripts import agent_detection_manifest_check as check


def manifest(agent_id: str, version: str, contains: str = "ready") -> str:
    return f'''id = "{agent_id}"
version = "{version}"
min_engine_version = 1
updated_at = "2026-06-10T00:00:00Z"

[[rules]]
id = "idle"
state = "idle"
contains = ["{contains}"]
'''


def catalog(agent_id: str = "codex", path: str = "codex.toml") -> str:
    return f'''schema_version = 1

[[agents]]
id = "{agent_id}"
path = "{path}"
'''


STAGED_BUNDLED_MANIFEST = manifest("codex", "2026.06.10.2").replace(
    "min_engine_version = 1", "min_engine_version = 2"
)
STAGED_PUBLISHED_MANIFEST = manifest("codex", "2026.06.10.1")
STAGED_TEST_EXCEPTION = {
    "codex": (
        "2026.06.10.2",
        "2026.06.10.1",
        hashlib.sha256(STAGED_PUBLISHED_MANIFEST.encode()).hexdigest(),
    ),
}


def staged_manifest_dirs(root: Path) -> tuple[Path, Path]:
    bundled = root / "bundled"
    published = root / "published"
    bundled.mkdir()
    published.mkdir()
    (bundled / "codex.toml").write_text(STAGED_BUNDLED_MANIFEST, encoding="utf-8", newline="\n")
    (published / "codex.toml").write_text(STAGED_PUBLISHED_MANIFEST, encoding="utf-8", newline="\n")
    (published / "index.toml").write_text(catalog())
    return bundled, published


UNPUBLISHED_TEST_MANIFEST = manifest("testagent", "2026.06.10.1")
UNPUBLISHED_TEST_EXCEPTION = {
    "testagent": (
        "2026.06.10.1",
        hashlib.sha256(UNPUBLISHED_TEST_MANIFEST.encode()).hexdigest(),
    ),
}


def unpublished_manifest_dirs(root: Path) -> tuple[Path, Path]:
    bundled = root / "bundled"
    published = root / "published"
    bundled.mkdir()
    published.mkdir()
    (bundled / "testagent.toml").write_text(UNPUBLISHED_TEST_MANIFEST, encoding="utf-8", newline="\n")
    (published / "index.toml").write_text("schema_version = 1\nagents = []\n")
    return bundled, published


class AgentDetectionManifestCheckTests(unittest.TestCase):
    def test_validates_bundled_and_matching_published_catalog(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            bundled = root / "bundled"
            website = root / "website"
            bundled.mkdir()
            website.mkdir()
            content = manifest("codex", "2026.06.10.1")
            (bundled / "codex.toml").write_text(content)
            (website / "codex.toml").write_text(content)
            (website / "index.toml").write_text(catalog())

            bundled_manifests = check.load_manifest_dir(bundled, engine_version=1)
            check.validate_catalog(website, bundled_manifests, engine_version=1)

    def test_rejects_published_version_lower_than_bundled(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            bundled = root / "bundled"
            website = root / "website"
            bundled.mkdir()
            website.mkdir()
            (bundled / "codex.toml").write_text(manifest("codex", "2026.06.10.2"))
            (website / "codex.toml").write_text(manifest("codex", "2026.06.10.1"))
            (website / "index.toml").write_text(catalog())

            bundled_manifests = check.load_manifest_dir(bundled, engine_version=1)
            with self.assertRaisesRegex(check.CheckError, "lower than bundled"):
                check.validate_catalog(website, bundled_manifests, engine_version=1)

    @patch.dict(check.STAGED_PUBLISHED_MANIFESTS, STAGED_TEST_EXCEPTION, clear=True)
    def test_allows_explicitly_staged_published_manifest(self):
        with tempfile.TemporaryDirectory() as tmp:
            bundled, website = staged_manifest_dirs(Path(tmp))

            bundled_manifests = check.load_manifest_dir(bundled, engine_version=2)
            check.validate_catalog(website, bundled_manifests, engine_version=2)

    @patch.dict(check.STAGED_PUBLISHED_MANIFESTS, STAGED_TEST_EXCEPTION, clear=True)
    def test_rejects_mutated_staged_published_manifest(self):
        with tempfile.TemporaryDirectory() as tmp:
            bundled, website = staged_manifest_dirs(Path(tmp))
            with (website / "codex.toml").open("a") as manifest_file:
                manifest_file.write("\n# unexpected mutation\n")

            bundled_manifests = check.load_manifest_dir(bundled, engine_version=2)
            with self.assertRaisesRegex(check.CheckError, "lower than bundled"):
                check.validate_catalog(website, bundled_manifests, engine_version=2)

    def test_claude_staging_pins_published_bytes_and_preserves_version_order(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            bundled = root / "bundled"
            published = root / "published"
            bundled.mkdir()
            published.mkdir()
            bundled_path = bundled / "claude.toml"
            published_path = published / "claude.toml"
            bundled_bytes = (check.DEFAULT_BUNDLED_DIR / "claude.toml").read_bytes()
            published_bytes = (check.DEFAULT_PUBLISHED_DIR / "claude.toml").read_bytes()
            bundled_path.write_bytes(bundled_bytes)
            published_path.write_bytes(published_bytes)
            (published / "index.toml").write_text(catalog("claude", "claude.toml"))
            bundled_agents = check.load_manifest_dir(bundled, engine_version=5)

            check.validate_catalog(published, bundled_agents, engine_version=5)
            with patch.dict(check.STAGED_PUBLISHED_MANIFESTS, {}, clear=True):
                with self.assertRaisesRegex(check.CheckError, "lower than bundled"):
                    check.validate_catalog(published, bundled_agents, engine_version=5)

            published_path.write_bytes(published_bytes + b"\n# unexpected mutation\n")
            with self.assertRaisesRegex(check.CheckError, "lower than bundled"):
                check.validate_catalog(published, bundled_agents, engine_version=5)
            published_path.write_bytes(published_bytes)

            bundled_version = bundled_agents["claude"][1]["version"]
            published_version = check.validate_manifest(published_path, engine_version=5)["version"]
            bundled_path.write_bytes(
                bundled_bytes.replace(
                    f'version = "{bundled_version}"'.encode(),
                    f'version = "{published_version}"'.encode(),
                    1,
                )
            )
            same_version = check.load_manifest_dir(bundled, engine_version=5)
            with self.assertRaisesRegex(check.CheckError, "same version"):
                check.validate_catalog(published, same_version, engine_version=5)

    def test_rejects_unlisted_published_manifest_lag_for_new_engine(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            bundled = root / "bundled"
            website = root / "website"
            bundled.mkdir()
            website.mkdir()
            bundled_content = manifest("codex", "2026.06.10.2").replace(
                "min_engine_version = 1", "min_engine_version = 2"
            )
            (bundled / "codex.toml").write_text(bundled_content)
            (website / "codex.toml").write_text(manifest("codex", "2026.06.10.1"))
            (website / "index.toml").write_text(catalog())

            bundled_manifests = check.load_manifest_dir(bundled, engine_version=2)
            with self.assertRaisesRegex(check.CheckError, "lower than bundled"):
                check.validate_catalog(website, bundled_manifests, engine_version=2)

    def test_rejects_same_version_content_drift(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            bundled = root / "bundled"
            website = root / "website"
            bundled.mkdir()
            website.mkdir()
            (bundled / "codex.toml").write_text(manifest("codex", "2026.06.10.1", "ready"))
            (website / "codex.toml").write_text(manifest("codex", "2026.06.10.1", "changed"))
            (website / "index.toml").write_text(catalog())

            bundled_manifests = check.load_manifest_dir(bundled, engine_version=1)
            with self.assertRaisesRegex(check.CheckError, "same version"):
                check.validate_catalog(website, bundled_manifests, engine_version=1)

    @patch.dict(check.UNPUBLISHED_BUNDLED_MANIFESTS, UNPUBLISHED_TEST_EXCEPTION, clear=True)
    def test_allows_exact_unpublished_bundled_manifest(self):
        with tempfile.TemporaryDirectory() as tmp:
            bundled, published = unpublished_manifest_dirs(Path(tmp))
            bundled_manifests = check.load_manifest_dir(bundled, engine_version=3)
            check.validate_catalog(
                published,
                bundled_manifests,
                engine_version=3,
                allow_unpublished=True,
            )

    @patch.dict(check.UNPUBLISHED_BUNDLED_MANIFESTS, UNPUBLISHED_TEST_EXCEPTION, clear=True)
    def test_release_gate_rejects_exact_unpublished_bundled_manifest(self):
        with tempfile.TemporaryDirectory() as tmp:
            bundled, published = unpublished_manifest_dirs(Path(tmp))
            bundled_manifests = check.load_manifest_dir(bundled, engine_version=3)
            with self.assertRaisesRegex(check.CheckError, "missing bundled agent"):
                check.validate_catalog(published, bundled_manifests, engine_version=3)

    @patch.dict(check.UNPUBLISHED_BUNDLED_MANIFESTS, UNPUBLISHED_TEST_EXCEPTION, clear=True)
    def test_rejects_mutated_unpublished_bundled_manifest(self):
        with tempfile.TemporaryDirectory() as tmp:
            bundled, published = unpublished_manifest_dirs(Path(tmp))
            with (bundled / "testagent.toml").open("a") as manifest_file:
                manifest_file.write("\n# unexpected mutation\n")
            bundled_manifests = check.load_manifest_dir(bundled, engine_version=3)
            with self.assertRaisesRegex(check.CheckError, "missing bundled agent"):
                check.validate_catalog(
                    published,
                    bundled_manifests,
                    engine_version=3,
                    allow_unpublished=True,
                )

    def test_rejects_unknown_catalog_agent(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            bundled = root / "bundled"
            website = root / "website"
            bundled.mkdir()
            website.mkdir()
            (bundled / "codex.toml").write_text(manifest("codex", "2026.06.10.1"))
            (website / "newagent.toml").write_text(manifest("newagent", "2026.06.10.1"))
            (website / "index.toml").write_text(catalog("newagent", "newagent.toml"))

            bundled_manifests = check.load_manifest_dir(bundled, engine_version=1)
            with self.assertRaisesRegex(check.CheckError, "unknown agent"):
                check.validate_catalog(website, bundled_manifests, engine_version=1)

    def test_rejects_manifest_requiring_newer_engine(self):
        with tempfile.TemporaryDirectory() as tmp:
            bundled = Path(tmp) / "bundled"
            bundled.mkdir()
            (bundled / "codex.toml").write_text(
                manifest("codex", "2026.06.10.1").replace(
                    "min_engine_version = 1", "min_engine_version = 2"
                )
            )

            with self.assertRaisesRegex(check.CheckError, "exceeds engine"):
                check.load_manifest_dir(bundled, engine_version=1)

    def test_rejects_top_non_empty_lines_below_engine_three(self):
        with tempfile.TemporaryDirectory() as tmp:
            bundled = Path(tmp) / "bundled"
            bundled.mkdir()
            content = manifest("codex", "2026.06.10.1").replace(
                'contains = ["ready"]',
                'region = "top_non_empty_lines(1)"\ncontains = ["ready"]',
            )
            (bundled / "codex.toml").write_text(content)

            with self.assertRaisesRegex(check.CheckError, "requires min_engine_version 3"):
                check.load_manifest_dir(bundled, engine_version=3)

    def test_top_non_empty_lines_requires_canonical_positive_bounded_count(self):
        base_rule = {
            "id": "test",
            "state": "working",
            "contains": ["ready"],
        }
        name = "top_non_empty_lines"
        for count in ("1", str(check.MAX_TOP_REGION_LINE_COUNT)):
            rule = {**base_rule, "region": f"{name}({count})"}
            check.validate_rule(Path("test.toml"), 0, rule, {"gates": 0, "matchers": 0})
        for count in (
            "0",
            "01",
            "+1",
            str(check.MAX_TOP_REGION_LINE_COUNT + 1),
            "9" * 40,
        ):
            rule = {**base_rule, "region": f"{name}({count})"}
            with self.subTest(region=rule["region"]):
                with self.assertRaisesRegex(check.CheckError, "invalid region"):
                    check.validate_rule(
                        Path("test.toml"), 0, rule, {"gates": 0, "matchers": 0}
                    )

    def test_input_only_manifest_rejects_unknown_nested_fields(self):
        content = '''id = "test"
version = "2026.09.25.1"
min_engine_version = 5
[composer]
region = "prompt_box_body"
[[input_rules]]
id = "choice"
kind = "select"
region = "above_prompt_box"
[[input_rules.any]]
contains = ["choose"]
'''
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "test.toml"
            path.write_text(content)
            self.assertEqual(check.validate_manifest(path, engine_version=5)["id"], "test")
            path.write_text(content.replace('kind = "select"', 'kind = "other"'))
            with self.assertRaisesRegex(check.CheckError, "invalid kind"):
                check.validate_manifest(path, engine_version=5)
            path.write_text(content.replace('contains = ["choose"]', 'contains = ["choose"]\nstate = "idle"'))
            with self.assertRaisesRegex(check.CheckError, "unknown gate field"):
                check.validate_manifest(path, engine_version=5)
            path.write_text(content.replace('region = "prompt_box_body"', 'region = "invalid"'))
            with self.assertRaisesRegex(check.CheckError, "composer has invalid region"):
                check.validate_manifest(path, engine_version=5)


    def test_shared_input_is_validated_without_weakening_agent_versions(self):
        shared = '''id = "shared"
min_engine_version = 4

[[input_rules]]
id = "choice"
kind = "unknown"
region = "bottom_non_empty_lines(4)"
contains = ["choose"]
'''
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            bundled = root / "bundled"
            published = root / "published"
            bundled.mkdir()
            published.mkdir()
            agent = manifest("codex", "2026.06.10.1")
            (bundled / "codex.toml").write_text(agent)
            shared_path = bundled / "shared-input.toml"
            shared_path.write_text(shared)
            (published / "codex.toml").write_text(agent)
            (published / "index.toml").write_text(catalog())

            bundled_agents = check.load_manifest_dir(bundled, engine_version=4, bundled=True)
            check.validate_catalog(published, bundled_agents, engine_version=4)

            shared_path.write_text(shared.replace('kind = "unknown"', 'kind = "unsupported"'))
            with self.assertRaisesRegex(check.CheckError, "invalid kind"):
                check.load_manifest_dir(bundled, engine_version=4, bundled=True)
            shared_path.write_text(shared)

            versionless_agent = agent.replace('version = "2026.06.10.1"\n', "")
            (published / "codex.toml").write_text(versionless_agent)
            with self.assertRaisesRegex(check.CheckError, "version must be dotted numeric"):
                check.validate_catalog(published, bundled_agents, engine_version=4)
            (bundled / "codex.toml").write_text(versionless_agent)
            with self.assertRaisesRegex(check.CheckError, "version must be dotted numeric"):
                check.load_manifest_dir(bundled, engine_version=4, bundled=True)


if __name__ == "__main__":
    unittest.main()
