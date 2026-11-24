use crate::api::schema::{ResponseResult, SidebarClearWidgetParams, SidebarReportWidgetParams};
use crate::app::App;

use super::super::api_helpers::{normalize_metadata_source, normalize_metadata_ttl};
use super::responses::{encode_error, encode_success};

impl App {
    pub(super) fn handle_sidebar_report_widget(
        &mut self,
        id: String,
        params: SidebarReportWidgetParams,
    ) -> String {
        let source = match normalize_metadata_source(params.source) {
            Ok(source) => source,
            Err(message) => return encode_error(id, "invalid_metadata_source", message),
        };
        let ttl = match normalize_metadata_ttl(params.ttl_ms) {
            Ok(ttl) => ttl,
            Err(message) => return encode_error(id, "invalid_metadata_ttl", message),
        };
        let changed = match self.state.sidebar_widgets.report(
            params.slot,
            source,
            params.title,
            params.right,
            params.lines,
            params.rows,
            params.action,
            ttl,
            std::time::Instant::now(),
        ) {
            Ok(changed) => changed,
            Err(message) => return encode_error(id, "invalid_sidebar_widget", message),
        };
        if changed {
            self.sync_agent_metadata_deadline();
        }
        encode_success(id, ResponseResult::Ok {})
    }

    pub(super) fn handle_sidebar_clear_widget(
        &mut self,
        id: String,
        params: SidebarClearWidgetParams,
    ) -> String {
        let source = match normalize_metadata_source(params.source) {
            Ok(source) => source,
            Err(message) => return encode_error(id, "invalid_metadata_source", message),
        };
        if self.state.sidebar_widgets.clear(params.slot, &source) {
            self.sync_agent_metadata_deadline();
        }
        encode_success(id, ResponseResult::Ok {})
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use crate::api::schema::{
        ErrorResponse, Method, Request, SidebarClearWidgetParams, SidebarReportWidgetParams,
        SidebarWidgetSlot, SuccessResponse,
    };
    use crate::config::Config;

    fn test_app() -> crate::app::App {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        crate::app::App::new(
            &Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            crate::api::EventHub::default(),
        )
    }

    #[test]
    fn sidebar_report_widget_stores_updates_expires_and_clears() {
        let mut app = test_app();

        let response = app.handle_api_request(Request {
            id: "rep-1".into(),
            method: Method::SidebarReportWidget(SidebarReportWidgetParams {
                source: "drover".into(),
                slot: SidebarWidgetSlot::Top,
                title: "drover".into(),
                right: Some("2/5".into()),
                lines: vec!["● 2 working".into(), "✓ 3 done".into()],
                rows: vec![],
                action: None,
                ttl_ms: Some(1_000),
            }),
        });
        let _: SuccessResponse = serde_json::from_str(&response).unwrap();

        let snapshot = app.session_snapshot();
        assert_eq!(snapshot.sidebar_widgets.len(), 1);
        assert_eq!(snapshot.sidebar_widgets[0].source, "drover");
        assert_eq!(snapshot.sidebar_widgets[0].title, "drover");
        assert_eq!(snapshot.sidebar_widgets[0].right.as_deref(), Some("2/5"));
        assert_eq!(
            snapshot.sidebar_widgets[0].lines,
            vec!["● 2 working", "✓ 3 done"]
        );

        let deadline = app
            .state
            .next_agent_metadata_expiry()
            .expect("ttl should schedule metadata expiry");
        app.expire_metadata_at(deadline, deadline + Duration::from_millis(1));
        assert!(app.session_snapshot().sidebar_widgets.is_empty());

        // Re-report without TTL and clear explicitly.
        let response = app.handle_api_request(Request {
            id: "rep-2".into(),
            method: Method::SidebarReportWidget(SidebarReportWidgetParams {
                source: "drover".into(),
                slot: SidebarWidgetSlot::Top,
                title: "drover".into(),
                right: None,
                lines: vec!["idle".into()],
                rows: vec![],
                action: None,
                ttl_ms: None,
            }),
        });
        let _: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(app.session_snapshot().sidebar_widgets.len(), 1);

        let clear = app.handle_api_request(Request {
            id: "clr-1".into(),
            method: Method::SidebarClearWidget(SidebarClearWidgetParams {
                source: "drover".into(),
                slot: SidebarWidgetSlot::Top,
            }),
        });
        let _: SuccessResponse = serde_json::from_str(&clear).unwrap();
        assert!(app.session_snapshot().sidebar_widgets.is_empty());
        let _ = Instant::now();
    }

    #[test]
    fn sidebar_report_widget_rejects_invalid_source_and_empty_title() {
        let mut app = test_app();

        let bad_source = app.handle_api_request(Request {
            id: "bad-src".into(),
            method: Method::SidebarReportWidget(SidebarReportWidgetParams {
                source: "bad source!".into(),
                slot: SidebarWidgetSlot::Top,
                title: "drover".into(),
                right: None,
                lines: vec![],
                rows: vec![],
                action: None,
                ttl_ms: None,
            }),
        });
        let err: ErrorResponse = serde_json::from_str(&bad_source).unwrap();
        assert_eq!(err.error.code, "invalid_metadata_source");

        let bad_title = app.handle_api_request(Request {
            id: "bad-title".into(),
            method: Method::SidebarReportWidget(SidebarReportWidgetParams {
                source: "drover".into(),
                slot: SidebarWidgetSlot::Top,
                title: "   ".into(),
                right: None,
                lines: vec![],
                rows: vec![],
                action: None,
                ttl_ms: None,
            }),
        });
        let err: ErrorResponse = serde_json::from_str(&bad_title).unwrap();
        assert_eq!(err.error.code, "invalid_sidebar_widget");
    }
}
