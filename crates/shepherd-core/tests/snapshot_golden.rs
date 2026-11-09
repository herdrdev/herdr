//! Golden files for the typed snapshot operations and their human CLI output (Phase F11a).
//!
//! Regenerate (only new files should appear) with:
//! `SHEPHERD_UPDATE_GOLDEN=1 ./cargo-fork.sh test -p shepherd-core --test snapshot_golden`

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;
use shepherd_core::snapshot::{self, render, SnapshotInfo, SnapshotLimits, SnapshotReason};

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .current_dir(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}");
}

fn golden(name: &str, ext: &str, actual: &str) {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(format!("{name}.{ext}"));
    if std::env::var_os("SHEPHERD_UPDATE_GOLDEN").is_some() {
        fs::write(&path, actual).unwrap();
        return;
    }
    let expected = fs::read_to_string(&path).unwrap_or_else(|_| {
        panic!(
            "missing golden {}; run with SHEPHERD_UPDATE_GOLDEN=1",
            path.display()
        )
    });
    assert_eq!(actual, expected, "golden mismatch: {}", path.display());
}

fn json(name: &str, value: &impl serde::Serialize) {
    let mut v: Value = serde_json::to_value(value).unwrap();
    scrub(&mut v);
    golden(
        name,
        "json",
        &format!("{}\n", serde_json::to_string_pretty(&v).unwrap()),
    );
}

/// Replace values that differ per run (commit ids, clocks, temp paths).
fn scrub(v: &mut Value) {
    match v {
        Value::Object(m) => {
            for (k, val) in m.iter_mut() {
                match k.as_str() {
                    "commit" => *val = "<COMMIT>".into(),
                    "created" => *val = 0.into(),
                    "worktree" => *val = "<ROOT>".into(),
                    _ => scrub(val),
                }
            }
        }
        Value::Array(a) => a.iter_mut().for_each(scrub),
        _ => {}
    }
}

fn scrubbed(mut s: SnapshotInfo) -> SnapshotInfo {
    s.commit = "<COMMIT>".into();
    s.created = 0;
    s.worktree = "<ROOT>".into();
    s
}

#[test]
fn snapshot_operations_and_human_output_match_golden() {
    let dir = std::env::temp_dir().join(format!("shep-snap-golden-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    git(&dir, &["init", "-q", "-b", "main"]);
    fs::write(dir.join("a.txt"), "one\ntwo\n").unwrap();
    git(&dir, &["add", "a.txt"]);
    git(&dir, &["commit", "-q", "-m", "init"]);
    let limits = SnapshotLimits::default();

    let first = snapshot::capture(&dir, "coder-1", SnapshotReason::Manual, &limits).unwrap();
    json("snapshot_capture", &first);
    let mut human = render::capture(&first);
    let again = snapshot::capture(&dir, "coder-1", SnapshotReason::Idle, &limits).unwrap();
    json("snapshot_capture_unchanged", &again);
    human += &render::capture(&again);
    golden("cli_human_snapshot_capture", "txt", &human);

    fs::write(dir.join("a.txt"), "one\nTWO\nthree\n").unwrap();
    fs::write(dir.join("new.txt"), "new\n").unwrap();
    snapshot::capture(&dir, "coder-1", SnapshotReason::Done, &limits).unwrap();

    let mut listed = snapshot::list(&dir, None, false).unwrap();
    json("snapshot_list", &listed);
    listed.iter_mut().for_each(|s| *s = scrubbed(s.clone()));
    golden("cli_human_snapshot_list", "txt", &render::list(&listed));
    golden("cli_human_snapshot_list_empty", "txt", &render::list(&[]));

    let shown = snapshot::show(&dir, "coder-1/2", true).unwrap();
    json("snapshot_show", &shown);
    let mut shown = shown;
    shown.snapshot = scrubbed(shown.snapshot);
    golden("cli_human_snapshot_show", "txt", &render::show(&shown));

    let d = snapshot::diff(&dir, "coder-1/1", Some("coder-1/2"), false, &limits).unwrap();
    json("snapshot_diff", &d);
    golden("cli_human_snapshot_diff", "txt", &render::diff(&d));
    fs::remove_file(dir.join("new.txt")).unwrap();
    let cur = snapshot::diff(&dir, "coder-1/2", None, true, &limits).unwrap();
    golden(
        "cli_human_snapshot_diff_current",
        "txt",
        &render::diff(&cur),
    );

    fs::write(dir.join("a.txt"), "broken\n").unwrap();
    fs::write(dir.join("later.txt"), "x\n").unwrap();
    let mut restored = snapshot::restore(&dir, "coder-1/2", &[], true, false, &limits).unwrap();
    json("snapshot_restore", &restored);
    restored.restored = scrubbed(restored.restored);
    restored.pre_restore = scrubbed(restored.pre_restore);
    golden(
        "cli_human_snapshot_restore",
        "txt",
        &render::restore(&restored),
    );
    let mut deleted_extra = snapshot::restore(&dir, "coder-1/2", &[], true, true, &limits).unwrap();
    deleted_extra.restored = scrubbed(deleted_extra.restored);
    deleted_extra.pre_restore = scrubbed(deleted_extra.pre_restore);
    golden(
        "cli_human_snapshot_restore_delete_extra",
        "txt",
        &render::restore(&deleted_extra),
    );

    let deleted = snapshot::delete(&dir, "coder-1/1").unwrap();
    json("snapshot_delete", &deleted);
    golden(
        "cli_human_snapshot_delete",
        "txt",
        &render::delete(&deleted),
    );
    let pruned = snapshot::prune(&dir, Some("coder-1"), Some(1), &limits).unwrap();
    json("snapshot_prune", &pruned);
    golden("cli_human_snapshot_prune", "txt", &render::prune(&pruned));
    golden(
        "cli_human_snapshot_prune_nothing",
        "txt",
        &render::prune(&snapshot::prune(&dir, None, Some(9), &limits).unwrap()),
    );

    let err = snapshot::restore(&dir, "coder-1/4", &[], false, false, &limits).unwrap_err();
    golden(
        "snapshot_error_restore_needs_confirm",
        "txt",
        &format!("{err}\n"),
    );

    let _ = fs::remove_dir_all(&dir);
}
