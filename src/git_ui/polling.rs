//! Idle Git work is bounded by known changed paths, never the entire workspace.
use super::{
    GitUi,
    actions::{Pending, RefreshReply, receive},
    fingerprint::FileVersion,
    repository::{Snapshot, git_with, snapshot_at},
    review::ReviewUpdate,
};
use crate::{
    dev_container::Runtime,
    filesystem::{DiskVersion, poll_interval},
};
use eframe::egui;
use std::{collections::BTreeMap, path::PathBuf, time::Instant};

#[derive(Default)]
pub(super) struct Polling {
    versions: BTreeMap<PathBuf, DiskVersion>,
    metadata: Vec<PathBuf>,
    pending: Option<Pending<Result<PollReply, String>>>,
    due: Option<Instant>,
    panel_frame: Option<u64>,
}
struct PollReply {
    versions: BTreeMap<PathBuf, DiskVersion>,
    metadata: Vec<PathBuf>,
    refresh: Option<RefreshReply>,
}
impl Polling {
    pub(super) fn reset(&mut self) {
        self.versions.clear();
        self.metadata.clear();
        self.pending = None;
        self.due = None;
    }
}
fn metadata_paths(snapshot: &Snapshot, runtime: Option<&Runtime>) -> Result<Vec<PathBuf>, String> {
    let resolve = |flag| -> Result<PathBuf, String> {
        let bytes = git_with(
            &snapshot.root,
            &["rev-parse", "--path-format=absolute", flag],
            runtime,
        )?;
        let path = PathBuf::from(String::from_utf8_lossy(&bytes).trim_end());
        runtime.map_or(Ok(path.clone()), |r| r.host_path(&path))
    };
    let directory = resolve("--git-dir")?;
    let common = resolve("--git-common-dir")?;
    let mut paths = vec![
        directory.join("index"),
        directory.join("HEAD"),
        common.join("packed-refs"),
        common.join("config"),
        common.join("shallow"),
    ];
    if let Ok(head) = std::fs::read_to_string(directory.join("HEAD")) {
        if let Some(reference) = head.trim().strip_prefix("ref: ") {
            paths.push(common.join(reference));
        }
    }
    if let Some(upstream) = &snapshot.tracking.upstream {
        paths.push(common.join("refs/remotes").join(upstream));
    }
    Ok(paths)
}
fn versions(snapshot: &Snapshot, metadata: &[PathBuf]) -> BTreeMap<PathBuf, DiskVersion> {
    metadata
        .iter()
        .cloned()
        .chain(snapshot.files.iter().flat_map(|c| {
            std::iter::once(snapshot.root.join(&c.path))
                .chain(c.old.as_ref().map(|p| snapshot.root.join(p)))
        }))
        .map(|p| {
            let v = DiskVersion::read(&p);
            (p, v)
        })
        .collect()
}
impl GitUi {
    pub(super) fn panel_opened(&mut self, ctx: &egui::Context) {
        let frame = ctx.cumulative_frame_nr();
        if self
            .polling
            .panel_frame
            .is_none_or(|previous| frame > previous + 1)
        {
            self.refresh_requested = true;
        }
        self.polling.panel_frame = Some(frame);
    }
    pub(super) fn schedule_poll(&mut self, ctx: &egui::Context) {
        let now = Instant::now();
        if self.polling.pending.is_some() {
            return;
        }
        if let Some(due) = self.polling.due {
            if now < due {
                ctx.request_repaint_after(due - now);
                return;
            }
        }
        let Some(snapshot) = self.snapshot.clone() else {
            return;
        };
        let runtime = self.runtime.clone();
        let before = self.polling.versions.clone();
        let metadata = self.polling.metadata.clone();
        let review = self
            .diff
            .as_ref()
            .map(|v| (v.id, v.path.clone(), v.version.clone()));
        self.polling.pending = Some(Pending::spawn(self.ticket(), false, ctx, move || {
            let metadata = if metadata.is_empty() {
                metadata_paths(&snapshot, runtime.as_ref())?
            } else {
                metadata
            };
            let current = versions(&snapshot, &metadata);
            if !before.is_empty() && before == current {
                return Ok(PollReply {
                    versions: current,
                    metadata,
                    refresh: None,
                });
            }
            let index_changed =
                before.is_empty() || metadata.iter().any(|p| before.get(p) != current.get(p));
            let mut paths: Vec<String> = snapshot
                .files
                .iter()
                .flat_map(|c| std::iter::once(c.path.clone()).chain(c.old.clone()))
                .collect();
            if index_changed {
                // Index enumeration does not examine unrelated working-tree files.
                let staged = git_with(
                    &snapshot.root,
                    &["diff", "--cached", "--name-only", "-z", "--no-renames"],
                    runtime.as_ref(),
                )?;
                paths.extend(
                    staged
                        .split(|b| *b == 0)
                        .filter(|p| !p.is_empty())
                        .map(|p| String::from_utf8_lossy(p).into_owned()),
                );
            }
            paths.sort();
            paths.dedup();
            let invalidation = review.and_then(|(id, path, version)| {
                (FileVersion::read(&snapshot.root, &path, runtime.as_ref())
                    .map_or(true, |v| v != version))
                .then(|| ReviewUpdate::Invalidated {
                    id,
                    reason: "File or index changed. Reload diff to continue.".into(),
                })
            });
            let updated = snapshot_at(snapshot.root.clone(), runtime.as_ref(), Some(&paths));
            let metadata = if let Ok(updated) = &updated {
                metadata_paths(updated, runtime.as_ref())?
            } else {
                metadata
            };
            // Keep the pre-read versions: changes racing the status read will be checked next cycle.
            Ok(PollReply {
                versions: current,
                metadata,
                refresh: Some(RefreshReply {
                    snapshot: updated,
                    invalidation,
                }),
            })
        }));
        self.polling.due = Some(now + poll_interval());
    }
    pub(super) fn poll_metadata_reply(&mut self) {
        let Some((ticket, result)) = receive(&mut self.polling.pending) else {
            return;
        };
        if ticket != self.ticket() {
            return;
        }
        match result.and_then(|r| r) {
            Ok(reply) => {
                self.polling.versions = reply.versions;
                self.polling.metadata = reply.metadata;
                if let Some(refresh) = reply.refresh {
                    self.apply_snapshot(refresh.snapshot);
                    if let Some(update) = refresh.invalidation {
                        self.accept_review(update);
                    }
                }
            }
            Err(error) => self.apply_snapshot(Err(error)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::repository::snapshot;
    struct Repo {
        root: PathBuf,
        _directory: tempfile::TempDir,
    }
    impl Repo {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let repo = Self {
                root: dir.path().to_path_buf(),
                _directory: dir,
            };
            for args in [
                vec!["init", "-q"],
                vec!["config", "user.name", "Test"],
                vec!["config", "user.email", "test@example.invalid"],
                vec!["config", "commit.gpgsign", "false"],
            ] {
                repo.run(&args);
            }
            repo
        }
        fn write(&self, name: &str, text: &str) {
            std::fs::write(self.root.join(name), text).unwrap();
        }
        fn run(&self, args: &[&str]) {
            git_with(&self.root, args, None).unwrap();
        }
    }
    use super::*;
    use std::time::Duration;

    fn poll(state: &mut GitUi) {
        state.polling.due = None;
        state.schedule_poll(&egui::Context::default());
        let deadline = Instant::now() + Duration::from_secs(5);
        while state.polling.pending.is_some() {
            state.poll_metadata_reply();
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(state.refresh_error.is_none(), "{:?}", state.refresh_error);
    }
    #[test]
    fn scoped_poll_discovers_staged_paths_but_not_unrelated_worktree_changes() {
        let repo = Repo::new();
        repo.write("known", "initial\n");
        repo.write("unrelated", "initial\n");
        repo.run(&["add", "."]);
        repo.run(&["-c", "core.hooksPath=/dev/null", "commit", "-qm", "initial"]);
        repo.write("known", "modified\n");
        let mut state = GitUi {
            workspace: Some(repo.root.clone()),
            snapshot: Some(snapshot(&repo.root).unwrap()),
            ..Default::default()
        };
        poll(&mut state);
        let refreshed = state.refreshed;
        // Removing the git executable from PATH is unnecessary: an unchanged poll must not even apply a snapshot.
        poll(&mut state);
        assert_eq!(state.refreshed, refreshed);
        repo.write("unrelated", "external modification\n");
        repo.write("new", "untracked\n");
        repo.write("known", "modified again\n");
        poll(&mut state);
        assert_eq!(state.snapshot.as_ref().unwrap().files.len(), 1);
        repo.run(&["add", "unrelated"]);
        poll(&mut state);
        let files = &state.snapshot.as_ref().unwrap().files;
        assert!(
            files
                .iter()
                .any(|c| c.path == "unrelated" && c.index == 'M')
        );
        assert!(!files.iter().any(|c| c.path == "new"));
        assert!(
            snapshot(&repo.root)
                .unwrap()
                .files
                .iter()
                .any(|c| c.path == "new")
        );
        repo.write("known", "initial\n");
        poll(&mut state);
        assert!(
            !state
                .snapshot
                .as_ref()
                .unwrap()
                .files
                .iter()
                .any(|c| c.path == "known")
        );
    }
    #[test]
    fn clean_repository_scope_stays_empty_until_explicit_discovery() {
        let repo = Repo::new();
        let mut state = GitUi {
            workspace: Some(repo.root.clone()),
            snapshot: Some(snapshot(&repo.root).unwrap()),
            ..Default::default()
        };
        repo.write("new", "untracked\n");
        poll(&mut state);
        poll(&mut state);
        assert!(state.snapshot.as_ref().unwrap().files.is_empty());
    }
}
