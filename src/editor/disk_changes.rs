//! Open-document metadata checks and explicit resolution of disk conflicts.
use super::EditorApp;
use crate::filesystem::{DiskVersion, poll_interval};
use eframe::egui;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::mpsc,
    time::Instant,
};

struct Document {
    id: u64,
    version: DiskVersion,
    dismissed: Option<DiskVersion>,
    text: String,
}
struct Conflict {
    path: PathBuf,
    version: DiskVersion,
    error: String,
}
type Check = (PathBuf, u64, DiskVersion, DiskVersion);
#[derive(Default)]
pub(super) struct DiskChanges {
    documents: HashMap<PathBuf, Document>,
    worker: Option<mpsc::Receiver<Vec<Check>>>,
    next_check: Option<Instant>,
    next_id: u64,
    conflict: Option<Conflict>,
}
impl DiskChanges {
    pub(super) fn remember(&mut self, path: &Path, text: String, version: DiskVersion) {
        self.next_id += 1;
        self.documents.insert(
            path.to_path_buf(),
            Document {
                id: self.next_id,
                version,
                dismissed: None,
                text,
            },
        );
        if self.conflict.as_ref().is_some_and(|c| c.path == path) {
            self.conflict = None;
        }
    }
    pub(super) fn forget(&mut self, path: &Path) {
        self.documents.remove(path);
        if self.conflict.as_ref().is_some_and(|c| c.path == path) {
            self.conflict = None;
        }
    }
    pub(super) fn has_conflict(&self) -> bool {
        self.conflict.is_some()
    }
    pub(super) fn cached_text(&self, path: &Path) -> Option<String> {
        self.documents.get(path).map(|doc| doc.text.clone())
    }
    pub(super) fn remap(&mut self, from: &Path, to: &Path) {
        let old = std::mem::take(&mut self.documents);
        for (path, doc) in old {
            match path.strip_prefix(from) {
                Ok(suffix) => {
                    let path = to.join(suffix);
                    self.remember(&path, doc.text, DiskVersion::read(&path));
                }
                Err(_) => {
                    self.documents.insert(path, doc);
                }
            }
        }
        self.conflict = None;
    }
    fn notify(&mut self, path: PathBuf, version: DiskVersion) {
        if self.conflict.is_none() {
            self.conflict = Some(Conflict {
                path,
                version,
                error: String::new(),
            });
        }
    }
}
impl EditorApp {
    pub(super) fn initialize_disk_changes(&mut self) {
        for path in self.open_files.clone() {
            let version = DiskVersion::read(&path);
            let text = if self.active_file.as_ref() == Some(&path) {
                self.buffer.clone()
            } else {
                std::fs::read_to_string(&path).unwrap_or_default()
            };
            self.disk_changes.remember(&path, text, version);
        }
    }
    pub(super) fn disk_save_conflict(&mut self, path: &Path) -> bool {
        let current = DiskVersion::read(path);
        if self
            .disk_changes
            .documents
            .get(path)
            .is_some_and(|doc| doc.version != current)
        {
            // Saving always asks again, including after Cancel.
            self.disk_changes.notify(path.to_path_buf(), current);
            return true;
        }
        false
    }
    pub(super) fn poll_disk_changes(&mut self, ctx: &egui::Context) {
        self.disk_changes
            .documents
            .retain(|path, _| self.open_files.contains(path));
        if self
            .disk_changes
            .conflict
            .as_ref()
            .is_some_and(|c| !self.open_files.contains(&c.path))
        {
            self.disk_changes.conflict = None;
        }
        if let Some(rx) = &self.disk_changes.worker {
            match rx.try_recv() {
                Ok(checks) => {
                    self.disk_changes.worker = None;
                    for (path, id, baseline, observed) in checks {
                        if self.disk_changes.documents.get(&path).is_some_and(|doc| {
                            doc.id == id
                                && doc.version == baseline
                                && baseline != observed
                                && doc.dismissed.as_ref() != Some(&observed)
                        }) {
                            self.disk_changes.notify(path, observed);
                        }
                    }
                }
                Err(mpsc::TryRecvError::Disconnected) => self.disk_changes.worker = None,
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        if self.disk_changes.documents.is_empty() {
            return;
        }
        let now = Instant::now();
        if self.disk_changes.worker.is_none()
            && self.disk_changes.next_check.is_none_or(|due| now >= due)
        {
            let paths: Vec<_> = self
                .disk_changes
                .documents
                .iter()
                .map(|(p, d)| (p.clone(), d.id, d.version.clone()))
                .collect();
            let (tx, rx) = mpsc::channel();
            let context = ctx.clone();
            std::thread::spawn(move || {
                let checks = paths
                    .into_iter()
                    .map(|(path, id, version)| {
                        let observed = DiskVersion::read(&path);
                        (path, id, version, observed)
                    })
                    .collect();
                let _ = tx.send(checks);
                context.request_repaint();
            });
            self.disk_changes.worker = Some(rx);
            self.disk_changes.next_check = Some(now + poll_interval());
        }
        if let Some(due) = self.disk_changes.next_check {
            if self.disk_changes.worker.is_none() {
                ctx.request_repaint_after(due.saturating_duration_since(now));
            }
        }
    }
    pub(super) fn disk_change_dialog(&mut self, ctx: &egui::Context) {
        let Some(mut conflict) = self.disk_changes.conflict.take() else {
            return;
        };
        let mut action = None;
        egui::Modal::new(egui::Id::new("disk_changed")).show(ctx, |ui| {
            ui.set_max_width(440.0_f32.min(ctx.viewport_rect().width() - 40.0));
            ui.heading("Local file on disk changed");
            ui.label(conflict.path.display().to_string());
            ui.label("Overwrite saves the editor contents to disk. Reload discards unsaved changes. Cancel keeps your edits. Merge changes manually if needed.");
            if !conflict.error.is_empty() { ui.colored_label(crate::ui_theme::colors(ui).error, &conflict.error); }
            ui.horizontal(|ui| {
                for label in ["Overwrite", "Reload", "Cancel"] {
                    if ui.button(label).clicked() { action = Some(label); }
                }
            });
        });
        if let Some(action) = action {
            if self
                .resolve_disk_change(&conflict.path, &conflict.version, action)
                .is_ok()
            {
                return;
            }
            conflict.error = self.status.clone();
        }
        self.disk_changes.conflict = Some(conflict);
    }
    pub(super) fn resolve_disk_change(
        &mut self,
        path: &Path,
        observed: &DiskVersion,
        action: &str,
    ) -> Result<(), ()> {
        if action == "Cancel" {
            if let Some(doc) = self.disk_changes.documents.get_mut(path) {
                doc.dismissed = Some(observed.clone());
            }
            return Ok(());
        }
        if action == "Overwrite" {
            if self.addons.worktree_busy() {
                self.status = "Wait for the Git operation before saving files".into();
                return Err(());
            }
            let text = if self.active_file.as_deref() == Some(path) {
                self.buffer.clone()
            } else if let Some(text) = self.unsaved_files.get(path) {
                text.clone()
            } else {
                self.disk_changes
                    .documents
                    .get(path)
                    .map(|d| d.text.clone())
                    .unwrap_or_default()
            };
            if let Err(error) = std::fs::write(path, &text) {
                self.status = format!("Could not overwrite file: {error}");
                return Err(());
            }
            if self.active_file.as_deref() == Some(path) {
                self.dirty = false;
            }
            self.unsaved_files.remove(path);
            self.disk_changes
                .remember(path, text, DiskVersion::read(path));
            self.git_ui.external_change();
        } else {
            let version = DiskVersion::read(path);
            let text = match std::fs::read_to_string(path) {
                Ok(text) => text,
                Err(error) => {
                    self.status = format!("Could not reload file: {error}");
                    return Err(());
                }
            };
            if self.active_file.as_deref() == Some(path) {
                self.buffer = text.clone();
                self.dirty = false;
                self.pending_editor_cursor = None;
                self.find_active = None;
            }
            // A new identity also clears stale undo history containing discarded edits.
            self.next_document_id += 1;
            self.document_ids.insert(
                path.to_path_buf(),
                egui::Id::new(("document_editor", self.next_document_id)),
            );
            self.unsaved_files.remove(path);
            self.disk_changes.remember(path, text, version);
        }
        self.status = if action == "Overwrite" {
            "File overwritten"
        } else {
            "File reloaded"
        }
        .into();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::tests::test_app;

    fn document() -> (tempfile::TempDir, PathBuf, EditorApp) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file.txt");
        std::fs::write(&path, "original").unwrap();
        let mut app = test_app();
        app.open_files.clear();
        app.active_file = None;
        app.dirty = false;
        assert!(app.open_file(path.clone()));
        app.buffer = "my edits".into();
        app.mark_modified();
        (dir, path, app)
    }
    #[test]
    fn saving_after_external_change_and_cancel_requires_explicit_overwrite() {
        let (_dir, path, mut app) = document();
        std::fs::write(&path, "external changes").unwrap();
        assert!(!app.save_document(&path));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "external changes");
        let observed = DiskVersion::read(&path);
        app.resolve_disk_change(&path, &observed, "Cancel").unwrap();
        app.disk_changes.conflict = None;
        assert_eq!(app.buffer, "my edits");
        assert!(app.dirty);
        assert!(!app.save_document(&path));
        assert!(app.disk_changes.conflict.is_some());
        app.resolve_disk_change(&path, &observed, "Overwrite")
            .unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "my edits");
        assert!(!app.dirty);
        assert!(!app.disk_save_conflict(&path));
    }
    #[test]
    fn failed_reload_keeps_edits_and_successful_reload_resets_history() {
        let (_dir, path, mut app) = document();
        let id = app.editor_id();
        std::fs::remove_file(&path).unwrap();
        assert!(
            app.resolve_disk_change(&path, &DiskVersion::Missing, "Reload")
                .is_err()
        );
        assert_eq!(app.buffer, "my edits");
        assert!(app.dirty);
        std::fs::write(&path, "new disk contents").unwrap();
        app.resolve_disk_change(&path, &DiskVersion::read(&path), "Reload")
            .unwrap();
        assert_eq!(app.buffer, "new disk contents");
        assert!(!app.dirty);
        assert_ne!(app.editor_id(), id);
    }
    #[test]
    fn inactive_dirty_document_reload_does_not_replace_active_buffer() {
        let (_dir, path, mut app) = document();
        let other = path.with_file_name("other.txt");
        std::fs::write(&other, "other").unwrap();
        assert!(app.open_file(other.clone()));
        std::fs::write(&path, "external").unwrap();
        app.resolve_disk_change(&path, &DiskVersion::read(&path), "Reload")
            .unwrap();
        assert_eq!(app.buffer, "other");
        assert!(!app.unsaved_files.contains_key(&path));
        assert!(app.activate_file(path));
        assert_eq!(app.buffer, "external");
    }
    #[test]
    fn poll_notifies_once_per_disk_version_and_empty_editor_does_not_poll() {
        let (_dir, path, mut app) = document();
        let ctx = egui::Context::default();
        std::fs::write(&path, "external").unwrap();
        fn check(app: &mut EditorApp, ctx: &egui::Context) {
            app.disk_changes.next_check = None;
            app.poll_disk_changes(ctx);
            app.disk_changes.next_check = Some(Instant::now() + std::time::Duration::from_secs(30));
            let deadline = Instant::now() + std::time::Duration::from_secs(5);
            while app.disk_changes.worker.is_some() {
                app.poll_disk_changes(ctx);
                assert!(Instant::now() < deadline);
                std::thread::yield_now();
            }
        }
        check(&mut app, &ctx);
        let conflict = app.disk_changes.conflict.take().unwrap();
        app.resolve_disk_change(&path, &conflict.version, "Cancel")
            .unwrap();
        check(&mut app, &ctx);
        assert!(app.disk_changes.conflict.is_none());
        std::fs::write(&path, "another external version").unwrap();
        check(&mut app, &ctx);
        assert!(app.disk_changes.conflict.is_some());
        app.open_files.clear();
        app.poll_disk_changes(&ctx);
        assert!(app.disk_changes.documents.is_empty());
        assert!(app.disk_changes.worker.is_none());
    }
    #[test]
    fn late_poll_cannot_override_a_successful_save() {
        let (_dir, path, mut app) = document();
        let doc = app.disk_changes.documents.get(&path).unwrap();
        let stale = (
            path.clone(),
            doc.id,
            doc.version.clone(),
            DiskVersion::Missing,
        );
        assert!(app.save_document(&path));
        let (tx, rx) = mpsc::channel();
        tx.send(vec![stale]).unwrap();
        app.disk_changes.worker = Some(rx);
        app.disk_changes.next_check = Some(Instant::now() + std::time::Duration::from_secs(30));
        app.poll_disk_changes(&egui::Context::default());
        assert!(app.disk_changes.conflict.is_none());
    }
}
