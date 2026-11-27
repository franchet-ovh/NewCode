use super::*;
use serde_json::json;
use std::fs;

fn request(folder: &str) -> Request {
    Request {
        container: "/my-container".into(),
        folder: folder.into(),
    }
}

fn inspection(source: &Path) -> serde_json::Value {
    json!({"Id":"pinned-container-id", "State":{"Running":true,"Paused":false},
        "Mounts":[{"Type":"bind","Source":source,"Destination":"/workspace"}]})
}

fn resolve(data: &serde_json::Value, folder: &str) -> Result<Attachment, String> {
    request(folder).resolve_with(|command, _| {
        let args: Vec<_> = command.get_args().collect();
        if args[0] == "inspect" {
            Ok(serde_json::to_vec(data).unwrap())
        } else {
            Ok(format!("{folder}\n").into_bytes())
        }
    })
}

#[test]
fn resolves_directory_using_immutable_id_and_default_user_without_shell_interpolation() {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join("sub project")).unwrap();
    let mut calls = Vec::new();
    let attachment = request("/alias/sub project")
        .resolve_with(|command, operation| {
            assert_eq!(command.get_program(), "docker");
            let args: Vec<_> = command
                .get_args()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect();
            calls.push((args.clone(), operation.to_owned()));
            match args[0].as_str() {
                "inspect" => {
                    assert_eq!(args.last().unwrap(), "/my-container");
                    assert!(!args[4].contains(".Config"));
                    Ok(serde_json::to_vec(&inspection(root.path())).unwrap())
                }
                "exec" => {
                    assert_eq!(
                        args,
                        [
                            "exec",
                            "--workdir",
                            "/alias/sub project",
                            "pinned-container-id",
                            "/bin/sh",
                            "-c",
                            "pwd -P"
                        ]
                    );
                    Ok(b"/workspace/sub project\n".to_vec())
                }
                other => panic!("Unexpected Docker operation {other}"),
            }
        })
        .unwrap();
    assert_eq!(calls.len(), 2);
    assert_eq!(
        attachment.runtime.host_workspace,
        root.path().join("sub project").canonicalize().unwrap()
    );
    assert_eq!(
        attachment.runtime.workspace_folder,
        Path::new("/workspace/sub project")
    );
    assert_eq!(attachment.runtime.remote_user, None);
    assert_eq!(attachment.request.folder, Path::new("/alias/sub project"));
    assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
}

#[test]
fn selects_deepest_mount_and_never_falls_back_through_a_volume() {
    let outer = tempfile::tempdir().unwrap();
    let inner = tempfile::tempdir().unwrap();
    fs::create_dir(inner.path().join("nested")).unwrap();
    let mut data = inspection(outer.path());
    data["Mounts"].as_array_mut().unwrap().push(json!({
        "Type":"bind", "Source":inner.path(), "Destination":"/workspace/project"
    }));
    assert_eq!(
        resolve(&data, "/workspace/project/nested")
            .unwrap()
            .runtime
            .host_workspace,
        inner.path().join("nested").canonicalize().unwrap()
    );
    data["Mounts"][1]["Type"] = json!("volume");
    assert!(
        resolve(&data, "/workspace/project")
            .unwrap_err()
            .contains("volumes are not supported")
    );
    assert!(
        resolve(&data, "/workspace-other")
            .unwrap_err()
            .contains("bind mount")
    );
    assert_eq!(
        resolve(&data, "/workspace").unwrap().runtime.host_workspace,
        outer.path().canonicalize().unwrap()
    );
}

#[test]
fn rejects_stopped_paused_unmounted_or_inaccessible_workspaces() {
    let root = tempfile::tempdir().unwrap();
    for (running, paused) in [(false, false), (true, true)] {
        let mut data = inspection(root.path());
        data["State"] = json!({"Running":running,"Paused":paused});
        let error = request("/workspace")
            .resolve_with(|_, operation| {
                assert_eq!(operation, "inspect attached container");
                Ok(serde_json::to_vec(&data).unwrap())
            })
            .unwrap_err();
        assert!(error.contains("must be running"));
    }
    assert!(
        resolve(&inspection(&root.path().join("missing")), "/workspace")
            .unwrap_err()
            .contains("Cannot access bind-mounted workspace")
    );
    assert!(resolve(&inspection(root.path()), "/workspace/missing").is_err());
    fs::write(root.path().join("file"), "fixture").unwrap();
    assert!(
        resolve(&inspection(root.path()), "/workspace/file")
            .unwrap_err()
            .contains("directory")
    );
    assert!(
        resolve(&json!({}), "/workspace")
            .unwrap_err()
            .contains("parse")
    );
    assert!(
        request("/workspace")
            .resolve_with(|_, _| Err("Docker unavailable".into()))
            .unwrap_err()
            .contains("Docker unavailable")
    );
}

#[test]
fn canonicalizes_symlinked_host_sources() {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join("real")).unwrap();
    std::os::unix::fs::symlink(root.path().join("real"), root.path().join("alias")).unwrap();
    assert_eq!(
        resolve(&inspection(&root.path().join("alias")), "/workspace")
            .unwrap()
            .runtime
            .host_workspace,
        root.path().join("real").canonicalize().unwrap()
    );
}

#[test]
fn workspace_changes_drop_attachment_without_loading_its_project_configuration() {
    let root = tempfile::tempdir().unwrap();
    fs::write(
        root.path().join(".devcontainer.json"),
        "invalid configuration",
    )
    .unwrap();
    let attachment = resolve(&inspection(root.path()), "/workspace").unwrap();
    let mut container = DevContainer::attached(attachment.clone());
    container.sync_workspace(Some(&attachment.runtime.host_workspace));
    assert_eq!(container.runtime(), Some(&attachment.runtime));
    let next = tempfile::tempdir().unwrap();
    container.sync_workspace(Some(next.path()));
    assert!(container.runtime().is_none());
    assert!(matches!(container.state, State::Unavailable));
}

#[test]
fn reconnect_failure_preserves_runtime_and_replacement_requires_same_host_workspace() {
    let root = tempfile::tempdir().unwrap();
    let original = resolve(&inspection(root.path()), "/workspace").unwrap();
    let mut container = DevContainer::attached(original.clone());
    assert!(!container.reattached(Err("container missing".into())));
    assert_eq!(container.runtime(), Some(&original.runtime));
    let other = tempfile::tempdir().unwrap();
    assert!(!container.reattached(Ok(
        resolve(&inspection(other.path()), "/workspace").unwrap()
    )));
    assert_eq!(container.runtime(), Some(&original.runtime));
    let mut replacement = original;
    replacement.runtime.container_id = "replacement-id".into();
    assert!(container.reattached(Ok(replacement.clone())));
    assert_eq!(container.runtime(), Some(&replacement.runtime));
}

#[test]
fn attached_menu_offers_reconnect_and_detach_without_container_lifecycle_actions() {
    let root = tempfile::tempdir().unwrap();
    let original = resolve(&inspection(root.path()), "/workspace").unwrap();
    let mut container = DevContainer::attached(original);
    let ctx = egui::Context::default();
    for failed in [false, true] {
        if failed {
            container.reattached(Err("missing".into()));
        }
        let output = crate::test_frame(&ctx, Default::default(), |root| {
            egui::CentralPanel::default().show(root, |ui| container.menu(ui, &ctx));
        });
        let labels: Vec<_> = output
            .shapes
            .iter()
            .filter_map(|shape| match &shape.shape {
                egui::Shape::Text(text) => Some(text.galley.job.text.as_str()),
                _ => None,
            })
            .collect();
        for label in ["Reconnect", "Detach", "Show logs"] {
            assert!(labels.contains(&label));
        }
        for label in ["Stop", "Rebuild", "Rebuild without cache"] {
            assert!(!labels.contains(&label));
        }
    }
    container.detach_attachment();
    assert!(container.runtime().is_none());
    assert!(container.tick());
    assert!(!container.tick());
    container.sync_workspace(Some(root.path()));
    assert!(matches!(container.state, State::Unavailable));
}

#[test]
#[ignore = "requires Docker daemon and a local debian:trixie image; creates an isolated test container"]
fn live_attachment_preserves_container_and_edits_bind_mounted_files() {
    struct Container(String);
    impl Drop for Container {
        fn drop(&mut self) {
            let _ = Command::new("docker").args(["rm", "-f", &self.0]).output();
        }
    }
    let root = tempfile::tempdir().unwrap();
    let output = Command::new("docker")
        .args(["run", "--pull=never", "-d", "--mount"])
        .arg(format!(
            "type=bind,source={},target=/workspace",
            root.path().display()
        ))
        .args(["debian:trixie", "sleep", "300"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let container = Container(String::from_utf8(output.stdout).unwrap().trim().to_owned());
    let attachment = Request {
        container: container.0.clone(),
        folder: "/workspace".into(),
    }
    .resolve()
    .unwrap();
    fs::write(
        attachment.runtime.host_workspace.join("sentinel"),
        "saved from host",
    )
    .unwrap();
    let output = attachment
        .runtime
        .command("cat")
        .arg("sentinel")
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(output.stdout, b"saved from host");
    let mut dev = DevContainer::attached(attachment.clone());
    dev.reconnect_attachment(egui::Context::default());
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !dev.tick() {
        assert!(std::time::Instant::now() < deadline);
        thread::sleep(std::time::Duration::from_millis(10));
    }
    dev.detach_attachment();
    drop(dev);
    let output = Command::new("docker")
        .args(["inspect", "--format", "{{.State.Running}}", &container.0])
        .output()
        .unwrap();
    assert_eq!(output.stdout, b"true\n");
    assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
}
