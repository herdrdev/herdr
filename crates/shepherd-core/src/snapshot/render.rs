//! Human-readable CLI output for snapshot results (JSON output is the serialized types).

use std::fmt::Write;

use super::capture::CaptureResult;
use super::diff::{DiffResult, ShowResult};
use super::restore::RestoreResult;
use super::{format_utc, DeleteResult, PruneResult, SnapshotInfo};

fn changes(s: &SnapshotInfo) -> String {
    format!("{} file(s), +{} -{}", s.files, s.added, s.removed)
}

fn omitted_lines(out: &mut String, s: &SnapshotInfo) {
    for o in &s.omitted {
        let _ = writeln!(out, "omitted {} ({}, {} bytes)", o.path, o.reason, o.size);
    }
}

pub fn capture(r: &CaptureResult) -> String {
    let s = &r.snapshot;
    let mut out = if r.created {
        format!(
            "created snapshot {} ({}, {})\n",
            s.id,
            s.reason.as_str(),
            changes(s)
        )
    } else {
        format!("no changes since {}; nothing captured\n", s.id)
    };
    omitted_lines(&mut out, s);
    if !r.pruned.is_empty() {
        let _ = writeln!(out, "pruned {}", r.pruned.join(", "));
    }
    out
}

pub fn list(snaps: &[SnapshotInfo]) -> String {
    if snaps.is_empty() {
        return "no snapshots\n".into();
    }
    let mut out = String::from("ID\tCREATED (UTC)\tREASON\tFILES\tCHANGES\n");
    for s in snaps {
        let _ = writeln!(
            out,
            "{}\t{}\t{}\t{}\t+{} -{}",
            s.id,
            format_utc(s.created),
            s.reason.as_str(),
            s.files,
            s.added,
            s.removed
        );
    }
    out
}

fn diff_body(out: &mut String, d: &DiffResult) {
    if d.files.is_empty() {
        out.push_str("no differences\n");
    }
    for f in &d.files {
        let code = match f.status.as_str() {
            "added" => 'A',
            "deleted" => 'D',
            "type-changed" => 'T',
            _ => 'M',
        };
        if f.binary {
            let _ = writeln!(out, "{code} {} (binary)", f.path);
        } else {
            let _ = writeln!(out, "{code} {} +{} -{}", f.path, f.added, f.removed);
        }
    }
    if !d.files.is_empty() {
        let _ = writeln!(
            out,
            "{} file(s), +{} -{}",
            d.files.len(),
            d.added,
            d.removed
        );
    }
    if let Some(p) = &d.patch {
        out.push('\n');
        out.push_str(p);
    }
}

pub fn diff(d: &DiffResult) -> String {
    let mut out = format!("{} -> {}\n", d.from, d.to);
    diff_body(&mut out, d);
    out
}

pub fn show(r: &ShowResult) -> String {
    let s = &r.snapshot;
    let mut out = format!(
        "snapshot {}\ncreated  {} UTC\nreason   {}\nworktree {}\ncommit   {}\n",
        s.id,
        format_utc(s.created),
        s.reason.as_str(),
        s.worktree,
        s.commit
    );
    if let Some(p) = &s.prev {
        let _ = writeln!(out, "previous {p}");
    }
    omitted_lines(&mut out, s);
    let _ = writeln!(out, "\nchanges since {}:", r.diff.from);
    diff_body(&mut out, &r.diff);
    out
}

pub fn restore(r: &RestoreResult) -> String {
    let mut out = format!(
        "restored {} file(s) from {}\n",
        r.restored_files.len(),
        r.restored.id
    );
    if r.pre_restore_created {
        let _ = writeln!(
            out,
            "previous state saved as {} (pre-restore)",
            r.pre_restore.id
        );
    } else {
        let _ = writeln!(
            out,
            "previous state was already saved as {}",
            r.pre_restore.id
        );
    }
    if !r.extra_files.is_empty() {
        let _ = writeln!(
            out,
            "kept {} file(s) that are not in the snapshot (use --delete-extra to remove them):",
            r.extra_files.len()
        );
        for f in &r.extra_files {
            let _ = writeln!(out, "  {f}");
        }
    }
    if !r.deleted_files.is_empty() {
        let _ = writeln!(
            out,
            "deleted {} file(s) that were not in the snapshot (recoverable from {}):",
            r.deleted_files.len(),
            r.pre_restore.id
        );
        for f in &r.deleted_files {
            let _ = writeln!(out, "  {f}");
        }
    }
    out
}

pub fn delete(r: &DeleteResult) -> String {
    format!("deleted snapshot {}\n", r.deleted)
}

pub fn prune(r: &PruneResult) -> String {
    if r.pruned.is_empty() {
        format!("nothing to prune ({} kept)\n", r.kept)
    } else {
        format!(
            "pruned {} snapshot(s): {} ({} kept)\n",
            r.pruned.len(),
            r.pruned.join(", "),
            r.kept
        )
    }
}
