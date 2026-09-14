use super::*;

#[test]
fn config_replacement_preserves_macos_acl() {
    let dir = std::env::temp_dir().join(format!("herdr-config-acl-{}", std::process::id()));
    std::fs::create_dir(&dir).unwrap();
    let source = dir.join("source");
    let temporary = dir.join("temporary");
    std::fs::write(&source, b"original").unwrap();
    assert!(Command::new("chmod")
        .args(["+a", "everyone deny execute"])
        .arg(&source)
        .status()
        .unwrap()
        .success());
    let acl = |path: &Path| {
        let output = Command::new("ls").arg("-le").arg(path).output().unwrap();
        assert!(output.status.success());
        String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .skip(1)
            .collect::<Vec<_>>()
            .join("\n")
    };
    let original = acl(&source);
    assert!(original.contains("everyone deny execute"));
    assert!(Command::new("chmod")
        .args(["+a", "everyone allow read,file_inherit"])
        .arg(&dir)
        .status()
        .unwrap()
        .success());
    drop(create_config_temporary(&temporary, true).unwrap());
    assert!(
        acl(&temporary).is_empty(),
        "staging must not inherit allow ACEs"
    );
    write_config_temporary(Some(&source), &temporary, b"new").unwrap();
    assert_eq!(acl(&temporary), original);
    assert_eq!(std::fs::read(&source).unwrap(), b"original");
    assert_eq!(std::fs::read(&temporary).unwrap(), b"new");
    std::fs::remove_dir_all(dir).unwrap();
}
