use serde::Deserialize;

use super::{Config, TabBarRightEntryConfig};

pub const PRODUTO: &str = "Pretorian";
/// Mesmo limite usado pelo lançador do Pretorian ao gravar o cabeçalho.
pub const MAX_NOME_CHARS: usize = 100;
const SEPARADOR: &str = " \u{00B7} ";

/// Seção `[pretorian]` própria deste fork; fica fora de `[ui]` para não colidir com chaves do
/// Herdr original em atualizações futuras.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct PretorianConfig {
    /// Nome mostrado no cabeçalho como `Pretorian · Nome`. Vazio ou ausente mostra só `Pretorian`.
    pub nome: Option<String>,
}

pub fn sanitizar_nome(valor: &str) -> Option<String> {
    let nome = valor
        .chars()
        .filter(|ch| !ch.is_control())
        .take(MAX_NOME_CHARS)
        .collect::<String>()
        .trim()
        .to_string();
    (!nome.is_empty()).then_some(nome)
}

pub fn cabecalho(nome: Option<&str>) -> String {
    match nome {
        Some(nome) => format!("{PRODUTO}{SEPARADOR}{nome}"),
        None => PRODUTO.to_string(),
    }
}

impl Config {
    /// `ui.tab_bar_right` explícito prevalece (inclusive o gravado pelo lançador); sem ele, o
    /// cabeçalho padrão usa o nome de `[pretorian]`.
    pub(crate) fn tab_bar_right_efetivo(&self) -> Vec<TabBarRightEntryConfig> {
        if !self.ui.tab_bar_right.is_empty() {
            return self.ui.tab_bar_right.clone();
        }
        let nome = self.pretorian.nome.as_deref().and_then(sanitizar_nome);
        vec![TabBarRightEntryConfig::Text {
            text: cabecalho(nome.as_deref()),
        }]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cabecalho_sem_nome_e_so_pretorian() {
        assert_eq!(cabecalho(None), "Pretorian");
    }

    #[test]
    fn cabecalho_com_nome_usa_ponto_mediano() {
        let texto = cabecalho(Some("Ana"));
        assert_eq!(texto, "Pretorian · Ana");
        assert!(texto.contains('\u{00B7}'));
    }

    #[test]
    fn sanitizar_nome_remove_controles_apara_e_limita() {
        assert_eq!(
            sanitizar_nome("  Ana\u{1b}[0m Souza\n ").as_deref(),
            Some("Ana[0m Souza")
        );
        assert_eq!(sanitizar_nome(""), None);
        assert_eq!(sanitizar_nome("   "), None);
        assert_eq!(sanitizar_nome("\u{7}"), None);
        let longo = "á".repeat(150);
        assert_eq!(
            sanitizar_nome(&longo).map(|nome| nome.chars().count()),
            Some(MAX_NOME_CHARS)
        );
    }

    #[test]
    fn tab_bar_right_explicito_prevalece() {
        let config: Config = toml::from_str(
            r#"
[ui]
tab_bar_right = [{ type = "text", text = "Pretorian · X" }]

[pretorian]
nome = "Y"
"#,
        )
        .unwrap();
        assert_eq!(
            config.tab_bar_right_efetivo(),
            vec![TabBarRightEntryConfig::Text {
                text: "Pretorian · X".into()
            }]
        );
    }

    #[test]
    fn tab_bar_right_vazio_usa_cabecalho_com_nome() {
        let config: Config = toml::from_str("[pretorian]\nnome = \" Ana \"\n").unwrap();
        assert_eq!(
            config.tab_bar_right_efetivo(),
            vec![TabBarRightEntryConfig::Text {
                text: "Pretorian · Ana".into()
            }]
        );
        assert_eq!(
            Config::default().tab_bar_right_efetivo(),
            vec![TabBarRightEntryConfig::Text {
                text: "Pretorian".into()
            }]
        );
        let vazio: Config = toml::from_str("[pretorian]\nnome = \"\"\n").unwrap();
        assert_eq!(
            vazio.tab_bar_right_efetivo(),
            vec![TabBarRightEntryConfig::Text {
                text: "Pretorian".into()
            }]
        );
    }
}
