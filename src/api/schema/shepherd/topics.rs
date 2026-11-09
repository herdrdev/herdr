use serde::{Deserialize, Serialize};

use crate::cli::projection::{cli_params, Field};
use crate::cli::shepherd_bindings as bindings;

fn default_partitions() -> u32 {
    1
}

cli_params! {
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
    pub struct ShepherdTopicCreateParams => ShepherdTopicCreateProjection {
        pub name: String => Field::new(String::new()).input(bindings::text_position("name", "NAME", 0)),
        #[serde(default = "default_partitions")]
        pub partitions: u32 => Field::new(default_partitions())
            .input(bindings::u32_value("partitions", "N")),
    }
}

impl Default for ShepherdTopicCreateParams {
    fn default() -> Self {
        Self {
            name: String::new(),
            partitions: default_partitions(),
        }
    }
}

fn default_from() -> String {
    "user".to_string()
}

cli_params! {
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
    pub struct ShepherdTopicPublishParams => ShepherdTopicPublishProjection {
        pub topic: String => Field::new(String::new()).input(bindings::text_position("topic", "TOPIC", 0)),
        pub content: String => Field::new(String::new()).input(bindings::text_position("content", "CONTENT", 1)),
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub key: Option<String> => Field::new(None).input(bindings::optional_text("key", "KEY")),
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub partition: Option<u32> => Field::new(None).input(bindings::optional_u32("partition", "PARTITION")),
        #[serde(default = "default_from")]
        pub from: String => Field::new(default_from())
            .input(bindings::text_value("from", "SENDER")),
    }
}

impl Default for ShepherdTopicPublishParams {
    fn default() -> Self {
        Self {
            topic: String::new(),
            content: String::new(),
            key: None,
            partition: None,
            from: default_from(),
        }
    }
}

cli_params! {
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
    pub struct ShepherdTopicTailParams => ShepherdTopicTailProjection {
        pub topic: String => Field::new(String::new()).input(bindings::text_position("topic", "TOPIC", 0)),
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub partition: Option<u32> => Field::new(None).input(bindings::optional_u32("partition", "PARTITION")),
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub limit: Option<usize> => Field::new(None)
            .input(bindings::optional_usize("limit", "N")),
    }
}

cli_params! {
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
    pub struct ShepherdTopicReplayParams => ShepherdTopicReplayProjection {
        pub topic: String => Field::new(String::new()).input(bindings::text_position("topic", "TOPIC", 0)),
        #[serde(default, alias = "from")]
        pub from_offset: u64 => Field::new(0).input(bindings::u64_value("from", "OFFSET")),
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub partition: Option<u32> => Field::new(None).input(bindings::optional_u32("partition", "PARTITION")),
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub limit: Option<usize> => Field::new(None)
            .input(bindings::optional_usize("limit", "N")),
    }
}

cli_params! {
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
    pub struct ShepherdTopicGroupsParams => ShepherdTopicGroupsProjection {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub topic: Option<String> => Field::new(None)
            .input(bindings::optional_text_position("topic", "TOPIC", 0)),
    }
}
