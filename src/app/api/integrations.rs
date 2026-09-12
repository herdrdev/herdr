use crate::api::schema::{
    IntegrationInfo, IntegrationInstallResult, IntegrationState, IntegrationUninstallResult,
    ResponseResult,
};
use crate::app::App;

use super::responses::{encode_error, encode_success};

impl App {
    pub(super) fn refresh_registry_integration_recommendations(&mut self) {
        crate::api::REGISTRY_PUBLICATION_WAKEUP.begin_refresh();
        let snapshot = crate::agents::store::snapshot();
        if refresh_recommendations_for_generation(
            &mut self.integration_registry_generation,
            &mut self.state.integration_recommendations,
            snapshot.generation,
            || crate::integration::integration_recommendations_with_registry(&snapshot),
        ) {
            self.render_dirty.request_generic();
            self.render_notify.notify_one();
        }
    }

    fn refresh_installed_integration_recommendations(&mut self) {
        let snapshot = crate::agents::store::snapshot();
        self.state.integration_recommendations =
            crate::integration::integration_recommendations_with_registry(&snapshot);
        self.integration_registry_generation = snapshot.generation;
    }

    pub(super) fn handle_integration_list(&self, id: String) -> String {
        let integrations = crate::integration::integration_recommendations()
            .into_iter()
            .map(|recommendation| IntegrationInfo {
                target: recommendation.target,
                label: recommendation.label.to_owned(),
                command: recommendation.command.to_owned(),
                available: recommendation.available,
                state: match recommendation.state {
                    crate::integration::IntegrationStatusKind::NotInstalled => {
                        IntegrationState::NotInstalled
                    }
                    crate::integration::IntegrationStatusKind::Current => IntegrationState::Current,
                    crate::integration::IntegrationStatusKind::Outdated => {
                        IntegrationState::Outdated
                    }
                },
            })
            .collect();
        encode_success(id, ResponseResult::IntegrationList { integrations })
    }

    pub(super) fn handle_integration_install(
        &mut self,
        id: String,
        params: crate::api::schema::IntegrationInstallParams,
    ) -> String {
        let target = params.target;
        let messages = match crate::integration::install_target(target) {
            Ok(messages) => messages,
            Err(err) => return encode_error(id, "integration_install_failed", err.to_string()),
        };
        self.refresh_installed_integration_recommendations();

        encode_success(
            id,
            ResponseResult::IntegrationInstall {
                target,
                details: IntegrationInstallResult { messages },
            },
        )
    }

    pub(super) fn handle_integration_uninstall(
        &mut self,
        id: String,
        params: crate::api::schema::IntegrationUninstallParams,
    ) -> String {
        let target = params.target;
        let messages = match crate::integration::uninstall_target(target) {
            Ok(messages) => messages,
            Err(err) => return encode_error(id, "integration_uninstall_failed", err.to_string()),
        };
        self.refresh_installed_integration_recommendations();

        encode_success(
            id,
            ResponseResult::IntegrationUninstall {
                target,
                details: IntegrationUninstallResult { messages },
            },
        )
    }
}

// Explicit publication maintenance only: never called from compute_view, render,
// or per-pane reconciliation. Obsolete/coalesced wakeups do no filesystem work.
fn refresh_recommendations_for_generation(
    observed: &mut u64,
    recommendations: &mut Vec<crate::integration::IntegrationRecommendation>,
    generation: u64,
    refresh: impl FnOnce() -> Vec<crate::integration::IntegrationRecommendation>,
) -> bool {
    if generation <= *observed {
        return false;
    }
    *recommendations = refresh();
    *observed = generation;
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::schema::IntegrationTarget;
    use crate::integration::{IntegrationRecommendation, IntegrationStatusKind};

    fn recommendation(label: &str) -> IntegrationRecommendation {
        IntegrationRecommendation {
            target: IntegrationTarget::Claude,
            label: label.into(),
            command: label.into(),
            available: true,
            path: "unused".into(),
            state: IntegrationStatusKind::Outdated,
        }
    }

    #[test]
    fn publication_refresh_uses_the_pinned_snapshot_registry_and_generation() {
        let snapshot = crate::agents::store::snapshot_for_test(
            vec![(
                "agents/claude/agent.toml".into(),
                "schema = 1\nid = 'claude'\nname = 'claude'\naliases = []\nstartable = true\n[launch]\nunix = 'claude'\nwindows = 'claude'\n".into(),
            )],
            42,
        ).unwrap();
        let mut observed = 41;
        let mut recommendations = vec![recommendation("old-label")];
        assert!(refresh_recommendations_for_generation(
            &mut observed,
            &mut recommendations,
            snapshot.generation,
            || crate::integration::integration_recommendations_with_registry(&snapshot),
        ));
        assert_eq!(observed, 42);
        // The pinned source removed integration metadata; the global bundled
        // registry must not supply its unrelated recommendation instead.
        assert!(recommendations.is_empty());
    }

    #[test]
    fn publication_refresh_relabels_and_removes_cached_recommendations_once_per_generation() {
        let mut observed = 1;
        let mut recommendations = vec![recommendation("old-label")];
        assert!(!refresh_recommendations_for_generation(
            &mut observed,
            &mut recommendations,
            1,
            || panic!("unchanged generation must not probe integration files"),
        ));
        assert!(refresh_recommendations_for_generation(
            &mut observed,
            &mut recommendations,
            3,
            || vec![recommendation("new-label")],
        ));
        assert_eq!(recommendations[0].label, "new-label");
        for stale in [2, 3] {
            assert!(!refresh_recommendations_for_generation(
                &mut observed,
                &mut recommendations,
                stale,
                || panic!("coalesced or stale wakeup must not refresh"),
            ));
        }
        assert!(refresh_recommendations_for_generation(
            &mut observed,
            &mut recommendations,
            4,
            Vec::new,
        ));
        let state = crate::app::state::AppState {
            integration_recommendations: recommendations,
            ..crate::app::state::AppState::test_new()
        };
        assert!(!state.integration_updates_available());
    }
}
