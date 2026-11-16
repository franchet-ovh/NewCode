use super::test_app;
use crate::editor::mobile::{MobilePanel, is_compact};
use eframe::egui;

fn frame(
    app: &mut super::EditorApp,
    ctx: &egui::Context,
    size: egui::Vec2,
    events: Vec<egui::Event>,
) -> egui::FullOutput {
    crate::test_frame(
        ctx,
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
            events,
            ..Default::default()
        },
        |root| app.render(root),
    )
}

#[test]
fn every_mobile_panel_fits_portrait_landscape_and_keyboard_height() {
    for size in [
        egui::vec2(360.0, 640.0),
        egui::vec2(390.0, 844.0),
        egui::vec2(844.0, 390.0),
        egui::vec2(390.0, 300.0),
        egui::vec2(899.0, 640.0),
    ] {
        let ctx = egui::Context::default();
        crate::ui_theme::apply(&ctx);
        let mut app = test_app();
        for panel in [
            MobilePanel::Editor,
            MobilePanel::Explorer,
            MobilePanel::Search,
            MobilePanel::Git,
            MobilePanel::Terminal,
            MobilePanel::Logs,
            MobilePanel::Manager,
        ] {
            app.select_mobile_panel(panel.clone(), &ctx);
            for _ in 0..3 {
                frame(&mut app, &ctx, size, vec![]);
                let used = ctx.globally_used_rect();
                assert!(
                    used.right() <= size.x + 1.0 && used.bottom() <= size.y + 1.0,
                    "{panel:?} overflows {size:?}: {used:?}"
                );
            }
        }
    }
    assert!(is_compact(899.0));
    assert!(!is_compact(900.0));
}

#[test]
fn switching_panels_and_resizing_preserves_unsaved_text_and_desktop_sizes() {
    let ctx = egui::Context::default();
    crate::ui_theme::apply(&ctx);
    let mut app = test_app();
    let buffer = app.buffer.clone();
    let sizes = (
        app.settings.explorer_width,
        app.settings.right_width,
        app.settings.top_height,
    );
    for panel in [
        MobilePanel::Explorer,
        MobilePanel::Search,
        MobilePanel::Terminal,
        MobilePanel::Logs,
        MobilePanel::Manager,
        MobilePanel::Editor,
    ] {
        app.select_mobile_panel(panel, &ctx);
        frame(&mut app, &ctx, egui::vec2(390.0, 844.0), vec![]);
    }
    assert_eq!(
        sizes,
        (
            app.settings.explorer_width,
            app.settings.right_width,
            app.settings.top_height
        )
    );
    let output = frame(&mut app, &ctx, egui::vec2(1440.0, 900.0), vec![]);
    assert!(
        text_position(&output, "println!").is_some(),
        "Desktop did not restore the document view"
    );
    frame(&mut app, &ctx, egui::vec2(390.0, 844.0), vec![]);
    assert_eq!(app.mobile.panel, Some(MobilePanel::Editor));
    assert_eq!(app.buffer, buffer);
    assert!(app.dirty);
}

fn click(app: &mut super::EditorApp, ctx: &egui::Context, pos: egui::Pos2) {
    for pressed in [true, false] {
        frame(
            app,
            ctx,
            egui::vec2(390.0, 844.0),
            vec![
                egui::Event::PointerMoved(pos),
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: Default::default(),
                },
            ],
        );
    }
}

#[test]
fn drawer_opens_selects_a_panel_and_dismisses_without_changing_documents() {
    let ctx = egui::Context::default();
    crate::ui_theme::apply(&ctx);
    let mut app = test_app();
    let buffer = app.buffer.clone();
    frame(&mut app, &ctx, egui::vec2(390.0, 844.0), vec![]);
    click(&mut app, &ctx, egui::pos2(40.0, 26.0));
    assert!(app.mobile.drawer_open);
    // Header and separator precede 44 px navigation rows, with 4 px spacing.
    frame(&mut app, &ctx, egui::vec2(390.0, 844.0), vec![]);
    click(&mut app, &ctx, egui::pos2(100.0, 144.0));
    assert_eq!(app.mobile.panel, Some(MobilePanel::Explorer));
    assert!(!app.mobile.drawer_open);
    click(&mut app, &ctx, egui::pos2(40.0, 26.0));
    frame(&mut app, &ctx, egui::vec2(390.0, 844.0), vec![]);
    click(&mut app, &ctx, egui::pos2(375.0, 400.0));
    assert!(!app.mobile.drawer_open);
    assert_eq!(app.buffer, buffer);
}

#[test]
fn missing_addon_returns_to_editor_and_hidden_terminal_loses_focus() {
    let ctx = egui::Context::default();
    crate::ui_theme::apply(&ctx);
    let mut app = test_app();
    app.mobile.panel = Some(MobilePanel::RightAddon("removed".into()));
    let id = crate::editor::terminal_input::terminal_id();
    let widget = id.with("test_widget");
    ctx.data_mut(|data| {
        data.insert_temp(id.with("widget"), widget);
        data.insert_temp(id.with("rect"), egui::Rect::EVERYTHING);
    });
    ctx.memory_mut(|memory| memory.request_focus(widget));
    frame(&mut app, &ctx, egui::vec2(390.0, 844.0), vec![]);
    assert_eq!(app.mobile.panel, Some(MobilePanel::Editor));
    assert!(!ctx.memory(|memory| memory.has_focus(widget)));
    assert!(
        ctx.data(|data| data.get_temp::<egui::Rect>(id.with("rect")))
            .is_none()
    );
}

#[test]
fn opening_files_and_search_results_navigates_to_editor_without_losing_drafts() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("new-file.txt");
    std::fs::write(&path, "first line\nsecond line\n").unwrap();
    let ctx = egui::Context::default();
    crate::ui_theme::apply(&ctx);
    let mut app = test_app();
    let original = app.active_file.clone().unwrap();
    let draft = app.buffer.clone();
    frame(&mut app, &ctx, egui::vec2(390.0, 844.0), vec![]);
    app.select_mobile_panel(MobilePanel::Explorer, &ctx);
    assert!(app.preview_file(path.clone()));
    assert_eq!(app.mobile.panel, Some(MobilePanel::Editor));
    assert_eq!(app.unsaved_files[&original], draft);
    app.select_mobile_panel(MobilePanel::Search, &ctx);
    app.open_search_result(path, 2);
    assert_eq!(app.mobile.panel, Some(MobilePanel::Editor));
    assert_eq!(app.pending_editor_cursor, Some(11));
}

#[test]
fn empty_workspace_and_drawer_fit_short_screens_and_escape_closes_drawer() {
    let ctx = egui::Context::default();
    crate::ui_theme::apply(&ctx);
    let mut app = test_app();
    app.active_file = None;
    app.open_files.clear();
    let size = egui::vec2(360.0, 300.0);
    frame(&mut app, &ctx, size, vec![]);
    assert_eq!(app.mobile.panel, Some(MobilePanel::Explorer));
    for panel in [MobilePanel::Editor, MobilePanel::Explorer, MobilePanel::Git] {
        app.select_mobile_panel(panel, &ctx);
        for _ in 0..3 {
            frame(&mut app, &ctx, size, vec![]);
        }
        assert!(ctx.globally_used_rect().right() <= size.x + 1.0);
        assert!(ctx.globally_used_rect().bottom() <= size.y + 1.0);
    }
    app.mobile.drawer_open = true;
    for _ in 0..3 {
        frame(&mut app, &ctx, size, vec![]);
    }
    assert!(ctx.globally_used_rect().bottom() <= size.y + 1.0);
    frame(
        &mut app,
        &ctx,
        size,
        vec![egui::Event::Key {
            key: egui::Key::Escape,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Default::default(),
        }],
    );
    assert!(!app.mobile.drawer_open);
}

#[test]
fn hidden_terminal_keeps_processing_output_without_being_restarted() {
    let ctx = egui::Context::default();
    crate::ui_theme::apply(&ctx);
    let mut app = test_app();
    let mut terminal =
        crate::terminal::Terminal::spawn("exec /bin/cat", None, ctx.clone()).unwrap();
    let output = terminal.capture_output(Box::new(std::io::sink()));
    let pid = terminal.process_id();
    app.top = Some(terminal);
    for (panel, text) in [
        (MobilePanel::Editor, "hidden output"),
        (MobilePanel::Logs, "more output"),
    ] {
        app.select_mobile_panel(panel, &ctx);
        output.send(text.as_bytes().to_vec()).unwrap();
        frame(&mut app, &ctx, egui::vec2(390.0, 844.0), vec![]);
        let terminal = app.top.as_mut().unwrap();
        assert_eq!(terminal.process_id(), pid);
        assert!(terminal.view.visible_text().unwrap().contains(text));
    }
}

fn text_position(output: &egui::FullOutput, text: &str) -> Option<egui::Pos2> {
    output.shapes.iter().find_map(|clipped| {
        if let egui::Shape::Text(shape) = &clipped.shape {
            let pos = shape.pos + shape.galley.size() * 0.5;
            if shape.galley.job.text.contains(text) && clipped.clip_rect.contains(pos) {
                return Some(pos);
            }
        }
        None
    })
}

#[test]
fn source_control_opens_a_fullscreen_diff_and_returns_to_changes() {
    let directory = tempfile::tempdir().unwrap();
    let workspace = directory.path();
    for args in [
        vec!["init", "-q"],
        vec!["config", "user.name", "Test"],
        vec!["config", "user.email", "test@example.invalid"],
    ] {
        assert!(
            std::process::Command::new("git")
                .current_dir(workspace)
                .args(args)
                .status()
                .unwrap()
                .success()
        );
    }
    std::fs::write(workspace.join("mobile-change.txt"), "new content\n").unwrap();
    let ctx = egui::Context::default();
    crate::ui_theme::apply(&ctx);
    let mut app = test_app();
    let draft = app.buffer.clone();
    app.settings.workspace = Some(workspace.to_path_buf());
    app.select_mobile_panel(MobilePanel::Git, &ctx);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let position = loop {
        let output = frame(&mut app, &ctx, egui::vec2(390.0, 844.0), vec![]);
        if let Some(position) = text_position(&output, "mobile-change.txt") {
            break position;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "Repository changes did not appear"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    };
    click(&mut app, &ctx, position);
    assert!(app.mobile.git_detail);
    let output = frame(&mut app, &ctx, egui::vec2(390.0, 844.0), vec![]);
    let back = text_position(&output, "Back to changes").unwrap();
    click(&mut app, &ctx, back);
    assert!(!app.mobile.git_detail);
    assert_eq!(app.mobile.panel, Some(MobilePanel::Git));
    assert_eq!(app.buffer, draft);
}

#[test]
fn touch_scrolling_the_short_drawer_does_not_activate_a_panel() {
    let ctx = egui::Context::default();
    crate::ui_theme::apply(&ctx);
    let mut app = test_app();
    let size = egui::vec2(844.0, 342.0);
    frame(&mut app, &ctx, size, vec![]);
    app.mobile.drawer_open = true;
    for _ in 0..3 {
        frame(&mut app, &ctx, size, vec![]);
    }
    for (phase, y) in [
        (egui::TouchPhase::Start, 260.0),
        (egui::TouchPhase::Move, 210.0),
        (egui::TouchPhase::Move, 150.0),
        (egui::TouchPhase::End, 150.0),
    ] {
        let pos = egui::pos2(120.0, y);
        let mut events = vec![
            egui::Event::Touch {
                device_id: egui::TouchDeviceId(0),
                id: egui::TouchId(1),
                phase,
                pos,
                force: None,
            },
            egui::Event::PointerMoved(pos),
        ];
        if matches!(phase, egui::TouchPhase::Start | egui::TouchPhase::End) {
            events.push(egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: phase == egui::TouchPhase::Start,
                modifiers: Default::default(),
            });
        }
        frame(&mut app, &ctx, size, events);
    }
    assert!(app.mobile.drawer_open);
    assert_eq!(app.mobile.panel, Some(MobilePanel::Editor));
}

#[test]
fn addon_rows_are_visible_and_selectable_in_the_phone_drawer() {
    let ctx = egui::Context::default();
    crate::ui_theme::apply(&ctx);
    let mut app = test_app();
    app.mobile.drawer_open = true;
    let entries = vec![
        (
            "left-tool".into(),
            "Left Tool".into(),
            focus_addon_sdk::Panel::Left,
        ),
        (
            "right-tool".into(),
            "Right Tool".into(),
            focus_addon_sdk::Panel::Right,
        ),
    ];
    let mut draw = |events| {
        crate::test_frame(
            &ctx,
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(390.0, 796.0),
                )),
                events,
                ..Default::default()
            },
            |_| app.mobile_drawer(&ctx, &entries),
        )
    };
    draw(vec![]);
    let output = draw(vec![]);
    assert!(text_position(&output, "Left Tool").is_some());
    let pos =
        text_position(&output, "Right Tool").expect("Addon row was clipped out of the drawer");
    for pressed in [true, false] {
        draw(vec![
            egui::Event::PointerMoved(pos),
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: Default::default(),
            },
        ]);
    }
    assert_eq!(
        app.mobile.panel,
        Some(MobilePanel::RightAddon("right-tool".into()))
    );
    assert!(!app.mobile.drawer_open);
}
