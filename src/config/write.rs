#[derive(Clone, Copy)]
pub(crate) enum ConfigEdit<'a> {
    Theme(&'a str),
    StatusIndicators(super::StatusIndicatorStyle),
    Sound(bool),
    ToastDelivery(super::ToastDelivery),
    Nome(&'a str),
}

impl ConfigEdit<'_> {
    pub(crate) fn description(self) -> &'static str {
        match self {
            Self::Theme(_) => "theme",
            Self::StatusIndicators(_) => "status indicators",
            Self::Sound(_) => "sound setting",
            Self::ToastDelivery(_) => "toast setting",
            Self::Nome(_) => "nome do usuário",
        }
    }

    pub(crate) fn apply(self, content: &str) -> String {
        match self {
            Self::Theme(name) => {
                let content =
                    super::upsert_section_value(content, "theme", "name", &format!("\"{name}\""));
                super::upsert_section_bool(&content, "theme", "auto_switch", false)
            }
            Self::StatusIndicators(style) => super::upsert_section_value(
                content,
                "ui",
                "status_indicators",
                &format!("\"{}\"", style.as_str()),
            ),
            Self::Sound(enabled) => {
                super::upsert_section_bool(content, "ui.sound", "enabled", enabled)
            }
            Self::ToastDelivery(delivery) => {
                let value = match delivery {
                    super::ToastDelivery::Off => "\"off\"",
                    super::ToastDelivery::Herdr => "\"herdr\"",
                    super::ToastDelivery::Terminal => "\"terminal\"",
                    super::ToastDelivery::System => "\"system\"",
                };
                let content = super::upsert_section_value(content, "ui.toast", "delivery", value);
                super::remove_section_key(&content, "ui.toast", "enabled")
            }
            Self::Nome(nome) => super::upsert_section_value(
                content,
                "pretorian",
                "nome",
                &toml::Value::String(nome.to_owned()).to_string(),
            ),
        }
    }
}

pub(crate) fn update_file_at(
    path: &std::path::Path,
    description: &str,
    update: impl FnOnce(&str) -> Result<String, String>,
) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("failed to create config directory: {error}"))?;
    }
    let content = match super::io::read_optional_config(path) {
        Ok(Some(content)) => content,
        Ok(None) => String::new(),
        Err(error) => {
            return Err(format!(
                "failed to read config before saving {description}: {error}"
            ));
        }
    };
    let updated = update(&content)?;
    std::fs::write(path, updated).map_err(|error| format!("failed to save {description}: {error}"))
}

pub(crate) fn write_edit(edit: ConfigEdit<'_>) -> Result<(), String> {
    update_file_at(&super::config_path(), edit.description(), |content| {
        Ok(edit.apply(content))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nome_edit_escapa_string_toml() {
        let nome = "Ana \"Zé\" \\ Souza";
        let escrito = ConfigEdit::Nome(nome).apply("");
        let lido: toml::Value = toml::from_str(&escrito).expect("TOML válido");
        assert_eq!(lido["pretorian"]["nome"].as_str(), Some(nome));
    }

    #[test]
    fn nome_edit_substitui_valor_existente_sem_tocar_outras_secoes() {
        let original = "[theme]\nname = \"pretorian\"\n\n[pretorian]\nnome = \"Ana\"\n";
        let escrito = ConfigEdit::Nome("Bia").apply(original);
        let lido: toml::Value = toml::from_str(&escrito).expect("TOML válido");
        assert_eq!(lido["pretorian"]["nome"].as_str(), Some("Bia"));
        assert_eq!(lido["theme"]["name"].as_str(), Some("pretorian"));
        assert_eq!(escrito.matches("nome = ").count(), 1);
    }

    #[test]
    fn update_file_at_does_not_move_a_leading_bom_into_the_file() {
        let dir = std::env::temp_dir().join(format!("herdr-config-bom-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(
            &path,
            b"\xEF\xBB\xBF[terminal]\ndefault_shell = \"pwsh.exe\"\n",
        )
        .unwrap();

        update_file_at(&path, "onboarding setting", |content| {
            Ok(crate::config::upsert_top_level_bool(
                content,
                "onboarding",
                false,
            ))
        })
        .unwrap();

        let written = std::fs::read_to_string(&path).unwrap();
        let _ = std::fs::remove_dir_all(dir);

        assert!(
            !written.contains('\u{feff}'),
            "unexpected BOM in {written:?}"
        );
        assert!(
            toml::from_str::<toml::Value>(&written).is_ok(),
            "written config is not valid TOML: {written:?}"
        );
    }
}
