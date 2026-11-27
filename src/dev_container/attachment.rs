//! Attach to an existing bind-mounted workspace without provisioning its container.
use super::{DevContainer, Event, Runtime, State, diagnostics};
use eframe::egui;
use serde::Deserialize;
use std::{
    path::{Component, Path, PathBuf},
    process::Command,
    sync::mpsc,
    thread,
};

#[derive(Clone, Debug)]
pub(crate) struct Request {
    pub container: String,
    pub folder: PathBuf,
}

#[derive(Clone, Debug)]
pub(crate) struct Attachment {
    pub request: Request,
    pub runtime: Runtime,
}

pub(super) struct Session {
    pub attachment: Attachment,
    reconnecting: bool,
    failed: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Inspection {
    id: String,
    state: ContainerState,
    mounts: Vec<Mount>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct ContainerState {
    running: bool,
    paused: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Mount {
    #[serde(rename = "Type")]
    kind: String,
    #[serde(default)]
    source: PathBuf,
    destination: PathBuf,
}

impl Request {
    pub fn validate(&self) -> Result<(), String> {
        let name = self.container.strip_prefix('/').unwrap_or(&self.container);
        if name.is_empty()
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"_.-".contains(&byte))
            || !name.as_bytes()[0].is_ascii_alphanumeric()
        {
            return Err("--attach-container requires a container name or ID".into());
        }
        if !self.folder.is_absolute() {
            return Err("--container-workspace requires an absolute container directory".into());
        }
        Ok(())
    }

    pub fn resolve(&self) -> Result<Attachment, String> {
        self.resolve_with(|command, operation| {
            let output = command
                .output()
                .map_err(|error| format!("Cannot {operation}: {error}"))?;
            if !output.status.success() {
                // Never log inspect stdout: other Docker output may contain environment secrets.
                return Err(format!(
                    "Cannot {operation} ({}): {}",
                    output.status,
                    crate::logging::redaction::sanitize(&String::from_utf8_lossy(&output.stderr))
                        .join("\n")
                ));
            }
            Ok(output.stdout)
        })
    }

    fn resolve_with(
        &self,
        mut run: impl FnMut(&mut Command, &str) -> Result<Vec<u8>, String>,
    ) -> Result<Attachment, String> {
        self.validate()?;
        let inspection = run(
            Command::new("docker").args([
                "inspect",
                "--type",
                "container",
                "--format",
                r#"{"Id":{{json .Id}},"State":{"Running":{{json .State.Running}},"Paused":{{json .State.Paused}}},"Mounts":{{json .Mounts}}}"#,
                &self.container,
            ]),
            "inspect attached container",
        )?;
        let inspection: Inspection = serde_json::from_slice(&inspection)
            .map_err(|_| "Cannot parse attached container inspection")?;
        if !inspection.state.running || inspection.state.paused {
            return Err("Attached container must be running and not paused".into());
        }
        if inspection.id.is_empty() || inspection.id.starts_with('-') {
            return Err("Docker returned an invalid container ID".into());
        }
        let directory = run(
            Command::new("docker")
                .args(["exec", "--workdir"])
                .arg(&self.folder)
                .args([&inspection.id, "/bin/sh", "-c", "pwd -P"]),
            "open attached container workspace",
        )?;
        let directory =
            String::from_utf8(directory).map_err(|_| "Container workspace path must be UTF-8")?;
        let folder = PathBuf::from(directory.strip_suffix('\n').unwrap_or(&directory));
        if !folder.is_absolute() || folder.components().any(|c| c == Component::ParentDir) {
            return Err("Docker returned an invalid container workspace path".into());
        }
        let host_workspace = host_workspace(&inspection.mounts, &folder)?;
        Ok(Attachment {
            request: self.clone(),
            runtime: Runtime {
                host_workspace,
                container_id: inspection.id,
                workspace_folder: folder,
                remote_user: None,
            },
        })
    }
}

fn host_workspace(mounts: &[Mount], folder: &Path) -> Result<PathBuf, String> {
    // Select before checking the kind: a child volume masks a parent bind mount.
    let mount = mounts
        .iter()
        .filter(|mount| folder.starts_with(&mount.destination))
        .max_by_key(|mount| mount.destination.components().count())
        .ok_or("Container workspace must be backed by a host-accessible bind mount")?;
    if mount.kind != "bind" || !mount.source.is_absolute() {
        return Err("Container workspace must be backed by a host-accessible bind mount; Docker volumes are not supported".into());
    }
    let relative = folder.strip_prefix(&mount.destination).unwrap();
    let host = mount.source.join(relative);
    let host = host.canonicalize().map_err(|error| {
        format!(
            "Cannot access bind-mounted workspace {} on the editor host: {error}",
            host.display()
        )
    })?;
    if !host.is_dir() {
        return Err("Bind-mounted workspace must be an existing host directory".into());
    }
    Ok(host)
}

impl DevContainer {
    pub(crate) fn attached(attachment: Attachment) -> Self {
        log::info!(target: "focus_editor::dev_container", "Attached to running container {}", attachment.request.container);
        Self {
            workspace: Some(attachment.runtime.host_workspace.clone()),
            state: State::Attached(Session {
                attachment,
                reconnecting: false,
                failed: false,
            }),
            events: None,
        }
    }

    pub(super) fn attachment_menu(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let State::Attached(session) = &self.state else {
            return;
        };
        let name = &session.attachment.request.container;
        ui.label(if session.reconnecting {
            format!("Reconnecting to {name}…")
        } else if session.failed {
            format!("Reconnect to {name} failed — see application log")
        } else {
            format!("Connected to {name}")
        });
        let reconnect = ui
            .add_enabled(
                !session.reconnecting,
                focus_addon_support::ui_theme::control_button("Reconnect"),
            )
            .clicked();
        let detach = ui
            .add(focus_addon_support::ui_theme::control_button("Detach"))
            .clicked();
        if detach {
            self.detach_attachment();
            ui.close();
        } else if reconnect {
            self.reconnect_attachment(ctx.clone());
            ui.close();
        }
    }

    fn reconnect_attachment(&mut self, ctx: egui::Context) {
        let State::Attached(session) = &mut self.state else {
            return;
        };
        if session.reconnecting {
            return;
        }
        session.reconnecting = true;
        let previous = session.attachment.clone();
        let (tx, rx) = mpsc::channel();
        self.events = Some(rx);
        thread::spawn(move || {
            let result = previous.request.resolve();
            let _ = tx.send(Event::Reattached(result));
            ctx.request_repaint();
        });
    }

    pub(super) fn reattached(&mut self, result: Result<Attachment, String>) -> bool {
        let State::Attached(session) = &mut self.state else {
            return false;
        };
        session.reconnecting = false;
        match result.and_then(|attachment| {
            same_workspace(&session.attachment, &attachment)?;
            Ok(attachment)
        }) {
            Ok(attachment) => {
                session.attachment = attachment;
                session.failed = false;
                log::info!(target: "focus_editor::dev_container", "Dev Container reconnect completed; connected");
                true
            }
            Err(error) => {
                session.failed = true;
                diagnostics::failure("reconnect", &error);
                // Keep routing tools into the previous container, never onto the host.
                false
            }
        }
    }

    fn detach_attachment(&mut self) {
        self.state = State::Unavailable;
        let (tx, rx) = mpsc::channel();
        self.events = Some(rx);
        let _ = tx.send(Event::Detached);
        log::info!(target: "focus_editor::dev_container", "Detached from container; container left running");
    }
}

fn same_workspace(previous: &Attachment, next: &Attachment) -> Result<(), String> {
    if previous.runtime.host_workspace != next.runtime.host_workspace {
        return Err("Attached workspace now maps to a different host directory; relaunch with the container name and folder".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests;
