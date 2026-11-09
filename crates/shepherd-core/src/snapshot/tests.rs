use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Once;

use super::*;

static N: AtomicU64 = AtomicU64::new(0);
static ISOLATE: Once = Once::new();

/// A throwaway git repo in a temp dir (never the real repo, never the real `$HOME` config).
struct TestRepo {
    dir: PathBuf,
}

impl Drop for TestRepo {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn git_in(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .current_dir(dir)
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.invalid",
            "-c",
            "core.quotepath=off",
        ])
        .args(args)
        .output()
        .expect("git");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

impl TestRepo {
    fn empty() -> Self {
        ISOLATE.call_once(|| {
            std::env::set_var("GIT_CONFIG_GLOBAL", "/dev/null");
            std::env::set_var("GIT_CONFIG_NOSYSTEM", "1");
        });
        let dir = std::env::temp_dir().join(format!(
            "shep-snap-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        git_in(&dir, &["init", "-q", "-b", "main"]);
        TestRepo { dir }
    }

    /// Repo with one commit containing `a.txt`.
    fn new() -> Self {
        let r = Self::empty();
        r.write("a.txt", "one\ntwo\n");
        git_in(&r.dir, &["add", "a.txt"]);
        git_in(&r.dir, &["commit", "-q", "-m", "init"]);
        r
    }

    fn write(&self, rel: &str, content: &str) {
        let p = self.dir.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, content).unwrap();
    }

    fn read(&self, rel: &str) -> String {
        fs::read_to_string(self.dir.join(rel)).unwrap()
    }

    fn exists(&self, rel: &str) -> bool {
        self.dir.join(rel).exists()
    }

    fn git(&self, args: &[&str]) -> String {
        git_in(&self.dir, args)
    }

    /// Everything the user could notice: status, HEAD, branches, stash, index bytes.
    fn user_state(&self) -> (String, String, String, String, Option<Vec<u8>>) {
        (
            self.git(&["status", "--porcelain=v1", "--ignored"]),
            self.git(&["rev-parse", "HEAD"]),
            self.git(&["branch", "--list"]),
            self.git(&["stash", "list"]),
            fs::read(self.dir.join(".git/index")).ok(),
        )
    }
}

fn limits() -> SnapshotLimits {
    SnapshotLimits::default()
}

fn cap(r: &TestRepo, agent: &str) -> capture::CaptureResult {
    capture(&r.dir, agent, SnapshotReason::Manual, &limits()).unwrap()
}

#[test]
fn capture_creates_hidden_ref_without_touching_user_state() {
    let r = TestRepo::new();
    r.write("a.txt", "one\nchanged\n"); // unstaged edit
    r.write("new.txt", "untracked\n");
    r.write("staged.txt", "staged\n");
    r.git(&["add", "staged.txt"]);
    let before = r.user_state();

    let res = cap(&r, "coder-1");

    assert!(res.created);
    assert_eq!(res.snapshot.id, "coder-1/1");
    assert_eq!(res.snapshot.reason, SnapshotReason::Manual);
    assert_eq!(res.snapshot.prev, None);
    assert_eq!(res.snapshot.files, 3); // a.txt (changed), new.txt, staged.txt: first snapshot = all files vs empty
    let _ = res.snapshot.files;
    assert_eq!(
        r.user_state(),
        before,
        "index/branch/status/stash must be untouched"
    );
    assert_eq!(
        r.git(&[
            "for-each-ref",
            "--format=%(refname)",
            "refs/shepherd/snapshots/"
        ])
        .trim(),
        "refs/shepherd/snapshots/coder-1/1"
    );
    // The snapshot holds the working-tree content, staged or not.
    let tree = r.git(&["ls-tree", "-r", "--name-only", &res.snapshot.commit]);
    assert_eq!(
        tree.lines().collect::<Vec<_>>(),
        ["a.txt", "new.txt", "staged.txt"]
    );
    assert_eq!(
        r.git(&["show", &format!("{}:a.txt", res.snapshot.commit)]),
        "one\nchanged\n"
    );
    // It is not a branch commit and not reachable from HEAD.
    assert_eq!(r.git(&["log", "--oneline"]).lines().count(), 1);
    assert!(r
        .git(&["branch", "--contains", &res.snapshot.commit])
        .trim()
        .is_empty());
}

#[test]
fn identical_capture_creates_no_new_snapshot() {
    let r = TestRepo::new();
    let first = cap(&r, "a");
    let again = cap(&r, "a");
    assert!(first.created);
    assert!(!again.created);
    assert_eq!(again.snapshot.id, first.snapshot.id);
    assert_eq!(list(&r.dir, Some("a"), false).unwrap().len(), 1);

    r.write("a.txt", "different\n");
    let third = cap(&r, "a");
    assert!(third.created);
    assert_eq!(third.snapshot.id, "a/2");
    assert_eq!(third.snapshot.prev.as_deref(), Some("a/1"));
    // Another agent has its own sequence.
    assert_eq!(cap(&r, "b").snapshot.id, "b/1");
}

#[test]
fn summary_counts_files_and_lines_against_previous() {
    let r = TestRepo::new();
    cap(&r, "a");
    r.write("a.txt", "one\ntwo\nthree\nfour\n"); // +2
    r.write("b.txt", "x\n"); // +1
    let s = cap(&r, "a").snapshot;
    assert_eq!((s.files, s.added, s.removed), (2, 3, 0));
    fs::remove_file(r.dir.join("b.txt")).unwrap();
    r.write("a.txt", "one\n"); // -3
    let s = cap(&r, "a").snapshot;
    assert_eq!((s.files, s.added, s.removed), (2, 0, 4));
}

#[test]
fn gitignore_is_respected_and_large_files_are_omitted() {
    let r = TestRepo::new();
    r.write(".gitignore", "ignored.log\nbuild/\n");
    r.write("ignored.log", "noise\n");
    r.write("build/out.o", "obj\n");
    r.write("big.bin", &"x".repeat(5000));
    r.write("ok.txt", "fine\n");
    let small = SnapshotLimits {
        max_file_bytes: 1000,
        ..limits()
    };
    let s = capture(&r.dir, "a", SnapshotReason::Idle, &small)
        .unwrap()
        .snapshot;
    let files = r.git(&["ls-tree", "-r", "--name-only", &s.commit]);
    let files: Vec<_> = files.lines().collect();
    assert_eq!(files, [".gitignore", "a.txt", "ok.txt"]);
    assert_eq!(s.omitted.len(), 1);
    assert_eq!(s.omitted[0].path, "big.bin");
    assert_eq!(s.omitted[0].size, 5000);
    // Metadata round-trips through the commit message.
    let listed = list(&r.dir, Some("a"), false).unwrap();
    assert_eq!(listed, vec![s]);
}

#[test]
fn oversized_tracked_file_is_left_out_but_state_is_consistent() {
    let r = TestRepo::new();
    r.write("big.dat", &"y".repeat(3000));
    r.git(&["add", "big.dat"]);
    r.git(&["commit", "-q", "-m", "big"]);
    let small = SnapshotLimits {
        max_file_bytes: 1000,
        ..limits()
    };
    let s = capture(&r.dir, "a", SnapshotReason::Manual, &small)
        .unwrap()
        .snapshot;
    assert!(!r
        .git(&["ls-tree", "-r", "--name-only", &s.commit])
        .contains("big.dat"));
    // Diffing against the live tree uses the same omission, so it is not reported as changed.
    let d = diff(&r.dir, &s.id, None, false, &small).unwrap();
    assert!(d.files.is_empty(), "{d:?}");
}

#[test]
fn capture_over_total_limit_fails_clearly() {
    let r = TestRepo::new();
    r.write("one.txt", &"z".repeat(600));
    let tiny = SnapshotLimits {
        max_capture_bytes: 500,
        ..limits()
    };
    let err = capture(&r.dir, "a", SnapshotReason::Manual, &tiny).unwrap_err();
    assert!(err.contains("max_capture_bytes"), "{err}");
    assert!(list(&r.dir, Some("a"), false).unwrap().is_empty());
}

#[test]
fn per_agent_limit_prunes_oldest_and_prune_command_works() {
    let r = TestRepo::new();
    let two = SnapshotLimits {
        max_per_agent: 2,
        ..limits()
    };
    for i in 0..4 {
        r.write("a.txt", &format!("v{i}\n"));
        let res = capture(&r.dir, "a", SnapshotReason::Manual, &two).unwrap();
        assert_eq!(res.pruned.len(), usize::from(i >= 2), "capture {i}");
    }
    let ids: Vec<_> = list(&r.dir, Some("a"), false)
        .unwrap()
        .into_iter()
        .map(|s| s.id)
        .collect();
    assert_eq!(ids, ["a/3", "a/4"]);

    // Numbering keeps growing; prune --keep trims further, per agent.
    r.write("a.txt", "other\n");
    cap(&r, "b");
    r.write("a.txt", "other2\n");
    cap(&r, "b");
    let p = prune(&r.dir, None, Some(1), &limits()).unwrap();
    assert_eq!(p.pruned, ["a/3", "b/1"]);
    assert_eq!(p.kept, 2);
    let p = prune(&r.dir, Some("a"), Some(5), &limits()).unwrap();
    assert!(p.pruned.is_empty());
}

#[test]
fn pruned_snapshots_release_their_objects() {
    let r = TestRepo::new();
    r.write("a.txt", "unique-old-content-1234\n");
    let old = cap(&r, "a").snapshot;
    r.write("a.txt", "newer\n");
    cap(&r, "a");
    delete(&r.dir, &old.id).unwrap();
    r.git(&["reflog", "expire", "--expire=now", "--all"]);
    r.git(&["gc", "-q", "--prune=now"]);
    let out = Command::new("git")
        .current_dir(&r.dir)
        .args(["cat-file", "-e", &old.commit])
        .output()
        .unwrap();
    assert!(!out.status.success(), "pruned commit must be collectable");
    // The surviving snapshot is intact after gc.
    assert_eq!(list(&r.dir, Some("a"), false).unwrap().len(), 1);
    let s = show(&r.dir, "a/2", false).unwrap();
    assert_eq!(s.snapshot.prev.as_deref(), Some("a/1"));
    assert!(s.diff.files.iter().any(|f| f.path == "a.txt")); // prev gone: diffed against nothing
}

#[test]
fn diff_between_snapshots_and_against_current() {
    let r = TestRepo::new();
    let a = cap(&r, "a").snapshot;
    r.write("a.txt", "one\nTWO\nthree\n");
    r.write("n.txt", "new\n");
    let b = cap(&r, "a").snapshot;

    let d = diff(&r.dir, &a.id, Some(&b.id), false, &limits()).unwrap();
    assert_eq!((d.from.as_str(), d.to.as_str()), ("a/1", "a/2"));
    assert_eq!(
        d.files
            .iter()
            .map(|f| (f.path.as_str(), f.status.as_str(), f.added, f.removed))
            .collect::<Vec<_>>(),
        [("a.txt", "modified", 2, 1), ("n.txt", "added", 1, 0)]
    );
    assert_eq!((d.added, d.removed), (3, 1));
    assert!(d.patch.is_none());

    let full = diff(&r.dir, &a.id, Some(&b.id), true, &limits()).unwrap();
    let patch = full.patch.unwrap();
    assert!(patch.contains("+TWO") && patch.contains("-two"), "{patch}");

    // Against the live worktree.
    fs::remove_file(r.dir.join("n.txt")).unwrap();
    let cur = diff(&r.dir, &b.id, None, false, &limits()).unwrap();
    assert_eq!(cur.to, "current");
    assert_eq!(cur.files.len(), 1);
    assert_eq!(
        (cur.files[0].path.as_str(), cur.files[0].status.as_str()),
        ("n.txt", "deleted")
    );
    // Identical state: empty diff.
    let same = diff(&r.dir, &b.id, Some(&b.id), false, &limits()).unwrap();
    assert!(same.files.is_empty());

    let shown = show(&r.dir, "a/2", true).unwrap();
    assert_eq!(shown.diff.from, "a/1");
    assert!(shown.diff.patch.unwrap().contains("+new"));
    let first = show(&r.dir, "a/1", false).unwrap();
    assert_eq!(first.diff.from, "-");
    assert_eq!(first.diff.files[0].status, "added");
}

#[test]
fn restore_single_file_keeps_everything_else_and_saves_pre_restore() {
    let r = TestRepo::new();
    r.write("keep.txt", "keep-v1\n");
    let s = cap(&r, "a").snapshot;
    r.write("a.txt", "broken\n");
    r.write("keep.txt", "keep-v2-uncommitted\n");
    let before = r.user_state();
    let staged_before = r.git(&["ls-files", "-s"]);

    let res = restore(&r.dir, &s.id, &["a.txt".into()], false, false, &limits()).unwrap();

    assert_eq!(r.read("a.txt"), "one\ntwo\n");
    assert_eq!(r.read("keep.txt"), "keep-v2-uncommitted\n");
    assert_eq!(res.restored_files, ["a.txt"]);
    assert!(res.pre_restore_created);
    assert_eq!(res.pre_restore.reason, SnapshotReason::PreRestore);
    assert_eq!(res.pre_restore.id, "a/2");
    // The pre-restore snapshot really holds the state we overwrote.
    assert_eq!(
        r.git(&["show", &format!("{}:a.txt", res.pre_restore.commit)]),
        "broken\n"
    );
    // Only working-tree files changed: same HEAD/branches/stash/index.
    // (Raw index bytes differ only because `git status` refreshes stat data of the file we
    // rewrote; the staged content is identical.)
    let after = r.user_state();
    assert_eq!(
        (&before.1, &before.2, &before.3),
        (&after.1, &after.2, &after.3)
    );
    assert_eq!(r.git(&["ls-files", "-s"]), staged_before);
}

#[test]
fn restore_whole_worktree_requires_confirm_and_keeps_extras_unless_asked() {
    let r = TestRepo::new();
    r.write("dir/sub.txt", "s1\n");
    let s = cap(&r, "a").snapshot;
    r.write("a.txt", "mangled\n");
    r.write("dir/sub.txt", "s2\n");
    r.write("later.txt", "created after\n");
    r.write("dir2/deep/later2.txt", "created after\n");
    fs::remove_file(r.dir.join("dir/sub.txt")).unwrap();
    r.write("dir/sub.txt", "s3\n");

    let err = restore(&r.dir, &s.id, &[], false, false, &limits()).unwrap_err();
    assert!(err.contains("--confirm"), "{err}");
    assert_eq!(
        r.read("a.txt"),
        "mangled\n",
        "nothing may change without --confirm"
    );
    assert_eq!(
        list(&r.dir, Some("a"), false).unwrap().len(),
        1,
        "no pre-restore without confirm"
    );

    let res = restore(&r.dir, &s.id, &[], true, false, &limits()).unwrap();
    assert_eq!(r.read("a.txt"), "one\ntwo\n");
    assert_eq!(r.read("dir/sub.txt"), "s1\n");
    assert!(r.exists("later.txt"), "extra files are kept by default");
    assert_eq!(res.extra_files, ["dir2/deep/later2.txt", "later.txt"]);
    assert!(res.deleted_files.is_empty());
    assert!(res.pre_restore_created);

    // Second time, deleting extras explicitly (they are recoverable from pre-restore).
    r.write("a.txt", "mangled again\n");
    let res = restore(&r.dir, &s.id, &[], true, true, &limits()).unwrap();
    assert!(
        !r.exists("later.txt") && !r.exists("dir2"),
        "extras and empty dirs gone"
    );
    assert_eq!(res.deleted_files, ["dir2/deep/later2.txt", "later.txt"]);
    assert!(res.extra_files.is_empty());
    let pre = &res.pre_restore;
    assert_eq!(
        r.git(&["show", &format!("{}:later.txt", pre.commit)]),
        "created after\n"
    );
}

#[test]
fn restore_rejects_bad_paths_and_unknown_ids_without_side_effects() {
    let r = TestRepo::new();
    let s = cap(&r, "a").snapshot;
    r.write("a.txt", "edited\n");
    for p in ["../x", "/etc/passwd", ""] {
        let err = restore(&r.dir, &s.id, &[p.into()], false, false, &limits()).unwrap_err();
        assert!(err.contains("path"), "{p:?}: {err}");
    }
    let err = restore(&r.dir, &s.id, &["nope.txt".into()], false, false, &limits()).unwrap_err();
    assert!(err.contains("not in snapshot"), "{err}");
    let err = restore(&r.dir, "a/99", &["a.txt".into()], false, false, &limits()).unwrap_err();
    assert!(err.contains("not found"), "{err}");
    let err = restore(&r.dir, &s.id, &["a.txt".into()], false, true, &limits()).unwrap_err();
    assert!(err.contains("delete-extra"), "{err}");
    assert_eq!(r.read("a.txt"), "edited\n");
    assert_eq!(list(&r.dir, Some("a"), false).unwrap().len(), 1);
}

#[test]
fn repo_without_initial_commit_works() {
    let r = TestRepo::empty();
    r.write("first.txt", "hello\n");
    let s = cap(&r, "a").snapshot;
    assert_eq!(
        r.git(&["ls-tree", "-r", "--name-only", &s.commit]).trim(),
        "first.txt"
    );
    // Still no commits, nothing staged.
    assert!(Command::new("git")
        .current_dir(&r.dir)
        .args(["rev-parse", "--verify", "HEAD"])
        .output()
        .map(|o| !o.status.success())
        .unwrap());
    assert!(!r.dir.join(".git/index").exists());
    r.write("first.txt", "changed\n");
    restore(
        &r.dir,
        &s.id,
        &["first.txt".into()],
        false,
        false,
        &limits(),
    )
    .unwrap();
    assert_eq!(r.read("first.txt"), "hello\n");
}

#[test]
fn empty_worktree_can_be_captured() {
    let r = TestRepo::empty();
    let s = cap(&r, "a").snapshot;
    assert_eq!(s.files, 0);
    assert!(!cap(&r, "a").created);
}

#[test]
fn not_a_repo_and_in_progress_operations_are_clear_errors() {
    let r = TestRepo::empty();
    let plain = r.dir.join("plain");
    fs::create_dir_all(&plain).unwrap();
    let outside = std::env::temp_dir().join(format!("shep-snap-norepo-{}", std::process::id()));
    fs::create_dir_all(&outside).unwrap();
    let err = capture(&outside, "a", SnapshotReason::Manual, &limits()).unwrap_err();
    assert!(err.contains("not a git worktree"), "{err}");
    assert!(list(&outside, None, true)
        .unwrap_err()
        .contains("not a git worktree"));
    let _ = fs::remove_dir_all(&outside);
    let err = capture(
        &r.dir.join("missing"),
        "a",
        SnapshotReason::Manual,
        &limits(),
    )
    .unwrap_err();
    assert!(err.contains("not a directory"), "{err}");

    // A merge in progress.
    let m = TestRepo::new();
    m.git(&["checkout", "-q", "-b", "side"]);
    m.write("a.txt", "side\n");
    m.git(&["commit", "-qam", "side"]);
    m.git(&["checkout", "-q", "main"]);
    m.write("a.txt", "main\n");
    m.git(&["commit", "-qam", "main"]);
    let merge = Command::new("git")
        .current_dir(&m.dir)
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.invalid",
            "merge",
            "side",
        ])
        .output()
        .unwrap();
    assert!(
        !merge.status.success(),
        "the test needs a conflicting merge"
    );
    let state = m.user_state();
    let err = capture(&m.dir, "a", SnapshotReason::Manual, &limits()).unwrap_err();
    assert!(err.contains("merge is in progress"), "{err}");
    assert!(restore(&m.dir, "a/1", &[], true, false, &limits())
        .unwrap_err()
        .contains("merge"));
    assert_eq!(m.user_state(), state);
    assert!(list(&m.dir, None, true).unwrap().is_empty());

    // A rebase in progress.
    let b = TestRepo::new();
    fs::create_dir_all(b.dir.join(".git/rebase-merge")).unwrap();
    assert!(capture(&b.dir, "a", SnapshotReason::Manual, &limits())
        .unwrap_err()
        .contains("rebase"));
}

#[test]
fn spaces_unicode_and_odd_names_roundtrip() {
    let r = TestRepo::new();
    r.write("dir with space/fichero único ñ 日本.txt", "contenido\n");
    r.write("-leading-dash.txt", "d\n");
    r.write("[glob]*.txt", "g\n");
    r.write(":colon.txt", "c\n");
    let big = SnapshotLimits {
        max_file_bytes: 5,
        ..limits()
    };
    fs::write(r.dir.join("grande ñ.txt"), "x".repeat(50)).unwrap();
    let s = capture(&r.dir, "a", SnapshotReason::Manual, &big)
        .unwrap()
        .snapshot;
    assert!(
        s.omitted.iter().any(|o| o.path == "grande ñ.txt"),
        "{:?}",
        s.omitted
    );
    let s = cap(&r, "b").snapshot; // default limits: now included
    let names = r.git(&["ls-tree", "-r", "--name-only", &s.commit]);
    assert!(names.contains("fichero único ñ 日本.txt"), "{names}");

    for p in [
        "dir with space/fichero único ñ 日本.txt",
        "-leading-dash.txt",
        "[glob]*.txt",
        ":colon.txt",
    ] {
        fs::remove_file(r.dir.join(p)).unwrap();
        restore(&r.dir, &s.id, &[p.into()], false, false, &limits()).unwrap();
        assert!(r.exists(p), "{p}");
    }
    // A directory restores everything below it.
    fs::remove_dir_all(r.dir.join("dir with space")).unwrap();
    let res = restore(
        &r.dir,
        &s.id,
        &["dir with space".into()],
        false,
        false,
        &limits(),
    )
    .unwrap();
    assert_eq!(
        res.restored_files,
        ["dir with space/fichero único ñ 日本.txt"]
    );
    assert!(diff(&r.dir, &s.id, None, false, &limits())
        .unwrap()
        .files
        .is_empty());
}

#[test]
fn agent_names_are_validated_and_worktrees_share_refs() {
    let r = TestRepo::new();
    for bad in [
        "", "a/b", "../x", "a b", ".hidden", "x.lock", "-rf", "a..b", "ñ",
    ] {
        assert!(
            capture(&r.dir, bad, SnapshotReason::Manual, &limits()).is_err(),
            "{bad:?}"
        );
    }
    assert!(parse_id("coder-24/7").is_ok());
    assert!(
        parse_id("coder-24").is_err() && parse_id("a/x").is_err() && parse_id("a/b/1").is_err()
    );

    // A linked worktree sees the same refs, and `list` without --all is per worktree.
    let wt = r.dir.parent().unwrap().join(format!(
        "{}-wt",
        r.dir.file_name().unwrap().to_string_lossy()
    ));
    r.git(&[
        "worktree",
        "add",
        "-q",
        wt.to_str().unwrap(),
        "-b",
        "feature",
    ]);
    fs::write(wt.join("w.txt"), "w\n").unwrap();
    cap(&r, "main-agent");
    let w = capture(&wt, "wt-agent", SnapshotReason::Done, &limits())
        .unwrap()
        .snapshot;
    let mine = list(&wt, None, false).unwrap();
    assert_eq!(
        mine.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(),
        ["wt-agent/1"]
    );
    let all = list(&wt, None, true).unwrap();
    assert_eq!(all.len(), 2);
    assert_eq!(list(&r.dir, Some("wt-agent"), false).unwrap(), vec![w]);
    restore(
        &wt,
        "wt-agent/1",
        &["w.txt".into()],
        false,
        false,
        &limits(),
    )
    .unwrap();
    r.git(&["worktree", "remove", "--force", wt.to_str().unwrap()]);
}

#[test]
fn delete_removes_only_the_ref() {
    let r = TestRepo::new();
    cap(&r, "a");
    r.write("a.txt", "v2\n");
    cap(&r, "a");
    let before = r.user_state();
    assert_eq!(delete(&r.dir, "a/1").unwrap().deleted, "a/1");
    assert_eq!(list(&r.dir, Some("a"), false).unwrap().len(), 1);
    assert!(delete(&r.dir, "a/1").unwrap_err().contains("not found"));
    assert_eq!(r.user_state(), before);
    // Numbering never reuses a deleted-from-the-middle id.
    r.write("a.txt", "v3\n");
    assert_eq!(cap(&r, "a").snapshot.id, "a/3");
}

#[test]
fn utc_formatting_and_reasons() {
    assert_eq!(format_utc(0), "1970-01-01 00:00:00");
    assert_eq!(format_utc(1_730_000_000), "2024-10-27 03:33:20");
    assert_eq!(format_utc(951_782_400), "2000-02-29 00:00:00");
    for reason in [
        "manual",
        "idle",
        "done",
        "pre-restore",
        "pre-clean",
        "pre-kill",
    ] {
        assert_eq!(SnapshotReason::parse(reason).unwrap().as_str(), reason);
        let json = serde_json::to_string(&SnapshotReason::parse(reason).unwrap()).unwrap();
        assert_eq!(json, format!("\"{reason}\""));
    }
    assert!(SnapshotReason::parse("later").is_err());
}
