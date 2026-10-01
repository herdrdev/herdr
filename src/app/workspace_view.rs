use crate::api::schema::WorkspaceViewSetParams;

use super::AppState;

const MAX_SOURCE_CHARS: usize = 120;
const MAX_LABEL_CHARS: usize = 32;
const MAX_WORKSPACE_IDS: usize = 64;
const MAX_WORKSPACE_ID_CHARS: usize = 120;

pub(crate) fn validate_workspace_view(spec: &mut WorkspaceViewSetParams) -> Result<(), String> {
    spec.source = normalize_source(&spec.source)?;
    spec.label = spec
        .label
        .take()
        .map(|label| normalize_label(&label))
        .transpose()?;
    spec.workspace_ids = normalize_workspace_ids(&spec.workspace_ids)?;
    Ok(())
}

pub(crate) fn validate_workspace_view_source(source: &str) -> Result<String, String> {
    normalize_source(source)
}

/// IDs presented in workspace chrome when a view is active.
///
/// `None` means no presentation filter (show all). Focused workspace is always
/// treated as presented by chrome helpers even when absent from this list.
pub(crate) fn presented_workspace_ids(app: &AppState) -> Option<Vec<String>> {
    app.workspace_view_override
        .as_ref()
        .map(|view| view.workspace_ids.clone())
}

fn normalize_source(source: &str) -> Result<String, String> {
    let source = source.trim();
    if source.is_empty()
        || source.chars().count() > MAX_SOURCE_CHARS
        || !source
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, ':' | '.' | '_' | '-'))
    {
        return Err(format!(
            "workspace view source must be non-empty, at most {MAX_SOURCE_CHARS} characters, and contain only ASCII letters, digits, colon, dot, underscore, or hyphen"
        ));
    }
    Ok(source.to_string())
}

fn normalize_label(label: &str) -> Result<String, String> {
    let label = label
        .trim()
        .chars()
        .filter(|ch| !ch.is_control())
        .collect::<String>();
    if label.is_empty() || label.chars().count() > MAX_LABEL_CHARS {
        return Err(format!(
            "workspace view label must be non-empty and at most {MAX_LABEL_CHARS} characters"
        ));
    }
    Ok(label)
}

fn normalize_workspace_ids(workspace_ids: &[String]) -> Result<Vec<String>, String> {
    if workspace_ids.len() > MAX_WORKSPACE_IDS {
        return Err(format!(
            "workspace view may contain at most {MAX_WORKSPACE_IDS} workspace ids"
        ));
    }
    let mut normalized = Vec::with_capacity(workspace_ids.len());
    for workspace_id in workspace_ids {
        let workspace_id = workspace_id.trim();
        if workspace_id.is_empty()
            || workspace_id.chars().count() > MAX_WORKSPACE_ID_CHARS
            || !workspace_id
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, ':' | '.' | '_' | '-'))
        {
            return Err(format!(
                "workspace view workspace_id must be non-empty, at most {MAX_WORKSPACE_ID_CHARS} characters, and contain only ASCII letters, digits, colon, dot, underscore, or hyphen"
            ));
        }
        if !normalized.iter().any(|existing| existing == workspace_id) {
            normalized.push(workspace_id.to_string());
        }
    }
    Ok(normalized)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_and_dedupes_workspace_ids() {
        let mut params = WorkspaceViewSetParams {
            source: "  plugin:example.zen  ".into(),
            label: Some("  zen\u{0007}  ".into()),
            workspace_ids: vec![
                " w1 ".into(),
                "w1".into(),
                "w3".into(),
                "w3".into(),
            ],
        };
        validate_workspace_view(&mut params).unwrap();
        assert_eq!(params.source, "plugin:example.zen");
        assert_eq!(params.label.as_deref(), Some("zen"));
        assert_eq!(params.workspace_ids, ["w1", "w3"]);
    }

    #[test]
    fn rejects_empty_source_and_oversized_lists() {
        let mut empty_source = WorkspaceViewSetParams {
            source: "   ".into(),
            label: None,
            workspace_ids: Vec::new(),
        };
        assert!(validate_workspace_view(&mut empty_source).is_err());

        let mut too_many = WorkspaceViewSetParams {
            source: "example.views".into(),
            label: None,
            workspace_ids: (0..=MAX_WORKSPACE_IDS)
                .map(|index| format!("w{index}"))
                .collect(),
        };
        assert!(validate_workspace_view(&mut too_many).is_err());
    }
}
