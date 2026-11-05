use serde::{Deserialize, Serialize};

fn default_partitions() -> u32 {
    1
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ShepherdTopicCreateParams {
    pub name: String,
    #[serde(default = "default_partitions")]
    pub partitions: u32,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ShepherdTopicPublishParams {
    pub topic: String,
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub partition: Option<u32>,
    #[serde(default = "default_from")]
    pub from: String,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
pub struct ShepherdTopicTailParams {
    pub topic: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub partition: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
pub struct ShepherdTopicReplayParams {
    pub topic: String,
    #[serde(default, alias = "from")]
    pub from_offset: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub partition: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
pub struct ShepherdTopicGroupsParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub topic: Option<String>,
}
