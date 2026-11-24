use super::{EditorApp, test_app};
use crate::indentation::{Action, Overrides, Preferences};
use eframe::egui::{
    self, Event, Key, Modifiers,
    text::{CCursor, CCursorRange},
};

struct Harness {
    app: EditorApp,
    ctx: egui::Context,
    time: f64,
}
impl Harness {
    fn new(text: &str) -> Self {
        let ctx = egui::Context::default();
        crate::ui_theme::apply(&ctx);
        let mut app = test_app();
        app.buffer = text.into();
        app.dirty = false;
        let mut this = Self {
            app,
            ctx,
            time: 0.0,
        };
        this.frame(vec![]);
        this.select(text.chars().count(), text.chars().count());
        this
    }
    fn select(&mut self, start: usize, end: usize) {
        let id = self.app.editor_id();
        let mut state = egui::TextEdit::load_state(&self.ctx, id).unwrap_or_default();
        state.cursor.set_char_range(Some(CCursorRange::two(
            CCursor::new(start),
            CCursor::new(end),
        )));
        state.store(&self.ctx, id);
        self.ctx.memory_mut(|m| m.request_focus(id));
    }
    fn frame(&mut self, events: Vec<Event>) -> egui::FullOutput {
        self.time += 0.1;
        crate::test_frame(
            &self.ctx,
            egui::RawInput {
                time: Some(self.time),
                events,
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(900.0, 600.0),
                )),
                ..Default::default()
            },
            |root| {
                egui::CentralPanel::default().show(root, |ui| self.app.text_view(ui, &self.ctx));
            },
        )
    }
    fn cursor(&self) -> CCursorRange {
        egui::TextEdit::load_state(&self.ctx, self.app.editor_id())
            .unwrap()
            .cursor
            .char_range()
            .unwrap()
    }
}
fn key(key: Key, modifiers: Modifiers) -> Event {
    Event::Key {
        key,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers,
    }
}
fn undo() -> Event {
    key(Key::Z, Modifiers::COMMAND)
}

#[test]
fn indentation_input_preserves_order_and_each_command_is_undoable() {
    let mut h = Harness::new("");
    h.frame(vec![
        Event::Text("ab".into()),
        key(Key::Tab, Modifiers::NONE),
        Event::Text("c".into()),
        key(Key::Tab, Modifiers::NONE),
    ]);
    assert_eq!(h.app.buffer, "ab  c   ");
    assert_eq!(h.cursor().primary.index.0, 8);
    assert!(h.app.dirty);
    h.frame(vec![undo()]);
    assert_eq!(h.app.buffer, "ab  c");
    h.frame(vec![key(Key::Z, Modifiers::COMMAND | Modifiers::SHIFT)]);
    assert_eq!(h.app.buffer, "ab  c   ");
    h.frame(vec![
        key(Key::ArrowLeft, Modifiers::NONE),
        key(Key::Backspace, Modifiers::NONE),
        Event::Text("!".into()),
    ]);
    assert_eq!(h.app.buffer, "ab  c ! ");
}

#[test]
fn block_indent_and_conversion_preserve_selection_and_undo() {
    let mut h = Harness::new("a\n  b\nc");
    h.select(6, 0);
    h.frame(vec![key(Key::Tab, Modifiers::NONE)]);
    assert_eq!(h.app.buffer, "    a\n      b\nc");
    assert!(h.cursor().primary.index < h.cursor().secondary.index);
    h.frame(vec![undo()]);
    assert_eq!(h.app.buffer, "a\n  b\nc");
    h.frame(vec![key(Key::Z, Modifiers::COMMAND | Modifiers::SHIFT)]);
    h.frame(vec![key(Key::Tab, Modifiers::SHIFT)]);
    assert_eq!(h.app.buffer, "a\n  b\nc");

    h.app.buffer = "\tvalue\t=1\r\n".into();
    h.select(1, 1);
    h.app
        .apply_indent_action(&h.ctx, Action::Convert { spaces: true }, None);
    assert_eq!(h.app.buffer, "    value\t=1\r\n");
    assert_eq!(h.cursor().primary.index.0, 4);
    h.frame(vec![undo()]);
    assert_eq!(h.app.buffer, "\tvalue\t=1\r\n");
    assert_eq!(h.cursor().primary.index.0, 1);
}

#[test]
fn pasted_tabs_are_preserved_and_workspace_width_can_change_live() {
    let mut h = Harness::new("");
    h.app.settings.editor.file_types.insert(
        "rust".into(),
        Overrides {
            tab_size: Some(6),
            indent_size: Some(6),
            ..Default::default()
        },
    );
    h.frame(vec![
        Event::Paste("a\tb".into()),
        key(Key::Tab, Modifiers::NONE),
    ]);
    assert_eq!(h.app.buffer, "a\tb     ");
    h.app
        .settings
        .editor
        .file_types
        .get_mut("rust")
        .unwrap()
        .insert_spaces = Some(false);
    h.frame(vec![key(Key::Tab, Modifiers::NONE)]);
    assert_eq!(h.app.buffer, "a\tb     \t");
}

#[test]
fn editorconfig_nested_patterns_fallbacks_and_workspace_precedence() {
    let root = tempfile::tempdir().unwrap();
    let nested = root.path().join("nested");
    std::fs::create_dir(&nested).unwrap();
    std::fs::write(
        root.path().join(".editorconfig"),
        "root=true\n[*]\nindent_style=space\nindent_size=6\n[*.{rs,py}]\ntab_width=8\n",
    )
    .unwrap();
    std::fs::write(
        nested.join(".editorconfig"),
        "[*.rs]\nindent_style=tab\nindent_size=tab\n[*.py]\nindent_size=unset\ntab_width=unset\n",
    )
    .unwrap();
    let config = super::super::indentation::editorconfig(&nested.join("a.rs"), 4).unwrap();
    assert_eq!(
        (config.tab_size, config.indent_size, config.insert_spaces),
        (Some(8), Some(8), Some(false))
    );
    let unset = super::super::indentation::editorconfig(&nested.join("a.py"), 4).unwrap();
    assert!(unset.indent_size.is_none() && unset.tab_size.is_none());
    let plain = super::super::indentation::editorconfig(&root.path().join("a.txt"), 4).unwrap();
    assert_eq!(plain.tab_size, Some(6));
    let path = nested.join("a.rs");
    std::fs::write(&path, "\tx").unwrap();
    let mut app = test_app();
    app.open_file(path);
    app.settings.editor.defaults.tab_size = Some(6);
    assert_eq!(app.indent_config().tab_size, 6);
    assert!(!app.indent_config().insert_spaces);
    assert_eq!(app.buffer, "\tx");
    std::fs::write(nested.join(".editorconfig"), "[*.rs]\nindent_style=space\n").unwrap();
    // Force a document activation, which must refresh project rules immediately.
    let other = root.path().join("other.txt");
    std::fs::write(&other, "other").unwrap();
    let original = app.active_file.clone().unwrap();
    app.open_file(other);
    app.indent_config();
    app.activate_file(original);
    assert!(app.indent_config().insert_spaces);
}

#[test]
fn global_storage_round_trips_sparse_values_without_session_state() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("editor-settings.toml");
    let prefs: Preferences =
        toml::from_str("[file_types.python]\ninsert_spaces=false\ntab_size=6").unwrap();
    super::super::indentation::write_global(&path, &prefs).unwrap();
    let loaded = super::super::indentation::load_global(&path);
    assert_eq!(loaded.file_types["python"].insert_spaces, Some(false));
    assert_eq!(loaded.file_types["python"].tab_size, Some(6));
    assert!(loaded.defaults.tab_size.is_none());
    assert!(loaded.file_types["python"].indent_size.is_none());
    assert!(
        !std::fs::read_to_string(path)
            .unwrap()
            .contains("open_files")
    );
}

#[test]
fn enter_and_smart_backspace_follow_separate_tab_and_indent_widths() {
    let mut h = Harness::new("fn f() {}");
    h.app.settings.editor.defaults.tab_size = Some(6);
    h.app.settings.editor.defaults.indent_size = Some(2);
    h.select(8, 8);
    h.frame(vec![key(Key::Enter, Modifiers::NONE)]);
    assert_eq!(h.app.buffer, "fn f() {\n  \n}");
    assert_eq!(h.cursor().primary.index.0, 11);
    h.frame(vec![key(Key::Backspace, Modifiers::NONE)]);
    assert_eq!(h.app.buffer, "fn f() {\n\n}");
}

#[test]
fn settings_dialog_renders_with_and_without_workspace() {
    for width in [390.0, 900.0] {
        let mut app = test_app();
        let ctx = egui::Context::default();
        crate::ui_theme::apply(&ctx);
        app.indentation.open = true;
        for workspace in [None, Some(std::path::PathBuf::from("/tmp"))] {
            app.settings.workspace = workspace;
            let output = crate::test_frame(
                &ctx,
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(width, 700.0),
                    )),
                    ..Default::default()
                },
                |_ui| app.indentation_dialog(&ctx),
            );
            assert!(app.indentation.open);
            assert!(!output.shapes.is_empty());
            let rect = ctx
                .memory(|m| m.area_rect(egui::Id::new("indentation_settings")))
                .unwrap();
            assert!(rect.width() <= width, "dialog overflows {width}: {rect:?}");
        }
    }
}
