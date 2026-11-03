use std::io;

use super::registry::{integration_target_label, integration_target_supported};
use super::targets::{
    install_antigravity_cli, install_claude, install_codex, install_copilot, install_cursor,
    install_devin, install_droid, install_grok, install_hermes, install_kilo, install_kimi,
    install_letta, install_mastracode, install_omp, install_opencode, install_pi, install_qodercli,
    install_qwen, uninstall_antigravity_cli, uninstall_claude, uninstall_codex, uninstall_copilot,
    uninstall_cursor, uninstall_devin, uninstall_droid, uninstall_grok, uninstall_hermes,
    uninstall_kilo, uninstall_kimi, uninstall_letta, uninstall_mastracode, uninstall_omp,
    uninstall_opencode, uninstall_pi, uninstall_qodercli, uninstall_qwen,
};
use super::version::{agent_version_requirement, enforce_agent_version};
use super::{KIMI_MIN_VERSION, PI_EXTENSION_INSTALL_NAME};

pub(crate) fn install_target(
    target: crate::api::schema::IntegrationTarget,
) -> io::Result<Vec<String>> {
    let result = install_target_inner(target);
    let outcome = if result.is_ok() { "ok" } else { "error" };
    crate::logging::integration_action("install", integration_target_label(target), outcome);
    result
}

/// Experimental Letta install that bypasses the frozen client endpoint
/// `IntegrationTarget` enum. Fold into the agent registry when it lands.
pub(crate) fn install_experimental_letta() -> io::Result<Vec<String>> {
    let result = install_letta().map(|installed| {
        vec![
            format!(
                "Instalado: gancho de integração do Letta em {}",
                installed.hook_path.display()
            ),
            format!(
                "Verificado: configurações do Letta em {}",
                installed.settings_path.display()
            ),
        ]
    });
    let outcome = if result.is_ok() { "ok" } else { "error" };
    crate::logging::integration_action("install", "letta", outcome);
    result
}

/// Experimental Letta uninstall counterpart.
pub(crate) fn uninstall_experimental_letta() -> io::Result<Vec<String>> {
    let result = uninstall_letta().map(|result| {
        let mut messages = Vec::new();
        if result.removed_hook_file {
            messages.push(format!(
                "Removido: gancho do Letta em {}",
                result.hook_path.display()
            ));
        } else {
            messages.push(format!(
                "Não encontrado: gancho do Letta em {}",
                result.hook_path.display()
            ));
        }
        if result.updated_settings {
            messages.push(format!(
                "Entrada de gancho do Herdr para Letta removida em {}",
                result.settings_path.display()
            ));
        } else {
            messages.push(format!(
                "Nenhuma entrada de gancho do Herdr para Letta encontrada em {}",
                result.settings_path.display()
            ));
        }
        messages
    });
    let outcome = if result.is_ok() { "ok" } else { "error" };
    crate::logging::integration_action("uninstall", "letta", outcome);
    result
}

fn install_target_inner(target: crate::api::schema::IntegrationTarget) -> io::Result<Vec<String>> {
    if !integration_target_supported(target) {
        return Err(io::Error::other(format!(
            "A integração {} não é suportada no Windows",
            integration_target_label(target)
        )));
    }

    let version_warning = match agent_version_requirement(target) {
        Some(requirement) => enforce_agent_version(&requirement)?,
        None => None,
    };

    let mut messages = match target {
        crate::api::schema::IntegrationTarget::Pi => {
            let path = install_pi()?;
            vec![format!("Instalado: integração do Pi em {}", path.display())]
        }
        crate::api::schema::IntegrationTarget::Omp => {
            let installed = install_omp()?;
            let mut messages = Vec::new();
            if installed.removed_legacy_pi_extension {
                messages.push(format!(
                    "Integração antiga do pi removida do diretório de extensão do OMP em {}",
                    installed
                        .extension_path
                        .with_file_name(PI_EXTENSION_INSTALL_NAME)
                        .display()
                ));
            }
            messages.push(format!(
                "Instalado: integração do OMP em {}",
                installed.extension_path.display()
            ));
            messages
        }
        crate::api::schema::IntegrationTarget::Claude => {
            let installed = install_claude()?;
            vec![
                format!(
                    "Instalado: gancho de integração do Claude em {}",
                    installed.hook_path.display()
                ),
                format!(
                    "Verificado: configurações do Claude em {}",
                    installed.settings_path.display()
                ),
            ]
        }
        crate::api::schema::IntegrationTarget::Codex => {
            let installed = install_codex()?;
            vec![
                format!(
                    "Instalado: gancho de integração do Codex em {}",
                    installed.hook_path.display()
                ),
                format!(
                    "Verificado: ganchos do Codex em {}",
                    installed.hooks_path.display()
                ),
                format!(
                    "Verificado: configuração do Codex em {}",
                    installed.config_path.display()
                ),
            ]
        }
        crate::api::schema::IntegrationTarget::Copilot => {
            let installed = install_copilot()?;
            vec![
                format!(
                    "Instalado: gancho de integração do Copilot em {}",
                    installed.hook_path.display()
                ),
                format!(
                    "Verificado: configurações do Copilot em {}",
                    installed.settings_path.display()
                ),
            ]
        }
        crate::api::schema::IntegrationTarget::Devin => {
            let installed = install_devin()?;
            vec![
                format!(
                    "Instalado: gancho de integração do Devin em {}",
                    installed.hook_path.display()
                ),
                format!(
                    "Verificado: configurações do Devin em {}",
                    installed.settings_path.display()
                ),
            ]
        }
        crate::api::schema::IntegrationTarget::Kimi => {
            let installed = install_kimi()?;
            vec![
                format!(
                    "Instalado: gancho de integração do Kimi em {}",
                    installed.hook_path.display()
                ),
                format!(
                    "Verificado: configuração do Kimi em {}",
                    installed.config_path.display()
                ),
                format!("Exige kimi code {KIMI_MIN_VERSION} ou mais recente"),
            ]
        }
        crate::api::schema::IntegrationTarget::Droid => {
            let installed = install_droid()?;
            let mut messages = vec![
                format!(
                    "Instalado: gancho de integração do Droid em {}",
                    installed.hook_path.display()
                ),
                format!(
                    "Verificado: ganchos do Droid em {}",
                    installed.settings_path.display()
                ),
            ];
            if installed.updated_legacy_hooks {
                messages.push(format!(
                    "Entradas de gancho antigo do Herdr para Droid removidas em {}",
                    installed.hooks_path.display()
                ));
            }
            messages
        }
        crate::api::schema::IntegrationTarget::Opencode => {
            let installed = install_opencode()?;
            let mut messages = vec![
                format!(
                    "Instalado: plugin de integração do OpenCode em {}",
                    installed.plugin_path.display()
                ),
                format!(
                    "Plugin de integração da interface do OpenCode instalado em {}",
                    installed.tui_plugin_path.display()
                ),
                format!(
                    "Configuração do plugin da interface do OpenCode verificada em {}",
                    installed.tui_config_path.display()
                ),
            ];
            if installed.cli_config_path.is_none() {
                messages.push(
                    "Para ativar OpenCode V2, inicie opencode2 uma vez e reinstale esta integração"
                        .to_string(),
                );
            }
            messages
        }
        crate::api::schema::IntegrationTarget::Kilo => {
            let installed = install_kilo()?;
            vec![format!(
                "Instalado: plugin de integração do Kilo em {}",
                installed.plugin_path.display()
            )]
        }
        crate::api::schema::IntegrationTarget::Hermes => {
            let installed = install_hermes()?;
            vec![
                format!(
                    "Instalado: plugin de integração do Hermes em {}",
                    installed.plugin_dir.display()
                ),
                format!(
                    "Ativado: plugin do Hermes em {}",
                    installed.config_path.display()
                ),
            ]
        }
        crate::api::schema::IntegrationTarget::Qodercli => {
            let installed = install_qodercli()?;
            vec![
                format!(
                    "Instalado: gancho de integração do Qoder CLI em {}",
                    installed.hook_path.display()
                ),
                format!(
                    "Verificado: configurações do Qoder CLI em {}",
                    installed.settings_path.display()
                ),
            ]
        }
        crate::api::schema::IntegrationTarget::Qwen => {
            let installed = install_qwen()?;
            vec![
                format!(
                    "Instalado: gancho de integração do Qwen em {}",
                    installed.hook_path.display()
                ),
                format!(
                    "Verificado: configurações do Qwen em {}",
                    installed.settings_path.display()
                ),
            ]
        }
        crate::api::schema::IntegrationTarget::Cursor => {
            let installed = install_cursor()?;
            vec![
                format!(
                    "Instalado: gancho de integração do Cursor em {}",
                    installed.hook_path.display()
                ),
                format!(
                    "Atualizado: ganchos do Cursor em {}",
                    installed.hooks_path.display()
                ),
            ]
        }
        crate::api::schema::IntegrationTarget::Mastracode => {
            let installed = install_mastracode()?;
            vec![
                format!(
                    "Instalado: gancho de integração do Mastra Code em {}",
                    installed.hook_path.display()
                ),
                format!(
                    "Verificado: ganchos do Mastra Code em {}",
                    installed.hooks_path.display()
                ),
            ]
        }
        crate::api::schema::IntegrationTarget::AntigravityCli => {
            let installed = install_antigravity_cli()?;
            vec![
                format!(
                    "Instalado: gancho de integração do antigravity-cli em {}",
                    installed.hook_path.display()
                ),
                format!(
                    "Verificado: ganchos do antigravity-cli em {}",
                    installed.hooks_path.display()
                ),
            ]
        }
        crate::api::schema::IntegrationTarget::Grok => {
            let installed = install_grok()?;
            vec![
                format!(
                    "Instalado: gancho de integração do Grok em {}",
                    installed.hook_path.display()
                ),
                format!(
                    "Registrado: configuração de gancho do Grok em {}",
                    installed.config_path.display()
                ),
            ]
        }
    };

    if let Some(warning) = version_warning {
        messages.push(warning);
    }

    Ok(messages)
}

pub(crate) fn uninstall_target(
    target: crate::api::schema::IntegrationTarget,
) -> io::Result<Vec<String>> {
    let messages = match target {
        crate::api::schema::IntegrationTarget::Pi => {
            let result = uninstall_pi()?;
            if result.removed_extension {
                vec![format!(
                    "Removido: extensão de integração do Pi em {}",
                    result.extension_path.display()
                )]
            } else {
                vec![format!(
                    "Não encontrado: extensão de integração do Pi em {}",
                    result.extension_path.display()
                )]
            }
        }
        crate::api::schema::IntegrationTarget::Omp => {
            let result = uninstall_omp()?;
            if result.removed_extension {
                vec![format!(
                    "Removido: extensão de integração do OMP em {}",
                    result.extension_path.display()
                )]
            } else {
                vec![format!(
                    "Não encontrado: extensão de integração do OMP em {}",
                    result.extension_path.display()
                )]
            }
        }
        crate::api::schema::IntegrationTarget::Claude => {
            let result = uninstall_claude()?;
            let mut messages = Vec::new();
            if result.removed_hook_file {
                messages.push(format!(
                    "Removido: gancho do Claude em {}",
                    result.hook_path.display()
                ));
            } else {
                messages.push(format!(
                    "Não encontrado: gancho do Claude em {}",
                    result.hook_path.display()
                ));
            }
            if result.updated_settings {
                messages.push(format!(
                    "Entradas de gancho do Herdr para Claude removidas em {}",
                    result.settings_path.display()
                ));
            } else {
                messages.push(format!(
                    "Nenhuma entrada de gancho do Herdr para Claude encontrada em {}",
                    result.settings_path.display()
                ));
            }
            messages
        }
        crate::api::schema::IntegrationTarget::Codex => {
            let result = uninstall_codex()?;
            let mut messages = Vec::new();
            if result.removed_hook_file {
                messages.push(format!(
                    "Removido: gancho do Codex em {}",
                    result.hook_path.display()
                ));
            } else {
                messages.push(format!(
                    "Não encontrado: gancho do Codex em {}",
                    result.hook_path.display()
                ));
            }
            if result.updated_hooks {
                messages.push(format!(
                    "Entradas de gancho do Herdr para Codex removidas em {}",
                    result.hooks_path.display()
                ));
            } else {
                messages.push(format!(
                    "Nenhuma entrada de gancho do Herdr para Codex encontrada em {}",
                    result.hooks_path.display()
                ));
            }
            messages.push(format!(
                "Mantido sem alteração: configuração do Codex em {}",
                result.config_path.display()
            ));
            messages
        }
        crate::api::schema::IntegrationTarget::Copilot => {
            let result = uninstall_copilot()?;
            let mut messages = Vec::new();
            if result.removed_hook_file {
                messages.push(format!(
                    "Removido: gancho do Copilot em {}",
                    result.hook_path.display()
                ));
            } else {
                messages.push(format!(
                    "Não encontrado: gancho do Copilot em {}",
                    result.hook_path.display()
                ));
            }
            if result.updated_settings {
                messages.push(format!(
                    "Entradas de gancho do Herdr para Copilot removidas em {}",
                    result.settings_path.display()
                ));
            } else {
                messages.push(format!(
                    "Nenhuma entrada de gancho do Herdr para Copilot encontrada em {}",
                    result.settings_path.display()
                ));
            }
            messages
        }
        crate::api::schema::IntegrationTarget::Devin => {
            let result = uninstall_devin()?;
            let mut messages = Vec::new();
            if result.removed_hook_file {
                messages.push(format!(
                    "Removido: gancho do Devin em {}",
                    result.hook_path.display()
                ));
            } else {
                messages.push(format!(
                    "Não encontrado: gancho do Devin em {}",
                    result.hook_path.display()
                ));
            }
            if result.updated_settings {
                messages.push(format!(
                    "Entradas de gancho do Herdr para Devin removidas em {}",
                    result.settings_path.display()
                ));
            } else {
                messages.push(format!(
                    "Nenhuma entrada de gancho do Herdr para Devin encontrada em {}",
                    result.settings_path.display()
                ));
            }
            messages
        }
        crate::api::schema::IntegrationTarget::Kimi => {
            let result = uninstall_kimi()?;
            let mut messages = Vec::new();
            if result.removed_hook_file {
                messages.push(format!(
                    "Removido: gancho do Kimi em {}",
                    result.hook_path.display()
                ));
            } else {
                messages.push(format!(
                    "Não encontrado: gancho do Kimi em {}",
                    result.hook_path.display()
                ));
            }
            if result.updated_config {
                messages.push(format!(
                    "Entradas de gancho do Herdr para Kimi removidas em {}",
                    result.config_path.display()
                ));
            } else {
                messages.push(format!(
                    "Nenhuma entrada de gancho do Herdr para Kimi encontrada em {}",
                    result.config_path.display()
                ));
            }
            messages
        }
        crate::api::schema::IntegrationTarget::Droid => {
            let result = uninstall_droid()?;
            let mut messages = Vec::new();
            if result.removed_hook_file {
                messages.push(format!(
                    "Removido: gancho do Droid em {}",
                    result.hook_path.display()
                ));
            } else {
                messages.push(format!(
                    "Não encontrado: gancho do Droid em {}",
                    result.hook_path.display()
                ));
            }
            if result.updated_hooks {
                messages.push(format!(
                    "Entradas de gancho antigo do Herdr para Droid removidas em {}",
                    result.hooks_path.display()
                ));
            } else {
                messages.push(format!(
                    "Nenhuma entrada de gancho antigo do Herdr para Droid encontrada em {}",
                    result.hooks_path.display()
                ));
            }
            if result.updated_settings {
                messages.push(format!(
                    "Entradas de gancho do Herdr para Droid removidas em {}",
                    result.settings_path.display()
                ));
            } else {
                messages.push(format!(
                    "Nenhuma entrada de gancho do Herdr para Droid encontrada em {}",
                    result.settings_path.display()
                ));
            }
            messages
        }
        crate::api::schema::IntegrationTarget::Opencode => {
            let result = uninstall_opencode()?;
            let mut messages = vec![if result.removed_plugin {
                format!(
                    "Removido: plugin de integração do OpenCode em {}",
                    result.plugin_path.display()
                )
            } else {
                format!(
                    "Não encontrado: plugin de integração do OpenCode em {}",
                    result.plugin_path.display()
                )
            }];
            messages.push(if result.removed_tui_plugin {
                format!(
                    "Plugin de integração da interface do OpenCode removido em {}",
                    result.tui_plugin_path.display()
                )
            } else {
                format!(
                    "Plugin de integração da interface do OpenCode não encontrado em {}",
                    result.tui_plugin_path.display()
                )
            });
            for path in result.updated_tui_configs {
                messages.push(format!(
                    "Entrada do plugin Herdr removida da configuração OpenCode em {}",
                    path.display()
                ));
            }
            messages
        }
        crate::api::schema::IntegrationTarget::Kilo => {
            let result = uninstall_kilo()?;
            if result.removed_plugin {
                vec![format!(
                    "Removido: plugin de integração do Kilo em {}",
                    result.plugin_path.display()
                )]
            } else {
                vec![format!(
                    "Não encontrado: plugin de integração do Kilo em {}",
                    result.plugin_path.display()
                )]
            }
        }
        crate::api::schema::IntegrationTarget::Hermes => {
            let result = uninstall_hermes()?;
            let mut messages = Vec::new();
            if result.removed_plugin_dir {
                messages.push(format!(
                    "Removido: plugin de integração do Hermes em {}",
                    result.plugin_dir.display()
                ));
            } else {
                messages.push(format!(
                    "Não encontrado: plugin de integração do Hermes em {}",
                    result.plugin_dir.display()
                ));
            }
            if result.updated_config {
                messages.push(format!(
                    "Desativado: plugin do Hermes em {}",
                    result.config_path.display()
                ));
            } else {
                messages.push(format!(
                    "Nenhuma entrada do plugin Hermes encontrada em {}",
                    result.config_path.display()
                ));
            }
            messages
        }
        crate::api::schema::IntegrationTarget::Qodercli => {
            let result = uninstall_qodercli()?;
            let mut messages = Vec::new();
            if result.removed_hook_file {
                messages.push(format!(
                    "Removido: gancho do Qoder CLI em {}",
                    result.hook_path.display()
                ));
            } else {
                messages.push(format!(
                    "Não encontrado: gancho do Qoder CLI em {}",
                    result.hook_path.display()
                ));
            }
            if result.updated_settings {
                messages.push(format!(
                    "Entradas de gancho do Herdr para Qoder CLI removidas em {}",
                    result.settings_path.display()
                ));
            } else {
                messages.push(format!(
                    "Nenhuma entrada de gancho do Herdr para Qoder CLI encontrada em {}",
                    result.settings_path.display()
                ));
            }
            messages
        }
        crate::api::schema::IntegrationTarget::Qwen => {
            let result = uninstall_qwen()?;
            let mut messages = Vec::new();
            if result.removed_hook_file {
                messages.push(format!(
                    "Removido: gancho do Qwen em {}",
                    result.hook_path.display()
                ));
            } else {
                messages.push(format!(
                    "Não encontrado: gancho do Qwen em {}",
                    result.hook_path.display()
                ));
            }
            if result.updated_settings {
                messages.push(format!(
                    "Entradas de gancho do Herdr para Qwen removidas em {}",
                    result.settings_path.display()
                ));
            } else {
                messages.push(format!(
                    "Nenhuma entrada de gancho do Herdr para Qwen encontrada em {}",
                    result.settings_path.display()
                ));
            }
            messages
        }
        crate::api::schema::IntegrationTarget::Cursor => {
            let result = uninstall_cursor()?;
            let mut messages = Vec::new();
            if result.removed_hook_file {
                messages.push(format!(
                    "Removido: gancho do Cursor em {}",
                    result.hook_path.display()
                ));
            } else {
                messages.push(format!(
                    "Não encontrado: gancho do Cursor em {}",
                    result.hook_path.display()
                ));
            }
            if result.updated_hooks {
                messages.push(format!(
                    "Entradas de gancho do Herdr para Cursor removidas em {}",
                    result.hooks_path.display()
                ));
            } else {
                messages.push(format!(
                    "Nenhuma entrada de gancho do Herdr para Cursor encontrada em {}",
                    result.hooks_path.display()
                ));
            }
            messages
        }
        crate::api::schema::IntegrationTarget::Mastracode => {
            let result = uninstall_mastracode()?;
            let mut messages = Vec::new();
            if result.removed_hook_file {
                messages.push(format!(
                    "Removido: gancho do Mastra Code em {}",
                    result.hook_path.display()
                ));
            } else {
                messages.push(format!(
                    "Não encontrado: gancho do Mastra Code em {}",
                    result.hook_path.display()
                ));
            }
            if result.updated_hooks {
                messages.push(format!(
                    "Entradas de gancho do Herdr para Mastra Code removidas em {}",
                    result.hooks_path.display()
                ));
            } else {
                messages.push(format!(
                    "Nenhuma entrada de gancho do Herdr para Mastra Code encontrada em {}",
                    result.hooks_path.display()
                ));
            }
            messages
        }
        crate::api::schema::IntegrationTarget::AntigravityCli => {
            let result = uninstall_antigravity_cli()?;
            let mut messages = Vec::new();
            if result.removed_hook_file {
                messages.push(format!(
                    "Removido: gancho do antigravity-cli em {}",
                    result.hook_path.display()
                ));
            } else {
                messages.push(format!(
                    "Não encontrado: gancho do antigravity-cli em {}",
                    result.hook_path.display()
                ));
            }
            if result.updated_hooks {
                messages.push(format!(
                    "Entradas de gancho do Herdr para antigravity-cli removidas em {}",
                    result.hooks_path.display()
                ));
            } else {
                messages.push(format!(
                    "Nenhuma entrada de gancho do Herdr para antigravity-cli encontrada em {}",
                    result.hooks_path.display()
                ));
            }
            messages
        }
        crate::api::schema::IntegrationTarget::Grok => {
            let result = uninstall_grok()?;
            let mut messages = Vec::new();
            if result.removed_hook_file {
                messages.push(format!(
                    "Removido: gancho do Grok em {}",
                    result.hook_path.display()
                ));
            } else {
                messages.push(format!(
                    "Não encontrado: gancho do Grok em {}",
                    result.hook_path.display()
                ));
            }
            if result.removed_config_file {
                messages.push(format!(
                    "Removido: configuração de gancho do Grok em {}",
                    result.config_path.display()
                ));
            } else {
                messages.push(format!(
                    "Não encontrado: configuração de gancho do Grok em {}",
                    result.config_path.display()
                ));
            }
            messages
        }
    };

    crate::logging::integration_action("uninstall", integration_target_label(target), "ok");
    Ok(messages)
}
