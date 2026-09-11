from __future__ import annotations

import json
import tempfile
import unittest
from pathlib import Path

from scripts.config_reference_check import (
    Model,
    StructField,
    augment_sound_override_fields,
    check,
    collect_entries,
    collect_keys,
    parse_file,
    parse_model,
    parse_sound_profile_keys,
    parse_sound_profile_source,
)


SAMPLE_MODEL = """
#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct Config {
    pub onboarding: Option<bool>,
    pub ui: UiConfig,
    pub keys: KeysConfig,
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct UiConfig {
    /// Sidebar width in columns. Default: 26.
    pub sidebar_width: u16,
    /// Host cursor policy. Default: auto.
    pub host_cursor: HostCursorModeConfig,
    /// Tab bar status entries.
    pub tab_bar_right: Vec<TabBarRightEntryConfig>,
    #[serde(rename = "accent_color")]
    pub accent: String,
    #[serde(skip)]
    pub internal_cache: usize,
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct KeysConfig {
    /// Prefix key. Default: "ctrl+b".
    pub prefix: String,
    pub zoom: BindingConfig,
    /// Prefix-mode custom command bindings.
    pub command: Vec<CommandKeybindConfig>,
    pub(crate) user_fields: BTreeSet<&'static str>,
}

#[derive(Debug, Deserialize)]
pub struct CommandKeybindConfig {
    pub key: String,
    pub command: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HostCursorModeConfig {
    Auto,
    NativeCursor,
    Drawn,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TabBarRightEntryConfig {
    Hostname,
    Datetime {
        format: String,
    },
    Command {
        command: String,
        interval_seconds: u64,
    },
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum BindingConfig {
    One(String),
    Many(Vec<String>),
}
"""

SOUND_MODEL = """
pub struct Config {
    pub sound: SoundConfig,
}
pub struct SoundConfig {
    pub agents: AgentSoundOverrides,
}
pub struct AgentSoundOverrides {
}
pub enum AgentSoundSetting {
    Default,
    On,
    Off,
}
"""


def sample_model() -> Model:
    model = Model()
    parse_file(SAMPLE_MODEL, model)
    return model


class SoundProfileSourceTests(unittest.TestCase):
    def test_extracts_literal_keys_and_augments_sound_override_entries(self) -> None:
        sound_keys = parse_sound_profile_keys(
            'schema = 1\nid = "claude"\n[sound]\nkey = "claude"\ndefault = "default"\n'
        ) + parse_sound_profile_keys(
            'sound = { key = "github_copilot", default = "default" }\n'
        )
        self.assertEqual(sound_keys, ["claude", "github_copilot"])

        model = Model(
            structs={
                "Config": [StructField("ui", "UiConfig", "")],
                "UiConfig": [StructField("sound", "SoundConfig", "")],
                "SoundConfig": [
                    StructField("agents", "AgentSoundOverrides", "")
                ],
                "AgentSoundOverrides": [],
            },
            enums={"AgentSoundSetting": ["default", "on", "off"]},
        )
        augment_sound_override_fields(model, sound_keys)
        entries = {entry["key"]: entry for entry in collect_entries(model)}

        self.assertEqual(
            entries["ui.sound.agents.claude"],
            {
                "key": "ui.sound.agents.claude",
                "rust_type": "AgentSoundSetting",
                "doc": "",
                "values": ["default", "on", "off"],
            },
        )
        self.assertIn("ui.sound.agents.github_copilot", entries)

    def test_accepts_toml_whitespace_and_literal_strings(self) -> None:
        catalog = "  [ sound ]\n key = 'pi' # comment\n default = 'off'\n"
        self.assertEqual(parse_sound_profile_keys(catalog), ["pi"])

    def test_ignores_comments_and_unrelated_keys(self) -> None:
        catalog = (
            '# sound = { key = "comment" }\n'
            'example = \'sound = { key = "example" }\'\n'
            '[other]\nkey = "unrelated"\n'
        )
        self.assertEqual(parse_sound_profile_keys(catalog), [])

    def test_rejects_missing_or_nonstring_sound_key(self) -> None:
        for catalog in ('[sound]\ndefault = "off"', '[sound]\nkey = 42', 'sound = "pi"'):
            with self.subTest(catalog=catalog), self.assertRaisesRegex(ValueError, "string config key"):
                parse_sound_profile_keys(catalog)

    def test_rejects_invalid_or_duplicate_toml_fields(self) -> None:
        for catalog in ('[sound', '[sound]\nkey = "pi"\nkey = "other"'):
            with self.subTest(catalog=catalog), self.assertRaises(ValueError):
                parse_sound_profile_keys(catalog)

    def test_rejects_empty_sound_profile_keys(self) -> None:
        for key in ("", "  "):
            with self.subTest(key=key), self.assertRaisesRegex(ValueError, "must not be empty"):
                parse_sound_profile_keys(f'[sound]\nkey = "{key}"')

    def test_discovers_sorted_vendored_packages_ignoring_nonpackage_files(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            src = Path(tmp) / "src"
            config_root = src / "config"
            agents_root = Path(tmp) / "vendor" / "agent-registry" / "agents"
            config_root.mkdir(parents=True)
            (agents_root / "zeta").mkdir(parents=True)
            (agents_root / "alpha").mkdir()
            (agents_root / "tests").mkdir()
            (config_root / "model.rs").write_text(SOUND_MODEL, encoding="utf-8")
            (agents_root / "zeta" / "agent.toml").write_text(
                '[sound]\nkey = "zeta"\n',
                encoding="utf-8",
            )
            (agents_root / "alpha" / "agent.toml").write_text(
                '[sound]\nkey = "alpha"\n',
                encoding="utf-8",
            )
            (agents_root / "agent.toml").write_text(
                '[sound]\nkey = "shared"\n',
                encoding="utf-8",
            )
            (agents_root / "tests" / "example.toml").write_text(
                '[sound]\nkey = "test_only"\n',
                encoding="utf-8",
            )

            model = parse_model(sorted(config_root.glob("*.rs")))
            sound_entries = [
                entry["key"]
                for entry in collect_entries(model)
                if entry["key"].startswith("sound.agents.")
            ]

        self.assertEqual(
            sound_entries,
            ["sound.agents.alpha", "sound.agents.zeta"],
        )

    def test_single_profile_fixture_remains_a_supported_override(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            model_path = root / "model.rs"
            fixture = root / "agent.toml"
            model_path.write_text(SOUND_MODEL, encoding="utf-8")
            fixture.write_text(
                '[sound]\nkey = "fixture"\n',
                encoding="utf-8",
            )

            model = parse_model([model_path], agent_catalog=fixture)

        self.assertIn("sound.agents.fixture", collect_keys(model))

    def test_rejects_duplicate_keys_across_profile_files(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            agents_root = Path(tmp) / "agents"
            (agents_root / "alpha").mkdir(parents=True)
            (agents_root / "zeta").mkdir()
            for name in ["alpha", "zeta"]:
                (agents_root / name / "agent.toml").write_text(
                    '[sound]\nkey = "duplicate"\n',
                    encoding="utf-8",
                )

            with self.assertRaisesRegex(ValueError, "duplicate.*duplicate"):
                parse_sound_profile_source(agents_root)


class CollectKeysTests(unittest.TestCase):
    def test_walks_nested_structs_into_dotted_keys(self) -> None:
        keys = collect_keys(sample_model())

        self.assertIn("onboarding", keys)
        self.assertIn("ui.sidebar_width", keys)
        self.assertIn("keys.prefix", keys)
        self.assertIn("keys.zoom", keys)

    def test_serde_rename_wins_over_field_name(self) -> None:
        keys = collect_keys(sample_model())

        self.assertIn("ui.accent_color", keys)
        self.assertNotIn("ui.accent", keys)

    def test_skips_serde_skip_and_private_fields(self) -> None:
        keys = collect_keys(sample_model())

        self.assertNotIn("ui.internal_cache", keys)
        self.assertNotIn("keys.user_fields", keys)

    def test_skips_listed_vec_of_struct_subtrees(self) -> None:
        keys = collect_keys(sample_model())

        self.assertNotIn("keys.command", keys)
        self.assertNotIn("keys.command.key", keys)

    def test_unlisted_vec_of_struct_subtree_is_an_error(self) -> None:
        model = sample_model()
        parse_file(
            "pub struct ExtraConfig {\n"
            "    pub items: Vec<CommandKeybindConfig>,\n"
            "}\n",
            model,
        )
        model.structs["Config"].append(
            StructField(name="extra", rust_type="ExtraConfig", doc="")
        )

        with self.assertRaises(ValueError) as raised:
            collect_keys(model)

        self.assertIn("extra.items", str(raised.exception))
        self.assertIn("SKIPPED_SUBTREES", str(raised.exception))

    def test_enum_values_respect_rename_all_and_untagged_enums_have_none(self) -> None:
        entries = {entry["key"]: entry for entry in collect_entries(sample_model())}

        self.assertEqual(
            entries["ui.host_cursor"]["values"], ["auto", "native-cursor", "drawn"]
        )
        self.assertEqual(
            entries["ui.tab_bar_right"]["values"],
            ["hostname", "datetime", "command"],
        )
        self.assertNotIn("values", entries["keys.zoom"])


class CheckTests(unittest.TestCase):
    def run_check(
        self,
        documented_keys: list[str],
        *,
        value_overrides: dict[str, list[str]] | None = None,
    ) -> list[str]:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            model_root = root / "config"
            model_root.mkdir()
            (model_root / "model.rs").write_text(SAMPLE_MODEL, encoding="utf-8")

            model_entries = {entry["key"]: entry for entry in collect_entries(sample_model())}
            entries = []
            for key in documented_keys:
                entry = {"key": key}
                if key in model_entries and "values" in model_entries[key]:
                    entry["values"] = model_entries[key]["values"]
                if value_overrides and key in value_overrides:
                    entry["values"] = value_overrides[key]
                entries.append(entry)

            reference = root / "config-reference.json"
            reference.write_text(
                json.dumps(
                    {
                        "sections": [
                            {
                                "id": "all",
                                "title": "All",
                                "keys": entries,
                            }
                        ]
                    }
                ),
                encoding="utf-8",
            )
            return check(model_root, reference)

    def all_keys(self) -> list[str]:
        return sorted(collect_keys(sample_model()))

    def test_in_sync_reference_passes(self) -> None:
        self.assertEqual(self.run_check(self.all_keys()), [])

    def test_missing_key_is_named(self) -> None:
        documented = [key for key in self.all_keys() if key != "ui.sidebar_width"]

        errors = self.run_check(documented)

        self.assertEqual(len(errors), 1)
        self.assertIn("ui.sidebar_width", errors[0])
        self.assertIn("missing", errors[0])

    def test_stale_key_is_named(self) -> None:
        errors = self.run_check(self.all_keys() + ["ui.removed_option"])

        self.assertEqual(len(errors), 1)
        self.assertIn("ui.removed_option", errors[0])
        self.assertIn("not in src/config", errors[0])

    def test_swapped_key_fails_despite_equal_count(self) -> None:
        documented = [
            "ui.renamed_option" if key == "ui.sidebar_width" else key
            for key in self.all_keys()
        ]

        errors = self.run_check(documented)

        self.assertEqual(len(errors), 2)

    def test_duplicate_key_is_rejected(self) -> None:
        errors = self.run_check(self.all_keys() + ["ui.sidebar_width"])

        self.assertEqual(len(errors), 1)
        self.assertIn("duplicated", errors[0])

    def test_changed_enum_values_are_rejected(self) -> None:
        errors = self.run_check(
            self.all_keys(),
            value_overrides={"ui.host_cursor": ["auto", "native"]},
        )

        self.assertEqual(len(errors), 1)
        self.assertIn("ui.host_cursor", errors[0])
        self.assertIn("allowed values", errors[0])


class RealModelTests(unittest.TestCase):
    def test_real_config_model_parses_and_yields_keys(self) -> None:
        model = parse_model(sorted(Path("src/config").glob("*.rs")))
        keys = collect_keys(model)

        self.assertGreater(len(keys), 100)
        self.assertIn("keys.prefix", keys)
        self.assertIn("ui.sound.agents.claude", keys)
        self.assertNotIn("keys.command", keys)

    def test_preview_reference_matches_real_config_model(self) -> None:
        self.assertEqual(
            check(
                Path("src/config"),
                Path("docs/next/website/src/data/config-reference.json"),
            ),
            [],
        )


if __name__ == "__main__":
    unittest.main()
