use serde::{Deserialize, Serialize};

use crate::cli::projection::{cli_params, Field};
use crate::cli::shepherd_bindings as bindings;

cli_params! {
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
    pub struct ShepherdPatternListParams => ShepherdPatternListProjection {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub root: Option<String> => Field::new(None)
            .input(bindings::root_option()),
    }
}

cli_params! {
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
    pub struct ShepherdPatternGetParams => ShepherdPatternGetProjection {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub root: Option<String> => Field::new(None).input(bindings::root_option()),
        pub name: String => Field::new(String::new())
            .input(bindings::text_position("name", "NAME", 0)),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
pub struct ShepherdPatternSaveParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root: Option<String>,
    pub pattern: shepherd_core::patterns::Pattern,
    #[serde(default)]
    pub global: bool,
}

cli_params! {
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
    pub struct ShepherdPatternDeleteParams => ShepherdPatternDeleteProjection {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub root: Option<String> => Field::new(None).input(bindings::root_option()),
        pub name: String => Field::new(String::new())
            .input(bindings::text_position("name", "NAME", 0)),
    }
}

cli_params! {
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
    pub struct ShepherdPatternValidateParams => ShepherdPatternValidateProjection {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub root: Option<String> => Field::new(None).input(bindings::root_option()),
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub name: Option<String> => Field::new(None).input(bindings::optional_text_position("name", "NAME", 0)),
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub pattern: Option<shepherd_core::patterns::Pattern> => Field::new(None::<shepherd_core::patterns::Pattern>),
    }
}
