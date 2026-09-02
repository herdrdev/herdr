use crate::api::schema::{
    IntegrationInfo, IntegrationInstallResult, IntegrationState, IntegrationUninstallResult,
    ResponseResult,
};
use crate::app::App;

use super::responses::{encode_error, encode_success};

impl App {
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
        self.state.integration_recommendations = crate::integration::integration_recommendations();

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
        self.state.integration_recommendations = crate::integration::integration_recommendations();

        encode_success(
            id,
            ResponseResult::IntegrationUninstall {
                target,
                details: IntegrationUninstallResult { messages },
            },
        )
    }
}
