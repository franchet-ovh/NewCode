//! Settings UI, configuration resolution, and source-editor input integration.
use super::EditorApp;
use crate::indentation::{self, Action, Config, Overrides, Preferences};
use eframe::egui::{self, text::CCursorRange};
use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

#[derive(Default)]
pub(super) struct State {
    pub global: Preferences,
    pub open: bool,
    workspace_scope: bool,
    kind: String,
    project: Overrides,
    project_path: PathBuf,
    checked: Option<Instant>,
    pub project_error: Option<String>,
}

fn global_path() -> Result<PathBuf, String> {
    crate::app_paths::data_root()
        .map(|p| p.join("editor-settings.toml"))
        .map_err(|e| e.to_string())
}
pub(super) fn load_global(path: &Path) -> Preferences {
    let mut preferences: Preferences = match std::fs::read_to_string(path) {
        Ok(text) => toml::from_str(&text).unwrap_or_else(|e| {
            log::warn!("Invalid global editor settings: {e}");
            Preferences::default()
        }),
        Err(e) => {
            if e.kind() != std::io::ErrorKind::NotFound {
                log::warn!("Cannot read global editor settings: {e}");
            }
            Preferences::default()
        }
    };
    preferences.sanitize();
    preferences
}
impl State {
    pub fn load() -> Self {
        Self {
            global: global_path().map(|p| load_global(&p)).unwrap_or_default(),
            ..Default::default()
        }
    }
    fn refresh(&mut self, path: &Path, fallback_tab: usize) {
        if self.project_path == path
            && self
                .checked
                .is_some_and(|t| t.elapsed() < Duration::from_secs(1))
        {
            return;
        }
        self.project_path = path.to_owned();
        self.checked = Some(Instant::now());
        match editorconfig(path, fallback_tab) {
            Ok(config) => {
                self.project = config;
                self.project_error = None;
            }
            Err(error) => {
                if self.project_error.as_ref() != Some(&error) {
                    log::warn!("{error}");
                }
                self.project_error = Some(error);
                self.project = Overrides::default();
            }
        }
    }
}
pub(super) fn editorconfig(path: &Path, fallback_tab: usize) -> Result<Overrides, String> {
    let mut properties =
        ec4rs::properties_of(path).map_err(|e| format!("Cannot load EditorConfig: {e}"))?;
    properties.use_fallbacks();
    let value = |key: &str| properties.get_raw_for_key(key).to_string();
    let tab_size = value("tab_width").parse().ok();
    let indent_size = match value("indent_size").as_str() {
        "tab" => Some(tab_size.unwrap_or(fallback_tab)),
        other => other.parse().ok(),
    };
    let mut config = Overrides {
        tab_size,
        indent_size,
        insert_spaces: match value("indent_style").as_str() {
            "space" => Some(true),
            "tab" => Some(false),
            _ => None,
        },
        ..Default::default()
    };
    config.sanitize();
    Ok(config)
}

impl EditorApp {
    pub(super) fn indent_config(&mut self) -> Config {
        let Some(path) = &self.active_file else {
            return Config::default();
        };
        let kind = indentation::file_type(path);
        let base = indentation::resolve(
            kind,
            &self.indentation.global,
            &Overrides::default(),
            &Preferences::default(),
        );
        self.indentation.refresh(path, base.tab_size);
        indentation::resolve(
            kind,
            &self.indentation.global,
            &self.indentation.project,
            &self.settings.editor,
        )
    }
    pub(super) fn indent_details(&self) -> String {
        let kind = self
            .active_file
            .as_deref()
            .map(indentation::file_type)
            .unwrap_or("plain");
        let mut sources = std::collections::BTreeMap::new();
        self.indentation
            .global
            .defaults
            .record_sources("Global default", &mut sources);
        if let Some(values) = self.indentation.global.file_types.get(kind) {
            values.record_sources("Global file type", &mut sources);
        }
        self.indentation
            .project
            .record_sources("EditorConfig", &mut sources);
        self.settings
            .editor
            .defaults
            .record_sources("Workspace default", &mut sources);
        if let Some(values) = self.settings.editor.file_types.get(kind) {
            values.record_sources("Workspace file type", &mut sources);
        }
        let sources = sources
            .into_iter()
            .map(|(key, source)| format!("{key}: {source}"))
            .collect::<Vec<_>>()
            .join("\n");
        format!(
            "{sources}\nDetected: {}{}",
            self.editor_layout_cache
                .lock()
                .unwrap()
                .indentation(&self.buffer),
            self.indentation
                .project_error
                .as_ref()
                .map(|e| format!("\n{e}"))
                .unwrap_or_default()
        )
    }
    pub(super) fn apply_indent_action(
        &mut self,
        ctx: &egui::Context,
        action: Action,
        visual_column: Option<usize>,
    ) -> bool {
        let config = self.indent_config();
        let kind = self
            .active_file
            .as_deref()
            .map(indentation::file_type)
            .unwrap_or("plain");
        let id = self.editor_id();
        let mut state = egui::TextEdit::load_state(ctx, id).unwrap_or_default();
        let before_cursor = state.cursor.char_range().unwrap_or_default();
        let mut cursor = before_cursor;
        let before = self.buffer.clone();
        indentation::edit(
            &mut self.buffer,
            &mut cursor,
            config,
            kind,
            action,
            visual_column,
        );
        if before == self.buffer {
            return false;
        }
        let mut undo = state.undoer();
        undo.add_undo(&(before_cursor, before));
        undo.add_undo(&(cursor, self.buffer.clone()));
        state.set_undoer(undo);
        state.cursor.set_char_range(Some(cursor));
        state.store(ctx, id);
        self.mark_modified();
        ctx.request_repaint();
        true
    }
    pub(super) fn indentation_menu(&mut self, ui: &mut egui::Ui) {
        if ui
            .add(focus_addon_support::ui_theme::control_button(
                "Indentation Settings…",
            ))
            .clicked()
        {
            self.indentation.open = true;
            ui.close();
        }
        for (label, spaces) in [
            ("Convert Indentation to Spaces", true),
            ("Convert Indentation to Tabs", false),
        ] {
            if ui
                .add_enabled(
                    self.document_active(),
                    focus_addon_support::ui_theme::control_button(label),
                )
                .clicked()
            {
                self.apply_indent_action(ui.ctx(), Action::Convert { spaces }, None);
                ui.close();
            }
        }
    }
    pub(super) fn indentation_dialog(&mut self, ctx: &egui::Context) {
        if !self.indentation.open {
            return;
        }
        let mut open = true;
        let mut changed = false;
        let active_kind = self
            .active_file
            .as_deref()
            .map(indentation::file_type)
            .unwrap_or("plain");
        egui::Window::new("Indentation Settings")
            .id(egui::Id::new("indentation_settings"))
            .open(&mut open)
            .resizable(false)
            .default_width(420.0)
            .max_width((ctx.viewport_rect().width() - 48.0).max(240.0))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut self.indentation.workspace_scope, false, "Global");
                    ui.add_enabled_ui(self.settings.workspace.is_some(), |ui| {
                        ui.selectable_value(
                            &mut self.indentation.workspace_scope,
                            true,
                            "Workspace",
                        );
                    });
                });
                if self.settings.workspace.is_none() {
                    self.indentation.workspace_scope = false;
                }
                egui::ComboBox::from_label("File type")
                    .selected_text(if self.indentation.kind.is_empty() {
                        "All file types"
                    } else {
                        &self.indentation.kind
                    })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(
                            &mut self.indentation.kind,
                            String::new(),
                            "All file types",
                        );
                        for kind in [
                            "rust",
                            "python",
                            "javascript",
                            "typescript",
                            "json",
                            "toml",
                            "html",
                            "css",
                            "sh",
                            "sql",
                            "md",
                            "yaml",
                            "makefile",
                            "plain",
                        ] {
                            ui.selectable_value(&mut self.indentation.kind, kind.into(), kind);
                        }
                    });
                let kind = if self.indentation.kind.is_empty() {
                    active_kind
                } else {
                    &self.indentation.kind
                };
                let mut inherited = Config::default();
                if kind == "makefile" {
                    inherited.insert_spaces = false;
                }
                let workspace_scope = self.indentation.workspace_scope;
                if workspace_scope {
                    self.indentation.global.apply(kind, &mut inherited);
                    if kind == active_kind {
                        self.indentation.project.apply(&mut inherited);
                    }
                    if !self.indentation.kind.is_empty() {
                        self.settings.editor.defaults.apply(&mut inherited);
                    }
                } else if !self.indentation.kind.is_empty() {
                    self.indentation.global.defaults.apply(&mut inherited);
                }
                let prefs = if workspace_scope {
                    &mut self.settings.editor
                } else {
                    &mut self.indentation.global
                };
                let overrides = if self.indentation.kind.is_empty() {
                    &mut prefs.defaults
                } else {
                    prefs
                        .file_types
                        .entry(self.indentation.kind.clone())
                        .or_default()
                };
                ui.label("Unchecked overrides inherit the value shown.");
                if workspace_scope {
                    ui.label("Workspace overrides take priority over EditorConfig.");
                }
                egui::Grid::new("indent_settings_grid")
                    .num_columns(3)
                    .show(ui, |ui| {
                        changed |= width_control(
                            ui,
                            "Tab stop width",
                            &mut overrides.tab_size,
                            inherited.tab_size,
                        );
                        changed |= width_control(
                            ui,
                            "Indent width",
                            &mut overrides.indent_size,
                            inherited.indent_size,
                        );
                        changed |= bool_control(
                            ui,
                            "Insert spaces",
                            &mut overrides.insert_spaces,
                            inherited.insert_spaces,
                        );
                        changed |= bool_control(
                            ui,
                            "Auto indent",
                            &mut overrides.auto_indent,
                            inherited.auto_indent,
                        );
                        changed |= bool_control(
                            ui,
                            "Smart Backspace",
                            &mut overrides.smart_backspace,
                            inherited.smart_backspace,
                        );
                        changed |= bool_control(
                            ui,
                            "Smart tabs (tab mode)",
                            &mut overrides.smart_tabs,
                            inherited.smart_tabs,
                        );
                    });
                if ui
                    .add(focus_addon_support::ui_theme::control_button(
                        "Reset to inherited",
                    ))
                    .clicked()
                {
                    *overrides = Overrides::default();
                    changed = true;
                }
                ui.collapsing(format!("Active file details ({active_kind})"), |ui| {
                    ui.label(self.indent_details());
                });
                ui.separator();
                self.indentation_menu(ui);
            });
        self.indentation.open = open;
        if changed {
            self.indentation.checked = None;
            let result = if self.indentation.workspace_scope {
                if self.save_settings() {
                    Ok(())
                } else {
                    Err("Cannot save workspace indentation settings".into())
                }
            } else {
                global_path().and_then(|path| write_global(&path, &self.indentation.global))
            };
            self.status = result
                .err()
                .unwrap_or_else(|| "Indentation settings saved".into());
            ctx.request_repaint();
        }
    }
}
pub(super) fn write_global(path: &Path, preferences: &Preferences) -> Result<(), String> {
    use std::io::Write;
    let parent = path.parent().ok_or("Missing settings directory")?;
    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let text = toml::to_string_pretty(preferences).map_err(|e| e.to_string())?;
    let mut file = tempfile::NamedTempFile::new_in(parent).map_err(|e| e.to_string())?;
    file.write_all(text.as_bytes()).map_err(|e| e.to_string())?;
    file.as_file().sync_all().map_err(|e| e.to_string())?;
    file.persist(path).map_err(|e| e.to_string())?;
    Ok(())
}
fn width_control(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut Option<usize>,
    inherited: usize,
) -> bool {
    let mut enabled = value.is_some();
    let mut changed = ui.checkbox(&mut enabled, label).changed();
    let mut width = value.unwrap_or(inherited);
    changed |= ui
        .add_enabled(enabled, egui::DragValue::new(&mut width).range(1..=256))
        .changed();
    ui.label(format!("Inherited: {inherited}"));
    ui.end_row();
    *value = enabled.then_some(width);
    changed
}
fn bool_control(ui: &mut egui::Ui, label: &str, value: &mut Option<bool>, inherited: bool) -> bool {
    let mut enabled = value.is_some();
    let mut changed = ui.checkbox(&mut enabled, label).changed();
    let mut flag = value.unwrap_or(inherited);
    changed |= ui
        .add_enabled(enabled, egui::Checkbox::without_text(&mut flag))
        .changed();
    ui.label(format!("Inherited: {inherited}"));
    ui.end_row();
    *value = enabled.then_some(flag);
    changed
}

/// Preserve egui's ordered input processing by flushing ordinary events before
/// each indentation command. Preliminary widgets share the final widget's ID and
/// rectangle. Their paint shapes are discarded; only the final widget is painted.
pub(super) fn source_edit(
    app: &mut EditorApp,
    ui: &mut egui::Ui,
    width: f32,
    layouter: &mut dyn FnMut(&egui::Ui, &dyn egui::TextBuffer, f32) -> std::sync::Arc<egui::Galley>,
) -> egui::text_edit::TextEditOutput {
    let ctx = ui.ctx().clone();
    let id = app.editor_id();
    let focused = ctx.memory(|m| m.has_focus(id));
    if focused {
        // A programmatic focus request may not yet have installed TextEdit's
        // code-editor filter when egui processes this frame's raw Tab/arrows.
        ctx.memory_mut(|m| m.move_focus(egui::FocusDirection::None));
    }
    let events = if focused {
        ctx.input_mut(|i| std::mem::take(&mut i.events))
    } else {
        Vec::new()
    };
    let mut pending = Vec::new();
    let mut changed = false;
    for event in events {
        let action = match &event {
            egui::Event::Key {
                key,
                pressed: true,
                modifiers,
                ..
            } if !modifiers.ctrl && !modifiers.alt && !modifiers.command => match key {
                egui::Key::Tab => Some(Action::Tab {
                    backwards: modifiers.shift,
                }),
                egui::Key::Enter if !modifiers.shift => Some(Action::Enter),
                egui::Key::Backspace if !modifiers.shift => Some(Action::Backspace),
                _ => None,
            },
            _ => None,
        };
        if let Some(action) = action {
            if !pending.is_empty() {
                ctx.input_mut(|i| i.events = std::mem::take(&mut pending));
                let mut child =
                    ui.new_child(egui::UiBuilder::new().max_rect(ui.available_rect_before_wrap()));
                let layer = child.layer_id();
                let first_shape = ctx.graphics_mut(|g| g.entry(layer).next_idx());
                changed |= widget(app, &mut child, width, layouter).response.changed();
                ctx.graphics_mut(|g| {
                    let list = g.entry(layer);
                    let end = list.next_idx();
                    for index in first_shape.0..end.0 {
                        list.reset_shape(egui::layers::ShapeIdx(index));
                    }
                });
                ctx.input_mut(|i| i.events.clear());
            }
            let galley = layouter(ui, &app.buffer, width);
            let range = egui::TextEdit::load_state(&ctx, id)
                .and_then(|s| s.cursor.char_range())
                .unwrap_or(CCursorRange::one(galley.end()));
            let cursor = range.sorted_cursors()[0];
            let x = galley.pos_from_cursor(cursor).left();
            let space = ui
                .fonts_mut(|f| f.glyph_width(&egui::TextStyle::Monospace.resolve(ui.style()), ' '));
            let column = (x / space).round().max(0.0) as usize;
            changed |= app.apply_indent_action(&ctx, action, Some(column));
        } else {
            pending.push(event);
        }
    }
    if focused {
        ctx.input_mut(|i| i.events = pending);
    }
    let mut output = widget(app, ui, width, layouter);
    if changed {
        output.response.mark_changed();
    }
    output
}
fn widget(
    app: &mut EditorApp,
    ui: &mut egui::Ui,
    width: f32,
    layouter: &mut dyn FnMut(&egui::Ui, &dyn egui::TextBuffer, f32) -> std::sync::Arc<egui::Galley>,
) -> egui::text_edit::TextEditOutput {
    let id = app.editor_id();
    focus_addon_support::ui_theme::multiline(&mut app.buffer)
        .id(id)
        .font(egui::TextStyle::Monospace)
        .desired_width(width)
        .desired_rows(1)
        .code_editor()
        .frame(egui::Frame::NONE)
        .layouter(layouter)
        .show(ui)
}
