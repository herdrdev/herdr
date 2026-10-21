#[path = "tab_bindings.rs"]
mod bindings;

pub(crate) use bindings::command;

pub(super) fn run_tab_command(args: &[String]) -> std::io::Result<i32> {
    super::run_json_request(bindings::request(args))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use crate::api::schema::{TabCreateParams, TabListParams};

    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn simple_families_tab_rejects_invalid_grammar_before_transport() {
        for values in [
            vec![],
            vec!["unknown"],
            vec!["list", "--workspace"],
            vec!["create", "--cwd"],
            vec!["create", "--label"],
            vec!["create", "--env"],
            vec!["create", "--env", "NO_EQUALS"],
            vec!["create", "--focus=true"],
            vec!["get"],
            vec!["get", "2", "extra"],
            vec!["focus"],
            vec!["focus", "2", "extra"],
            vec!["rename", "2"],
            vec!["close"],
            vec!["close", "2", "extra"],
        ] {
            assert!(
                bindings::request(&args(&values)).is_err(),
                "argv: {values:?}"
            );
        }
    }

    #[test]
    fn simple_families_tab_dto_literal_defaults() {
        let create: TabCreateParams = serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(
            create,
            TabCreateParams {
                workspace_id: None,
                cwd: None,
                focus: false,
                label: None,
                env: HashMap::new(),
            }
        );
        assert_eq!(
            serde_json::to_value(create).unwrap(),
            serde_json::json!({"focus": false})
        );
        assert_eq!(
            serde_json::to_value(TabListParams::default()).unwrap(),
            serde_json::json!({})
        );
    }
}
