//! Source Control coordinates repository work, retained reviews, and a stable panel.
use crate::{
    dev_container::Runtime,
    ui_theme::{self as theme, Icon},
};
use actions::{Pending, RefreshReply, Reply, ReviewReply, Ticket};
use eframe::egui;
use model::Selection;
use presentation::{Notice, configure_input_style};
use repository::Snapshot;
use std::{
    path::{Path, PathBuf},
    time::Instant,
};
mod actions;
mod diff;
mod diff_rows;
mod file_list;
mod fingerprint;
mod model;
mod panel;
mod patch;
mod patch_model;
mod polling;
mod presentation;
mod repository;
mod review;
mod stats;
#[cfg(test)]
mod tests;

#[derive(Default)]
pub(crate) struct GitUi {
    polling: polling::Polling,
    workspace: Option<PathBuf>,
    runtime: Option<Runtime>,
    snapshot: Option<Snapshot>,
    repository_generation: u64,
    data_generation: u64,
    refresh: Option<Pending<RefreshReply>>,
    pending: Option<Pending<Reply>>,
    review_request: Option<Pending<ReviewReply>>,
    refresh_requested: bool,
    refreshed: Option<Instant>,
    notice: Option<Notice>,
    refresh_error: Option<String>,
    commit: String,
    diff: Option<review::DiffView>,
    review_generation: u64,
    diff_target: Option<Selection>,
    diff_hidden: bool,
    review_status: review::ReviewStatus,
    open_review_requested: bool,
}
impl GitUi {
    /// Cached state only: rendering the status bar never invokes Git.
    pub(crate) fn status_summary(&self) -> (String, String) {
        if self.workspace.is_none() {
            return (
                "No workspace".into(),
                "Open a workspace to view Git state".into(),
            );
        }
        let Some(snapshot) = &self.snapshot else {
            return if let Some(error) = &self.refresh_error {
                ("Git unavailable".into(), error.clone())
            } else {
                ("Git loading…".into(), "Reading repository state".into())
            };
        };
        let tracking = &snapshot.tracking;
        let mut label = format!(
            "⑂ {}{}",
            snapshot.branch,
            if snapshot.files.is_empty() { "" } else { "*" }
        );
        if tracking.upstream.is_some() && tracking.counts_known {
            label.push_str(&format!(" ↑{} ↓{}", tracking.ahead, tracking.behind));
        } else if tracking.upstream.is_some() {
            label.push_str(" · Upstream unavailable");
        } else {
            label.push_str(" · No upstream");
        }
        if self.busy() {
            label.push_str(" · Working…");
        } else if self.refresh_error.is_some() {
            label.push_str(" · Stale");
        } else if self.refresh.is_some() {
            label.push_str(" · Refreshing…");
        }
        let mut details = format!(
            "{}\n{}\n{} saved working-tree changes",
            snapshot.root.display(),
            snapshot.branch,
            snapshot.files.len()
        );
        if tracking.unborn {
            details.push_str("\nNo commits yet");
        }
        if let Some(upstream) = &tracking.upstream {
            details.push_str(&format!("\nUpstream: {upstream}"));
            if tracking.counts_known {
                details.push_str(&format!(
                    "\nAhead {}, behind {} (locally known refs; no fetch)",
                    tracking.ahead, tracking.behind
                ));
            } else {
                details.push_str("\nAhead/behind unavailable: upstream ref is missing");
            }
        } else {
            details.push_str("\nNo upstream");
        }
        if let Some(error) = &self.refresh_error {
            details.push_str(&format!(
                "\nRefresh failed; showing previous state: {error}"
            ));
        }
        (label, details)
    }

    pub(crate) fn take_open_review_request(&mut self) -> bool {
        std::mem::take(&mut self.open_review_requested)
    }

    pub(crate) fn background_tick(
        &mut self,
        workspace: Option<&Path>,
        runtime: Option<&Runtime>,
        ctx: &egui::Context,
    ) {
        self.sync_workspace(workspace, runtime);
        self.poll_reply();
        if self.workspace.is_some() {
            self.schedule_refresh(ctx, false);
        }
    }
    fn ticket(&self) -> Ticket {
        Ticket {
            repository: self.repository_generation,
            data: self.data_generation,
        }
    }
    fn busy(&self) -> bool {
        self.pending.is_some()
            || self
                .workspace
                .as_deref()
                .is_some_and(crate::repository_jobs::busy)
    }
    pub(crate) fn external_change(&mut self) {
        self.data_generation = self.data_generation.wrapping_add(1);
        self.refresh = None;
        self.polling.reset();
        self.review_request = None;
        self.diff = None;
        self.refresh_requested = true;
    }
    fn sync_workspace(&mut self, workspace: Option<&Path>, runtime: Option<&Runtime>) {
        if self.workspace.as_deref() != workspace || self.runtime.as_ref() != runtime {
            *self = Self {
                workspace: workspace.map(Path::to_path_buf),
                runtime: runtime.cloned(),
                repository_generation: self.repository_generation.wrapping_add(1),
                ..Self::default()
            };
        }
    }
    pub(crate) fn ui(
        &mut self,
        ui: &mut egui::Ui,
        workspace: Option<&Path>,
        runtime: Option<&Runtime>,
    ) -> Option<PathBuf> {
        configure_input_style(ui);
        self.sync_workspace(workspace, runtime);
        self.poll_reply();
        self.panel_opened(ui.ctx());
        let manual_refresh = self.panel_header(ui);
        if workspace.is_none() {
            ui.vertical_centered(|ui| {
                theme::empty_art(ui, Icon::Git, theme::colors(ui).success);
                ui.strong("Open a workspace");
                ui.label("Review, stage, and commit its saved changes here.");
            });
            return None;
        }

        self.commit_form(ui);
        self.notice_area(ui);
        let open = self.change_groups(ui);
        self.schedule_refresh(ui.ctx(), manual_refresh);
        open
    }
}
