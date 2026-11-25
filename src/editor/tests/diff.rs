use super::test_app;
use crate::editor::{
    EditorApp,
    close_dialog::{CloseChoice, CloseTabs},
};
use eframe::egui;
use std::path::Path;

fn frame(app: &mut EditorApp, ctx: &egui::Context, events: Vec<egui::Event>) -> egui::FullOutput {
    crate::test_frame(
        ctx,
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1000.0, 700.0),
            )),
            events,
            ..Default::default()
        },
        |root| {
            egui::CentralPanel::default().show(root, |ui| app.document_content(ui, false, true));
        },
    )
}

fn click(app: &mut EditorApp, ctx: &egui::Context, pos: egui::Pos2) {
    for pressed in [true, false] {
        frame(
            app,
            ctx,
            vec![
                egui::Event::PointerMoved(pos),
                egui::Event::PointerButton {
                    pos,
                    pressed,
                    button: egui::PointerButton::Primary,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );
    }
}

// Open through the real Source Control UI, including its asynchronous discovery.
fn open_diff(app: &mut EditorApp, ctx: &egui::Context, root: &Path, name: &str) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let render = |app: &mut EditorApp, events| {
        crate::test_frame(
            ctx,
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(400.0, 700.0),
                )),
                events,
                ..Default::default()
            },
            |root_ui| {
                egui::CentralPanel::default().show(root_ui, |ui| {
                    app.git_ui.ui(ui, Some(root), None);
                });
            },
        )
    };
    let pos = loop {
        let output = render(app, vec![]);
        if let Some(pos) = output.shapes.iter().find_map(|shape| {
            if let egui::Shape::Text(text) = &shape.shape {
                if text.galley.job.text == name {
                    return Some(text.pos + text.galley.size() * 0.5);
                }
            }
            None
        }) {
            break pos;
        }
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(std::time::Duration::from_millis(5));
    };
    for pressed in [true, false] {
        render(
            app,
            vec![
                egui::Event::PointerMoved(pos),
                egui::Event::PointerButton {
                    pos,
                    pressed,
                    button: egui::PointerButton::Primary,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );
    }
    assert!(app.git_ui.diff_active());
}

fn repository() -> tempfile::TempDir {
    let directory = tempfile::tempdir().unwrap();
    assert!(
        std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(directory.path())
            .status()
            .unwrap()
            .success()
    );
    std::fs::write(directory.path().join("first.txt"), "first\n").unwrap();
    std::fs::write(directory.path().join("second.txt"), "second\n").unwrap();
    directory
}

#[test]
fn reusable_diff_tab_preserves_dirty_document_and_keyboard_close_targets_diff() {
    let repo = repository();
    let ctx = egui::Context::default();
    crate::ui_theme::apply(&ctx, crate::ui_theme::Appearance::Light);
    let mut app = test_app();
    let draft = app.buffer.clone();
    let file = app.active_file.clone().unwrap();
    open_diff(&mut app, &ctx, repo.path(), "first.txt");
    let output = frame(&mut app, &ctx, vec![]);
    let text: Vec<_> = output
        .shapes
        .iter()
        .filter_map(|s| match &s.shape {
            egui::Shape::Text(t) => Some(t.galley.job.text.as_str()),
            _ => None,
        })
        .collect();
    assert!(text.iter().any(|t| t.contains("first.txt (diff)")));
    assert!(
        !text
            .iter()
            .any(|t| t.contains("Reload diff") || t.contains("Close diff"))
    );
    assert!(!app.document_active());
    let pos = ctx
        .read_response(egui::Id::new(("file_tab", &file)))
        .unwrap()
        .rect
        .center();
    click(&mut app, &ctx, pos);
    assert!(app.document_active());
    assert!(app.git_ui.diff_tab().is_some());
    let pos = ctx
        .read_response(egui::Id::new("diff_tab"))
        .unwrap()
        .rect
        .center();
    click(&mut app, &ctx, pos);
    assert!(app.git_ui.diff_active());
    open_diff(&mut app, &ctx, repo.path(), "second.txt");
    frame(&mut app, &ctx, vec![]);
    assert_eq!(
        app.git_ui.diff_tab().unwrap().0,
        std::path::PathBuf::from("second.txt")
    );
    assert_eq!(app.buffer, draft);
    assert!(app.dirty);
    crate::test_frame(
        &ctx,
        egui::RawInput {
            events: vec![
                egui::Event::ModifiersChanged(egui::Modifiers::COMMAND),
                egui::Event::Key {
                    key: egui::Key::W,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::COMMAND,
                },
            ],
            ..Default::default()
        },
        |_| {
            app.editor_shortcuts(&ctx);
        },
    );
    assert!(app.git_ui.diff_tab().is_none());
    assert_eq!(app.active_file, Some(file));
    assert_eq!(app.buffer, draft);
    assert!(app.close_confirmation.is_none());
}

#[test]
fn diff_without_files_and_bulk_closing_respect_unsaved_confirmation() {
    let repo = repository();
    let ctx = egui::Context::default();
    crate::ui_theme::apply(&ctx, crate::ui_theme::Appearance::Light);
    let mut app = test_app();
    app.open_files.clear();
    app.active_file = None;
    open_diff(&mut app, &ctx, repo.path(), "first.txt");
    frame(&mut app, &ctx, vec![]);
    assert!(ctx.read_response(egui::Id::new("diff_tab")).is_some());
    app.close_tabs(CloseTabs::All);
    assert!(app.git_ui.diff_tab().is_none());

    let file = repo.path().join("first.txt");
    app.open_file(file.clone());
    app.buffer = "unsaved draft".into();
    app.mark_modified();
    open_diff(&mut app, &ctx, repo.path(), "second.txt");
    app.close_tabs(CloseTabs::OthersDiff);
    assert_eq!(app.close_confirmation, Some(file.clone()));
    app.resolve_close(CloseChoice::Cancel);
    assert!(app.git_ui.diff_active());
    assert!(app.open_files.contains(&file));
    assert_eq!(app.buffer, "unsaved draft");
    app.close_tabs(CloseTabs::OthersDiff);
    app.resolve_close(CloseChoice::Discard);
    assert!(app.open_files.is_empty());
    assert!(app.git_ui.diff_active());
    assert!(app.active_file.is_none());
    frame(&mut app, &ctx, vec![]);
    let close = ctx
        .read_response(egui::Id::new("diff_tab").with("close"))
        .unwrap()
        .rect
        .center();
    click(&mut app, &ctx, close);
    assert!(app.git_ui.diff_tab().is_none());
}
