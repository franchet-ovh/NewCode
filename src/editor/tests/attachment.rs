//! Exercise the actual editor with a Docker shim, isolated from parallel-test environments.
use super::super::*;
use std::{
    fs,
    path::Path,
    process::Command,
    time::{Duration, Instant},
};

#[test]
fn attachment_starts_in_container_and_survives_reconnect_failure_until_explicit_detach() {
    let marker = "FOCUS_ATTACH_TEST_ROOT";
    let Ok(root) = std::env::var(marker) else {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("bin")).unwrap();
        let docker = root.path().join("bin/docker");
        fs::write(
            &docker,
            include_str!("../../../tests/fixtures/attachment-docker.sh"),
        )
        .unwrap();
        fs::set_permissions(docker, fs::Permissions::from_mode(0o700)).unwrap();
        let output = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "editor::tests::attachment::attachment_starts_in_container_and_survives_reconnect_failure_until_explicit_detach", "--nocapture"])
            .env(marker, root.path())
            .env("XDG_DATA_HOME", root.path().join("data"))
            .env("XDG_CONFIG_HOME", root.path().join("config"))
            .env("PATH", root.path().join("bin"))
            .output().unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    };
    let root = PathBuf::from(root);
    let workspace = root.join("source/sub project");
    fs::create_dir_all(&workspace).unwrap();
    fs::write(
        workspace.join(".devcontainer.json"),
        "invalid, must not be read",
    )
    .unwrap();
    fs::write(workspace.join(".gitignore"), "keep-this\n").unwrap();
    let inspection = serde_json::json!({"Id":"initial-id", "State":{"Running":true,"Paused":false},
        "Mounts":[{"Type":"bind","Source":root.join("source"),"Destination":"/workspace"}]});
    fs::write(root.join("inspection.json"), inspection.to_string()).unwrap();
    let mut args = crate::logging::Arguments::parse(
        [
            "--attach-container",
            "my-container",
            "--container-workspace",
            "/workspace/sub project",
        ]
        .map(Into::into),
    )
    .unwrap();
    args.resolve_attachment().unwrap();
    let ctx = egui::Context::default();
    let mut app = EditorApp::new(&ctx, &args);
    for _ in 0..2 {
        let output = crate::test_frame(
            &ctx,
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1440.0, 700.0),
                )),
                ..Default::default()
            },
            |root| app.toolbar(root),
        );
        let labels: Vec<_> = output
            .shapes
            .iter()
            .filter_map(|shape| match &shape.shape {
                egui::Shape::Text(text) => Some(text.galley.job.text.as_str()),
                _ => None,
            })
            .collect();
        assert!(labels.contains(&"Addons"));
        assert!(!labels.contains(&"Dev Container"));
    }
    let original = app.dev_container.runtime().unwrap().clone();
    assert_eq!(app.workspace(), Some(workspace.canonicalize().unwrap()));
    assert_eq!(
        original.workspace_folder,
        Path::new("/workspace/sub project")
    );
    assert!(app.window_title().contains("Devcontainer"));
    assert!(
        app.top
            .as_ref()
            .unwrap()
            .matches_context(Some(&original.host_workspace), Some(&original))
    );
    wait(|| {
        fs::read_to_string(root.join("docker.log"))
            .unwrap()
            .contains("exec -it")
    });
    let file = workspace.join("file.txt");
    fs::write(&file, "before").unwrap();
    assert!(app.open_file(file.clone()));
    app.buffer = "saved through attached workspace".into();
    app.mark_modified();
    app.save_file();
    assert_eq!(
        fs::read_to_string(file).unwrap(),
        "saved through attached workspace"
    );

    // Drive real menu buttons so asynchronous reconnect and explicit detach are covered.
    click(&mut app, &ctx, "Reconnect");
    assert_eq!(app.dev_container.runtime(), Some(&original));
    wait(|| app.dev_container.tick());
    let mut replacement = inspection;
    replacement["Id"] = serde_json::json!("replacement-id");
    fs::write(root.join("inspection.json"), replacement.to_string()).unwrap();
    click(&mut app, &ctx, "Reconnect");
    wait(|| app.dev_container.tick());
    assert_eq!(
        app.dev_container.runtime().unwrap().container_id,
        "replacement-id"
    );
    fs::write(root.join("inspection.json"), "invalid inspection").unwrap();
    click(&mut app, &ctx, "Reconnect");
    wait(|| {
        app.dev_container.tick();
        labels(&mut app, &ctx)
            .iter()
            .any(|label| label.contains("failed"))
    });
    assert_eq!(
        app.dev_container.runtime().unwrap().container_id,
        "replacement-id"
    );
    click(&mut app, &ctx, "Detach");
    assert!(app.dev_container.runtime().is_none());
    assert!(app.dev_container.tick());
    assert!(!app.window_title().contains("Devcontainer"));

    // The existing workspace switch must clear an active attachment too.
    app.dev_container = crate::dev_container::DevContainer::attached(args.attachment.unwrap());
    let next = root.join("other-workspace");
    fs::create_dir(&next).unwrap();
    app.switch_workspace(next.clone(), &ctx);
    assert_eq!(app.workspace(), Some(next));
    assert!(app.dev_container.runtime().is_none());
    assert_eq!(
        fs::read_to_string(workspace.join(".gitignore")).unwrap(),
        "keep-this\n"
    );
    assert!(!workspace.join(".devcontainer").exists());
    let log = fs::read_to_string(root.join("docker.log")).unwrap();
    assert!(
        log.lines()
            .all(|line| line.starts_with("inspect ") || line.starts_with("exec "))
    );
}

fn wait(mut predicate: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !predicate() {
        assert!(Instant::now() < deadline, "Attachment test timed out");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn labels(app: &mut EditorApp, ctx: &egui::Context) -> Vec<String> {
    let output = crate::test_frame(ctx, Default::default(), |root| {
        egui::CentralPanel::default().show(root, |ui| app.dev_container.menu(ui, ctx));
    });
    output
        .shapes
        .iter()
        .filter_map(|shape| match &shape.shape {
            egui::Shape::Text(text) => Some(text.galley.job.text.clone()),
            _ => None,
        })
        .collect()
}

fn click(app: &mut EditorApp, ctx: &egui::Context, label: &str) {
    let output = crate::test_frame(ctx, Default::default(), |root| {
        egui::CentralPanel::default().show(root, |ui| app.dev_container.menu(ui, ctx));
    });
    let position = output
        .shapes
        .iter()
        .find_map(|shape| match &shape.shape {
            egui::Shape::Text(text) if text.galley.job.text == label => {
                Some(text.visual_bounding_rect().center())
            }
            _ => None,
        })
        .unwrap();
    crate::test_frame(
        ctx,
        egui::RawInput {
            events: vec![
                egui::Event::PointerMoved(position),
                egui::Event::PointerButton {
                    pos: position,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: Default::default(),
                },
                egui::Event::PointerButton {
                    pos: position,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: Default::default(),
                },
            ],
            ..Default::default()
        },
        |root| {
            egui::CentralPanel::default().show(root, |ui| app.dev_container.menu(ui, ctx));
        },
    );
}
