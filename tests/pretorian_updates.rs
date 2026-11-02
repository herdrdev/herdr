use std::fs;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

#[test]
fn bloqueia_atualizacao_e_preserva_configuracao_e_instalacao_pessoal() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let base =
        std::env::temp_dir().join(format!("pretorian-update-{}-{nonce}", std::process::id()));
    fs::create_dir_all(&base).unwrap();
    let binario = base.join(if cfg!(windows) { "fork.exe" } else { "fork" });
    fs::copy(env!("CARGO_BIN_EXE_herdr"), &binario).unwrap();
    let antes_binario = fs::read(&binario).unwrap();
    let config = base.join("config.toml");
    let dados = b"[update]\nversion_check = true\nmanifest_check = true\nchannel = \"preview\"\n";
    fs::write(&config, dados).unwrap();
    let pessoal = base.join("herdr-pessoal");
    fs::create_dir_all(&pessoal).unwrap();
    fs::write(pessoal.join("herdr.exe"), b"instalacao pessoal").unwrap();
    for args in [
        &["update"][..],
        &["update", "--handoff"],
        &["channel", "set", "stable"],
    ] {
        let result = Command::new(&binario)
            .args(args)
            .env("HERDR_CONFIG_PATH", &config)
            .env("HERDR_INSTALL_DIR", &pessoal)
            .env("PATH", &base)
            .env_remove("HERDR")
            .output()
            .unwrap();
        assert!(!result.status.success());
        assert!(String::from_utf8_lossy(&result.stderr).contains("instalador aprovado"));
        assert_eq!(fs::read(&config).unwrap(), dados);
        assert_eq!(fs::read(&binario).unwrap(), antes_binario);
        assert_eq!(
            fs::read(pessoal.join("herdr.exe")).unwrap(),
            b"instalacao pessoal"
        );
    }
    fs::remove_dir_all(base).unwrap();
}
