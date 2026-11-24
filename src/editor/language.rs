//! Native language UI, shared by all language addons.
use super::EditorApp;
use crate::{lsp, ui_theme as theme};
use eframe::egui::{
    self,
    text::{CCursor, CCursorRange},
};

impl EditorApp {
    pub(super) fn apply_language_changes(&mut self, ctx: &egui::Context) {
        let changes = self
            .addons
            .language
            .as_mut()
            .map(|p| std::mem::take(&mut p.changes))
            .unwrap_or_default();
        for edit in changes {
            let active = self
                .active_file
                .as_ref()
                .map(|p| p.canonicalize().unwrap_or_else(|_| p.clone()));
            if active.as_ref() != Some(&edit.path) || self.buffer != &*edit.before {
                self.status = "Language edit discarded because the document changed".into();
                continue;
            }
            if self.buffer == edit.after {
                continue;
            }
            let id = self.editor_id();
            let mut state = egui::TextEdit::load_state(ctx, id).unwrap_or_default();
            let before_cursor = state.cursor.char_range().unwrap_or_default();
            let mut undo = state.undoer();
            undo.add_undo(&(before_cursor, self.buffer.clone()));
            self.buffer = edit.after;
            let cursor = edit.cursor.min(self.buffer.chars().count());
            let range = CCursorRange::one(CCursor::new(cursor));
            undo.add_undo(&(range, self.buffer.clone()));
            state.set_undoer(undo);
            state.cursor.set_char_range(Some(range));
            state.store(ctx, id);
            self.pending_editor_cursor = Some(cursor);
            self.mark_modified();
            ctx.request_repaint();
        }
    }

    pub(super) fn language_keys(&mut self, ctx: &egui::Context) {
        if !ctx.memory(|m| m.has_focus(self.editor_id())) {
            return;
        }
        let Some(provider) = &mut self.addons.language else {
            return;
        };
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::CTRL, egui::Key::Space)) {
            provider.complete();
        }
        if !provider.completions.is_empty() {
            if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
                provider.dismiss_completion();
            } else if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown))
            {
                provider.selected = (provider.selected + 1) % provider.completions.len();
                provider.reveal_completion = true;
            } else if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp)) {
                provider.selected = (provider.selected + provider.completions.len() - 1)
                    % provider.completions.len();
                provider.reveal_completion = true;
            } else if ctx.input_mut(|i| {
                i.consume_key(egui::Modifiers::NONE, egui::Key::Enter)
                    || i.consume_key(egui::Modifiers::NONE, egui::Key::Tab)
            }) {
                provider.accept_completion(provider.selected);
            }
        }
        self.addons.flush_language();
        self.apply_language_changes(ctx);
    }

    pub(super) fn language_context_menu(&mut self, response: &egui::Response) {
        response.context_menu(|ui| {
            if ui.button("Format Document").clicked() {
                let config = self.indent_config();
                if let Some(provider) = &mut self.addons.language {
                    provider.format(config.tab_size, config.insert_spaces);
                }
                self.addons.flush_language();
                ui.close();
            }
            if let Some(path) = self.active_file.clone() {
                let providers = self.addons.language_providers(&path);
                if providers.len() > 1 {
                    ui.separator();
                    ui.label("Language provider");
                    for (id, name) in providers {
                        if ui.button(name).clicked() {
                            if let Some(extension) = path.extension().and_then(|e| e.to_str()) {
                                self.settings.lsp.providers.insert(extension.into(), id);
                            }
                            self.navigation.client.restart = true;
                            ui.close();
                        }
                    }
                }
            }
        });
    }

    pub(super) fn completion_ui(&mut self, ui: &egui::Ui, edit: &egui::text_edit::TextEditOutput) {
        let popup_id = self.editor_id().with("language_completions");
        let Some(provider) = &mut self.addons.language else {
            return;
        };
        if provider.completions.is_empty() {
            return;
        }
        // Button clicks must not dismiss the list before the Area processes them.
        if !edit.response.has_focus() && !ui.input(|i| i.pointer.any_pressed()) {
            provider.dismiss_completion();
            return;
        }
        let cursor = edit.cursor_range.map_or(0, |r| r.primary.index.0);
        let rect = edit
            .galley
            .pos_from_cursor(CCursor::new(cursor))
            .translate(edit.galley_pos.to_vec2());
        let mut accept = None;
        egui::Area::new(popup_id)
            .order(egui::Order::Foreground)
            .fixed_pos(rect.left_bottom())
            .constrain(true)
            .show(ui.ctx(), |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_max_width(420.0);
                    let row_height = ui.text_style_height(&egui::TextStyle::Body) + 8.0;
                    let mut scroll = egui::ScrollArea::vertical().max_height(240.0);
                    if std::mem::take(&mut provider.reveal_completion) {
                        scroll = scroll.vertical_scroll_offset(
                            (provider.selected as f32 - 4.0).max(0.0) * row_height,
                        );
                    }
                    scroll.show_rows(ui, row_height, provider.completions.len(), |ui, rows| {
                        for index in rows {
                            if ui
                                .add(
                                    egui::Button::selectable(
                                        index == provider.selected,
                                        &provider.completions[index].label,
                                    )
                                    .truncate(),
                                )
                                .clicked()
                            {
                                accept = Some(index);
                            }
                        }
                    });
                });
            });
        if let Some(index) = accept {
            provider.accept_completion(index);
            ui.ctx().request_repaint();
        }
    }

    pub(super) fn paint_language_diagnostics(
        &self,
        ui: &egui::Ui,
        edit: &egui::text_edit::TextEditOutput,
        gutter: egui::Rect,
        clip: egui::Rect,
    ) {
        if edit.response.changed() {
            return;
        }
        let Some(provider) = self.addons.language.as_ref() else {
            return;
        };
        let diagnostics = provider.active_diagnostics();
        let colors = theme::colors(ui);
        for diagnostic in diagnostics {
            let line_y = edit.galley_pos.y
                + diagnostic.range.start.line as f32 * theme::metrics(ui).editor_line_height;
            if line_y + theme::metrics(ui).editor_line_height < clip.top() || line_y > clip.bottom()
            {
                continue;
            }
            let Some((start, end)) = diagnostic.offsets else {
                continue;
            };
            let from = edit
                .galley
                .pos_from_cursor(CCursor::new(start))
                .translate(edit.galley_pos.to_vec2());
            if from.bottom() < clip.top() || from.top() > clip.bottom() {
                continue;
            }
            let color = if diagnostic.severity == 1 {
                colors.error
            } else {
                colors.warning
            };
            ui.painter()
                .with_clip_rect(gutter.intersect(clip))
                .circle_filled(egui::pos2(gutter.left() + 5.0, from.center().y), 3.0, color);
            let to = edit
                .galley
                .pos_from_cursor(CCursor::new(end))
                .translate(edit.galley_pos.to_vec2());
            let right = if (to.top() - from.top()).abs() < 1.0 {
                to.left().max(from.left() + 4.0)
            } else {
                clip.right()
            };
            let line = egui::Rect::from_min_max(
                egui::pos2(from.left(), from.top()),
                egui::pos2(right, from.bottom() + 2.0),
            )
            .intersect(clip);
            if line.is_positive() {
                ui.painter().with_clip_rect(clip).line_segment(
                    [
                        egui::pos2(line.left(), line.bottom()),
                        egui::pos2(line.right(), line.bottom()),
                    ],
                    egui::Stroke::new(1.0, color),
                );
                ui.interact(
                    line,
                    self.editor_id()
                        .with(("diagnostic", start, end, &diagnostic.source)),
                    egui::Sense::hover(),
                )
                .on_hover_text(&diagnostic.message);
            }
        }
    }

    pub(super) fn problems_ui(&mut self, ui: &mut egui::Ui) {
        ui.add(
            egui::TextEdit::singleline(&mut self.navigation.problems_query)
                .hint_text("Filter problems"),
        );
        let query = self.navigation.problems_query.to_lowercase();
        let problems: Vec<&lsp::Diagnostic> = self
            .addons
            .language
            .as_ref()
            .into_iter()
            .flat_map(|p| p.diagnostics.values())
            .flatten()
            .filter(|d| {
                query.is_empty()
                    || d.message.to_lowercase().contains(&query)
                    || d.path.to_string_lossy().to_lowercase().contains(&query)
            })
            .collect();
        if problems.is_empty() {
            ui.label("No problems reported.");
        }
        let mut jump = None;
        let row_height = ui.text_style_height(&egui::TextStyle::Body) + 8.0;
        egui::ScrollArea::both().show_rows(ui, row_height, problems.len(), |ui, rows| {
            for index in rows {
                let d = &problems[index];
                let label = format!(
                    "{} {}:{}:{}  {}  [{}]",
                    match d.severity {
                        1 => "Error",
                        2 => "Warning",
                        3 => "Info",
                        _ => "Hint",
                    },
                    d.path
                        .strip_prefix(
                            self.settings
                                .workspace
                                .as_deref()
                                .unwrap_or(std::path::Path::new(""))
                        )
                        .unwrap_or(&d.path)
                        .display(),
                    d.range.start.line + 1,
                    d.range.start.character + 1,
                    d.message.replace('\n', " "),
                    d.source
                );
                let color = if d.severity == 1 {
                    theme::colors(ui).error
                } else {
                    theme::colors(ui).warning
                };
                if ui
                    .add(
                        egui::Button::new(egui::RichText::new(label).color(color))
                            .frame(false)
                            .truncate(),
                    )
                    .on_hover_text(&d.message)
                    .clicked()
                {
                    jump = Some((d.path.clone(), d.range.start));
                }
            }
        });
        if let Some((path, position)) = jump {
            if self.open_file(path) {
                self.pending_editor_cursor = lsp::protocol::offset(&self.buffer, position);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use focus_addon_sdk::{Context, language::Start};
    use serde_json::json;
    use std::{fs, path::Path, sync::Arc};
    fn fixture() -> (tempfile::TempDir, EditorApp, egui::Context) {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("a.py");
        fs::write(&path, "pr").unwrap();
        let mut app = super::super::tests::test_app();
        app.active_file = Some(path.clone());
        app.open_files = vec![path.clone()];
        app.buffer = "pr".into();
        app.dirty = false;
        let mut provider = lsp::Provider::new(
            "fixture".into(),
            1,
            Start {
                generation: 1,
                session: 1,
                language: "python".into(),
                context: Context::default(),
                command: None,
                args: vec![],
            },
        );
        provider.update(&path, 2, vec![(path.clone(), "pr".into(), false)]);
        app.addons.language = Some(provider);
        let ctx = egui::Context::default();
        theme::apply(&ctx);
        (root, app, ctx)
    }
    fn key(key: egui::Key) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }
    }
    fn frame(app: &mut EditorApp, ctx: &egui::Context, events: Vec<egui::Event>) {
        crate::test_frame(
            ctx,
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(700.0, 400.0),
                )),
                events,
                ..Default::default()
            },
            |root| {
                egui::CentralPanel::default().show(root, |ui| app.text_view(ui, ctx));
            },
        );
    }
    #[test]
    fn formatting_has_one_undo_step_and_rejects_stale_edits() {
        let (_root, mut app, ctx) = fixture();
        let path = app.active_file.clone().unwrap();
        frame(&mut app, &ctx, vec![]);
        app.addons
            .language
            .as_mut()
            .unwrap()
            .changes
            .push_back(lsp::Edit {
                path: path.clone(),
                before: Arc::from("pr"),
                after: "print()".into(),
                cursor: 7,
            });
        app.apply_language_changes(&ctx);
        assert_eq!(app.buffer, "print()");
        assert!(app.dirty);
        assert_eq!(fs::read_to_string(&path).unwrap(), "pr");
        let state = egui::TextEdit::load_state(&ctx, app.editor_id()).unwrap();
        let mut undo = state.undoer();
        assert_eq!(
            undo.undo(&(state.cursor.char_range().unwrap(), app.buffer.clone()))
                .unwrap()
                .1,
            "pr"
        );
        app.addons
            .language
            .as_mut()
            .unwrap()
            .changes
            .push_back(lsp::Edit {
                path,
                before: Arc::from("pr"),
                after: "obsolete".into(),
                cursor: 0,
            });
        app.apply_language_changes(&ctx);
        assert_eq!(app.buffer, "print()");
    }
    #[test]
    fn completion_keyboard_selection_acceptance_and_escape_preserve_text_input() {
        let (_root, mut app, ctx) = fixture();
        frame(&mut app, &ctx, vec![]);
        ctx.memory_mut(|m| m.request_focus(app.editor_id()));
        let mut state = egui::TextEdit::load_state(&ctx, app.editor_id()).unwrap();
        state
            .cursor
            .set_char_range(Some(CCursorRange::one(CCursor::new(2))));
        state.store(&ctx, app.editor_id());
        let provider = app.addons.language.as_mut().unwrap();
        for label in ["print", "property"] {
            provider.completions.push(lsp::Completion { label:label.into(),cursor:label.len(),edits:json!([{"range":{"start":{"line":0,"character":0},"end":{"line":0,"character":2}},"newText":label}]) });
        }
        frame(&mut app, &ctx, vec![key(egui::Key::ArrowDown)]);
        assert_eq!(app.addons.language.as_ref().unwrap().selected, 1);
        assert_eq!(app.buffer, "pr");
        frame(&mut app, &ctx, vec![key(egui::Key::Enter)]);
        assert_eq!(app.buffer, "property");
        assert!(!app.buffer.contains('\n'));
        assert!(app.addons.language.as_ref().unwrap().completions.is_empty());
        frame(&mut app, &ctx, vec![egui::Event::Text("x".into())]);
        assert_eq!(app.buffer, "propertyx");
        app.addons
            .language
            .as_mut()
            .unwrap()
            .completions
            .push(lsp::Completion {
                label: "ignored".into(),
                edits: json!([]),
                cursor: 0,
            });
        frame(&mut app, &ctx, vec![key(egui::Key::Escape)]);
        assert!(app.addons.language.as_ref().unwrap().completions.is_empty());
        assert_eq!(app.buffer, "propertyx");
    }
    #[test]
    fn problems_fit_narrow_panels_and_click_opens_the_location() {
        let (root, mut app, ctx) = fixture();
        let path = root.path().join("b.py");
        fs::write(&path, "a\nbad\n").unwrap();
        app.addons.language.as_mut().unwrap().diagnostics.insert(
            path.clone(),
            vec![lsp::Diagnostic {
                path: path.clone(),
                range: lsp::protocol::Range {
                    start: lsp::protocol::Position {
                        line: 1,
                        character: 1,
                    },
                    end: lsp::protocol::Position {
                        line: 1,
                        character: 3,
                    },
                },
                severity: 1,
                source: "mypy".into(),
                message: "A long type mismatch message ".repeat(30),
                offsets: None,
            }],
        );
        let mut position = None;
        for width in [320.0, 1000.0] {
            let output = crate::test_frame(
                &ctx,
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(width, 300.0),
                    )),
                    ..Default::default()
                },
                |root| {
                    egui::CentralPanel::default().show(root, |ui| app.problems_ui(ui));
                    assert!(ctx.globally_used_rect().right() <= width + 1.0);
                },
            );
            position = output.shapes.iter().find_map(|s| match &s.shape {
                egui::Shape::Text(t) if t.galley.job.text.starts_with("Error ") => {
                    Some(t.visual_bounding_rect().left_center() + egui::vec2(20.0, 0.0))
                }
                _ => None,
            });
        }
        let pos = position.expect("Problems must show an error row");
        for pressed in [true, false] {
            crate::test_frame(
                &ctx,
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1000.0, 300.0),
                    )),
                    events: vec![
                        egui::Event::PointerMoved(pos),
                        egui::Event::PointerButton {
                            pos,
                            button: egui::PointerButton::Primary,
                            pressed,
                            modifiers: egui::Modifiers::NONE,
                        },
                    ],
                    ..Default::default()
                },
                |root| {
                    egui::CentralPanel::default().show(root, |ui| app.problems_ui(ui));
                },
            );
        }
        assert_eq!(app.active_file.as_deref(), Some(Path::new(&path)));
        assert_eq!(app.pending_editor_cursor, Some(3));
    }
    #[test]
    fn legacy_commands_migrate_without_overwriting_explicit_addon_settings() {
        let (_root, mut app, ctx) = fixture();
        app.settings.lsp.rust.command = "/custom/rust".into();
        app.settings.lsp.python.command = "/legacy/python".into();
        app.settings.lsp.servers.insert(
            "lsp-python".into(),
            lsp::ServerConfig {
                command: "/explicit/python".into(),
                args: vec![],
            },
        );
        app.update_navigation(&ctx);
        assert_eq!(app.settings.lsp.servers["lsp-rust"].command, "/custom/rust");
        assert_eq!(
            app.settings.lsp.servers["lsp-python"].command,
            "/explicit/python"
        );
        assert!(app.addons.language.is_none());
    }
}
