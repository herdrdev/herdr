use serde::{Deserialize, Serialize};

use crate::cli::projection::{cli_params, Field};
use crate::cli::shepherd_bindings as bindings;

cli_params! {
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
    pub struct ShepherdTaskListParams => ShepherdTaskListProjection {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub root: Option<String> => Field::new(None).input(bindings::root_option()),
        #[serde(default)]
        pub archived: bool => Field::new(false)
            .input(bindings::boolean_flag("archived")),
    }
}

cli_params! {
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
    pub struct ShepherdTaskCreateParams => ShepherdTaskCreateProjection {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub root: Option<String> => Field::new(None).input(bindings::root_option()),
        pub title: String => Field::new(String::new()).input(bindings::text_position("title", "TITLE", 0)),
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub description: Option<String> => Field::new(None).input(bindings::optional_text("description", "TEXT")),
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        pub criteria: Vec<String> => Field::new(Vec::new()).input(bindings::string_list("criteria", "TEXT")),
        #[serde(default, alias = "attachments", skip_serializing_if = "Vec::is_empty")]
        pub links: Vec<String> => Field::new(Vec::new())
            .input(bindings::string_list("links", "URL")),
    }
}

cli_params! {
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
    pub struct ShepherdTaskArchiveParams => ShepherdTaskArchiveProjection {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub root: Option<String> => Field::new(None).input(bindings::root_option()),
        #[serde(deserialize_with = "deserialize_string_or_number")]
        pub id: String => Field::new(String::new())
            .input(bindings::text_position("id", "ID", 0)),
    }
}

fn deserialize_string_or_number<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    struct StringOrNumberVisitor;
    impl<'de> serde::de::Visitor<'de> for StringOrNumberVisitor {
        type Value = String;
        fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
            formatter.write_str("a string or number")
        }
        fn visit_str<E>(self, v: &str) -> Result<String, E> {
            Ok(v.to_string())
        }
        fn visit_string<E>(self, v: String) -> Result<String, E> {
            Ok(v)
        }
        fn visit_u64<E>(self, v: u64) -> Result<String, E> {
            Ok(v.to_string())
        }
        fn visit_i64<E>(self, v: i64) -> Result<String, E> {
            Ok(v.to_string())
        }
    }
    deserializer.deserialize_any(StringOrNumberVisitor)
}
