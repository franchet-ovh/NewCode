//! Failure logging happens once on state transitions, never during rendering.
use super::*;
use std::{cell::RefCell, sync::Once};
thread_local! {static RECORDS:RefCell<Option<Vec<(log::Level,String)>>> = const {RefCell::new(None)};}
struct Capture;
impl log::Log for Capture {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        metadata.target() == "focus_editor::dev_container"
    }
    fn log(&self, record: &log::Record<'_>) {
        if self.enabled(record.metadata()) {
            RECORDS.with(|records| {
                if let Some(records) = records.borrow_mut().as_mut() {
                    records.push((record.level(), record.args().to_string()));
                }
            });
        }
    }
    fn flush(&self) {}
}
fn capture() {
    static INIT: Once = Once::new();
    static LOGGER: Capture = Capture;
    INIT.call_once(|| {
        log::set_logger(&LOGGER).unwrap();
        log::set_max_level(log::LevelFilter::Trace);
    });
    RECORDS.with(|records| *records.borrow_mut() = Some(Vec::new()));
}
fn records() -> Vec<(log::Level, String)> {
    RECORDS.with(|records| records.borrow().as_ref().unwrap().clone())
}
fn deliver(container: &mut DevContainer, event: Event) {
    let (tx, rx) = mpsc::channel();
    container.events = Some(rx);
    tx.send(event).unwrap();
    assert!(container.tick());
    assert!(!container.tick());
}
#[test]
fn errors_are_logged_once_and_never_expand_the_menu() {
    capture();
    let configuration = fallback_configuration(&None);
    let mut container = DevContainer::default();
    deliver(
        &mut container,
        Event::Started(
            configuration.clone(),
            Err(format!(
                "Cannot connect to Docker: {}\nTOKEN=private-value",
                "diagnostic ".repeat(300)
            )),
            "rebuild",
        ),
    );
    let first = records();
    assert_eq!(first.len(), 2);
    assert_eq!(first[0].0, log::Level::Warn);
    assert!(first[0].1.contains("rebuild failed"));
    assert!(first[0].1.contains("Cannot connect to Docker"));
    assert!(
        first
            .iter()
            .all(|record| !record.1.contains("private-value") && !record.1.contains('\n'))
    );
    let ctx = egui::Context::default();
    for _ in 0..3 {
        let output = crate::test_frame(
            &ctx,
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(360.0, 300.0),
                )),
                ..Default::default()
            },
            |root| {
                egui::CentralPanel::default().show(root, |ui| container.menu(ui, &ctx));
                assert!(ctx.globally_used_rect().right() <= 361.0);
            },
        );
        let labels: Vec<_> = output
            .shapes
            .iter()
            .filter_map(|shape| match &shape.shape {
                egui::epaint::Shape::Text(text) => Some(text.galley.job.text.as_str()),
                _ => None,
            })
            .collect();
        assert!(
            labels
                .iter()
                .any(|text| text.contains("see application log"))
        );
        assert!(labels.iter().any(|text| text.contains("Retry")));
        assert!(
            !labels
                .iter()
                .any(|text| text.contains("Cannot connect") || text.contains("diagnostic"))
        );
    }
    assert_eq!(records().len(), 2);
    deliver(
        &mut container,
        Event::Stopped(
            configuration,
            Err("Compose service shutdown timed out".into()),
        ),
    );
    assert_eq!(records().len(), 3);
    assert!(
        records()[2]
            .1
            .contains("stop failed: Compose service shutdown timed out")
    );
}
#[test]
fn success_and_configuration_failures_use_their_expected_levels() {
    capture();
    let configuration = fallback_configuration(&None);
    let mut container = DevContainer::default();
    let runtime = Runtime {
        host_workspace: "/tmp".into(),
        container_id: "test".into(),
        workspace_folder: "/workspaces".into(),
        remote_user: None,
    };
    deliver(
        &mut container,
        Event::Started(configuration.clone(), Ok(runtime), "reconnect"),
    );
    assert!(container.runtime().is_some());
    deliver(&mut container, Event::Stopped(configuration, Ok(())));
    assert!(container.runtime().is_none());
    assert!(records().iter().all(|record| record.0 == log::Level::Info));
    let root =
        std::env::temp_dir().join(format!("focus-container-diagnostic-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join(".devcontainer.json"), "{}").unwrap();
    container.sync_workspace(Some(&root));
    let count = records().len();
    container.sync_workspace(Some(&root));
    assert_eq!(records().len(), count);
    let warnings: Vec<_> = records()
        .into_iter()
        .filter(|record| record.0 == log::Level::Warn)
        .collect();
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].1.contains("configuration failed"));
    assert!(warnings[0].1.contains("missing field"));
    std::fs::remove_dir_all(root).unwrap();
}

fn test_runtime(configuration: &Configuration) -> Runtime {
    Runtime {
        host_workspace: configuration.workspace.clone(),
        container_id: "test".into(),
        workspace_folder: configuration.workspace_folder.clone(),
        remote_user: configuration.remote_user.clone(),
    }
}

#[test]
fn rebuild_actions_are_available_before_connection_and_after_failure() {
    let configuration = fallback_configuration(&None);
    for (state, available) in [
        (State::Unavailable, false),
        (State::Available(configuration.clone()), true),
        (State::Failed(configuration.clone()), true),
        (
            State::Ready(configuration.clone(), test_runtime(&configuration)),
            true,
        ),
        (State::Starting(configuration.clone()), false),
    ] {
        let ctx = egui::Context::default();
        let mut container = DevContainer {
            state,
            ..Default::default()
        };
        let output = crate::test_frame(&ctx, Default::default(), |root| {
            egui::CentralPanel::default().show(root, |ui| container.menu(ui, &ctx));
        });
        for label in ["Rebuild", "Rebuild without cache"] {
            let found = output.shapes.iter().any(|shape| {
                matches!(
                    &shape.shape, egui::Shape::Text(text) if text.galley.job.text == label
                )
            });
            assert_eq!(found, available, "{label}");
        }
    }
}

#[test]
fn rebuild_buttons_dispatch_the_selected_operation() {
    for (label, operation) in [
        ("Rebuild", "rebuild"),
        ("Rebuild without cache", "rebuild without cache"),
    ] {
        let workspace = tempfile::tempdir().unwrap();
        // Invalid configuration prevents any Docker calls during this UI test.
        std::fs::write(workspace.path().join(".devcontainer.json"), "{}").unwrap();
        let configuration = fallback_configuration(&Some(workspace.path().into()));
        let mut container = DevContainer {
            state: State::Failed(configuration),
            ..Default::default()
        };
        let ctx = egui::Context::default();
        let output = crate::test_frame(&ctx, Default::default(), |root| {
            egui::CentralPanel::default().show(root, |ui| container.menu(ui, &ctx));
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
            &ctx,
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
                egui::CentralPanel::default().show(root, |ui| container.menu(ui, &ctx));
            },
        );
        assert!(matches!(container.state, State::Starting(_)));
        let event = container
            .events
            .as_ref()
            .unwrap()
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        assert!(matches!(event, Event::Started(_, Err(_), actual) if actual == operation));
    }
}

#[test]
fn start_modes_use_the_correct_commands_and_stop_on_failure() {
    for (mode, expected) in [
        (StartMode::Normal, vec![vec!["up", "-d", "--build", "dev"]]),
        (
            StartMode::Rebuild,
            vec![vec!["up", "-d", "--build", "--force-recreate", "dev"]],
        ),
        (
            StartMode::RebuildWithoutCache,
            vec![
                vec!["build", "--no-cache", "--with-dependencies", "dev"],
                vec!["up", "-d", "--no-build", "--force-recreate", "dev"],
            ],
        ),
    ] {
        for fail_at in [None, Some(0), Some(expected.len() - 1)] {
            let mut calls = Vec::new();
            let result = compose_start("dev", mode, |args| {
                calls.push(args.iter().map(|arg| arg.to_string()).collect::<Vec<_>>());
                if fail_at == Some(calls.len() - 1) {
                    Err("Compose failed".into())
                } else {
                    Ok(())
                }
            });
            assert_eq!(result.is_err(), fail_at.is_some());
            assert_eq!(
                calls,
                expected[..fail_at.map_or(expected.len(), |index| index + 1)]
            );
        }
    }
}

#[test]
fn retry_and_rebuild_reload_fixed_configuration_and_recover_from_failures() {
    let workspace = tempfile::tempdir().unwrap();
    let file = workspace.path().join(".devcontainer.json");
    std::fs::write(workspace.path().join("compose.yml"), "services: {}").unwrap();
    let mut previous = fallback_configuration(&Some(workspace.path().into()));
    for mode in [
        StartMode::Normal,
        StartMode::Rebuild,
        StartMode::RebuildWithoutCache,
    ] {
        std::fs::write(&file, "{}").unwrap();
        let (configuration, result) = start_reloaded(previous, mode, |_, _| {
            panic!("invalid config must not start")
        });
        assert!(result.err().unwrap().contains("missing field"));
        std::fs::write(&file, r#"{"name":"Fixed","dockerComposeFile":"compose.yml","service":"fixed","workspaceFolder":"/fixed","remoteUser":"user"}"#).unwrap();
        let (configuration, result) =
            start_reloaded(configuration, mode, |configuration, actual_mode| {
                assert_eq!(actual_mode, mode);
                assert_eq!(configuration.service, "fixed");
                assert_eq!(configuration.workspace_folder, Path::new("/fixed"));
                assert_eq!(configuration.remote_user.as_deref(), Some("user"));
                Err("simulated build/start failure".into())
            });
        let mut container = DevContainer::default();
        deliver(
            &mut container,
            Event::Started(configuration.clone(), result, "start"),
        );
        assert!(matches!(container.state, State::Failed(_)));
        let (configuration, result) = start_reloaded(configuration, mode, |configuration, _| {
            Ok(test_runtime(configuration))
        });
        deliver(
            &mut container,
            Event::Started(configuration.clone(), result, "rebuild"),
        );
        assert!(container.runtime().is_some());
        assert_eq!(container.label(), Some("Fixed"));
        previous = configuration;
    }
    std::fs::remove_file(file).unwrap();
    assert!(
        start_reloaded(previous, StartMode::Rebuild, |_, _| panic!(
            "missing config must not start"
        ))
        .1
        .is_err()
    );
}

#[test]
fn an_in_flight_operation_cannot_be_replaced_by_another_start() {
    let configuration = fallback_configuration(&None);
    let (tx, rx) = mpsc::channel();
    let mut container = DevContainer {
        state: State::Starting(configuration.clone()),
        events: Some(rx),
        ..Default::default()
    };
    container.start(
        configuration.clone(),
        StartMode::RebuildWithoutCache,
        egui::Context::default(),
    );
    tx.send(Event::Stopped(configuration, Ok(()))).unwrap();
    assert!(container.tick());
    assert!(matches!(container.state, State::Available(_)));
}
