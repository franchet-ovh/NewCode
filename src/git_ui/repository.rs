//! Git command execution, status parsing, snapshots, and conflict classification.

use super::stats::{self, LineStats};
use crate::dev_container::Runtime;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::Command,
};

#[cfg(test)]
pub(super) fn git(root: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
    git_with(root, args, None)
}

pub(super) fn git_with(
    root: &Path,
    args: &[&str],
    runtime: Option<&Runtime>,
) -> Result<Vec<u8>, String> {
    let mut command = if let Some(runtime) = runtime {
        let remote_root = runtime.container_path(root)?;
        let mut command = runtime.command("git");
        command.arg("-C").arg(remote_root);
        command
    } else {
        let mut command = Command::new("git");
        command.current_dir(root);
        command
    };
    let output = command
        .env("GIT_LITERAL_PATHSPECS", "1")
        .args(args)
        .output()
        .map_err(|e| e.to_string())?;
    if output.status.success() {
        Ok(output.stdout)
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_owned())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Change {
    pub(super) path: String,
    pub(super) old: Option<String>,
    pub(super) index: char,
    pub(super) work: char,
}

pub(super) fn changes(bytes: &[u8]) -> Vec<Change> {
    let mut fields = bytes.split(|b| *b == 0).filter(|s| !s.is_empty());
    let mut result = Vec::new();
    while let Some(field) = fields.next() {
        if field.len() < 4 {
            continue;
        }
        let index = field[0] as char;
        let work = field[1] as char;
        let old = if matches!(index, 'R' | 'C') || matches!(work, 'R' | 'C') {
            fields
                .next()
                .map(|s| String::from_utf8_lossy(s).into_owned())
        } else {
            None
        };
        result.push(Change {
            path: String::from_utf8_lossy(&field[3..]).into_owned(),
            old,
            index,
            work,
        });
    }
    result
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct Tracking {
    pub upstream: Option<String>,
    pub ahead: u64,
    pub behind: u64,
    pub counts_known: bool,
    pub detached: bool,
    pub unborn: bool,
}

fn branch_status(bytes: &[u8]) -> (String, Tracking) {
    let text = String::from_utf8_lossy(bytes);
    let mut branch = String::new();
    let mut oid = String::new();
    let mut tracking = Tracking::default();
    for field in text.split('\0') {
        if let Some(value) = field.strip_prefix("# branch.head ") {
            tracking.detached = value == "(detached)";
            branch = value.to_owned();
        } else if let Some(value) = field.strip_prefix("# branch.oid ") {
            tracking.unborn = value == "(initial)";
            oid = value.to_owned();
        } else if let Some(value) = field.strip_prefix("# branch.upstream ") {
            tracking.upstream = Some(value.to_owned());
        } else if let Some(value) = field.strip_prefix("# branch.ab ") {
            tracking.counts_known = true;
            for count in value.split_whitespace() {
                if let Some(value) = count.strip_prefix('+') {
                    tracking.ahead = value.parse().unwrap_or(0);
                } else if let Some(value) = count.strip_prefix('-') {
                    tracking.behind = value.parse().unwrap_or(0);
                }
            }
        }
    }
    if tracking.detached {
        branch = format!("Detached HEAD ({})", &oid[..oid.len().min(8)]);
    }
    (branch, tracking)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Snapshot {
    pub(super) root: PathBuf,
    pub(super) branch: String,
    pub(super) tracking: Tracking,
    pub(super) files: Vec<Change>,
    pub(super) counts: [HashMap<String, LineStats>; 2],
}

#[cfg(test)]
pub(super) fn snapshot(workspace: &Path) -> Result<Snapshot, String> {
    snapshot_with(workspace, None)
}

pub(super) fn snapshot_with(
    workspace: &Path,
    runtime: Option<&Runtime>,
) -> Result<Snapshot, String> {
    let root = git_with(workspace, &["rev-parse", "--show-toplevel"], runtime)?;
    let root = PathBuf::from(String::from_utf8_lossy(&root).trim_end());
    let root = runtime.map_or(Ok(root.clone()), |runtime| runtime.host_path(&root))?;
    let (branch, tracking) = branch_status(&git_with(
        &root,
        &[
            "status",
            "--porcelain=v2",
            "--branch",
            "-z",
            "--untracked-files=no",
        ],
        runtime,
    )?);
    let files = changes(&git_with(
        &root,
        &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
        runtime,
    )?);
    let counts = |cached: bool| -> Result<HashMap<String, LineStats>, String> {
        let mut args = vec!["diff", "--numstat", "-z", "--find-renames"];
        if cached {
            args.push("--cached");
        }
        Ok(stats::numstat(&git_with(&root, &args, runtime)?))
    };
    let mut counts = [counts(true)?, counts(false)?];
    for change in &files {
        if change.index == '?' {
            counts[1].insert(
                change.path.clone(),
                stats::untracked(&root.join(&change.path)),
            );
        }
    }
    Ok(Snapshot {
        counts,
        root,
        branch,
        tracking,
        files,
    })
}

pub(super) fn conflict(c: &Change) -> bool {
    c.index == 'U'
        || c.work == 'U'
        || (c.index == 'A' && c.work == 'A')
        || (c.index == 'D' && c.work == 'D')
}

#[cfg(test)]
mod status_tests {
    use super::*;

    #[test]
    fn parses_branch_tracking_without_human_readable_status() {
        for (ahead, behind) in [(0, 0), (3, 0), (0, 2), (3, 2)] {
            let (branch, state) = branch_status(format!("# branch.oid abcdef123456\0# branch.head feature/topic\0# branch.upstream origin/feature/topic\0# branch.ab +{ahead} -{behind}\0").as_bytes());
            assert_eq!(branch, "feature/topic");
            assert_eq!(state.upstream.as_deref(), Some("origin/feature/topic"));
            assert_eq!((state.ahead, state.behind), (ahead, behind));
            assert!(state.counts_known);
            assert!(!state.detached && !state.unborn);
        }
        let (_, missing_upstream) =
            branch_status(b"# branch.head main\0# branch.upstream origin/main\0");
        assert!(!missing_upstream.counts_known);
        let (branch, _) =
            branch_status(b"# branch.head main\0# branch.ab +0 -0\x001 file\n# branch.head fake\0");
        assert_eq!(branch, "main");
        let (branch, state) = branch_status(b"# branch.oid (initial)\0# branch.head main\0");
        assert_eq!(branch, "main");
        assert!(state.unborn);
        assert!(state.upstream.is_none());
        let (branch, state) =
            branch_status(b"# branch.oid abcdef123456\0# branch.head (detached)\0");
        assert_eq!(branch, "Detached HEAD (abcdef12)");
        assert!(state.detached);
    }

    #[test]
    fn real_repository_reports_unborn_upstream_and_detached_head() {
        let root = tempfile::tempdir().unwrap();
        let run = |args: &[&str]| git(root.path(), args).unwrap();
        run(&["init", "-q", "-b", "main"]);
        let initial = snapshot(root.path()).unwrap();
        assert_eq!(initial.branch, "main");
        assert!(initial.tracking.unborn);
        run(&[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "core.hooksPath=/dev/null",
            "commit",
            "--allow-empty",
            "-qm",
            "initial",
        ]);
        run(&["branch", "base"]);
        run(&["branch", "--set-upstream-to=base", "main"]);
        let tracked = snapshot(root.path()).unwrap();
        assert_eq!(tracked.tracking.upstream.as_deref(), Some("base"));
        assert_eq!((tracked.tracking.ahead, tracked.tracking.behind), (0, 0));
        run(&["checkout", "--detach", "-q"]);
        assert!(snapshot(root.path()).unwrap().tracking.detached);
    }
}
