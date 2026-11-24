//! File breadcrumbs and the active document's LSP symbol picker.
use super::EditorApp;
use crate::{lsp, ui_theme as theme};
use eframe::egui;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
};

#[derive(Default)]
pub(super) struct Navigation {
    pub(super) problems_query: String,
    pub client: lsp::Client,
    query: String,
    selected: usize,
    document: Option<PathBuf>,
    last_cursor: usize,
    cached_symbols: Arc<Vec<lsp::Symbol>>,
    cached_query: String,
    filtered: Vec<(usize, String)>,
    folders: FolderCache,
}
impl EditorApp {
    pub(super) fn update_navigation(&mut self, ctx: &egui::Context) {
        let cursor = self
            .pending_editor_cursor
            .or_else(|| {
                egui::TextEdit::load_state(ctx, self.editor_id())
                    .and_then(|s| s.cursor.char_range())
                    .map(|c| c.primary.index.0)
            })
            .unwrap_or(0);
        let Some(path) = self.active_file.clone() else {
            self.addons.stop_language();
            self.navigation.client.output = Default::default();
            return;
        };
        // Import legacy custom commands once; defaults now belong to the addons.
        for (id, old, default) in [
            ("lsp-rust", &self.settings.lsp.rust, "rust-analyzer"),
            ("lsp-python", &self.settings.lsp.python, "pylsp"),
        ] {
            if old.command != default || !old.args.is_empty() {
                self.settings
                    .lsp
                    .servers
                    .entry(id.into())
                    .or_insert_with(|| old.clone());
            }
        }
        let buffer = &self.buffer;
        let unsaved = &self.unsaved_files;
        let disk = &self.disk_changes;
        let dirty = self.dirty;
        let documents = self
            .open_files
            .iter()
            .filter_map(|p| {
                let text = if p == &path {
                    Some(buffer.as_str())
                } else {
                    unsaved
                        .get(p)
                        .map(String::as_str)
                        .or_else(|| disk.cached_text_ref(p))
                }?;
                Some((
                    p.clone(),
                    text,
                    p == &path && dirty || unsaved.contains_key(p),
                ))
            })
            .collect();
        self.navigation.client.output = self.addons.update_language(
            &path,
            &self.buffer,
            cursor,
            documents,
            &self.settings.lsp,
            std::mem::take(&mut self.navigation.client.wake),
            std::mem::take(&mut self.navigation.client.restart),
            ctx,
        );
        self.apply_language_changes(ctx);
    }
    pub(super) fn navigation_row(&mut self, ui: &mut egui::Ui) {
        let Some(path) = self.active_file.clone() else {
            return;
        };
        self.update_navigation(ui.ctx());
        let cursor = self
            .pending_editor_cursor
            .or_else(|| {
                egui::TextEdit::load_state(ui.ctx(), self.editor_id())
                    .and_then(|s| s.cursor.char_range())
                    .map(|c| c.primary.index.0)
            })
            .unwrap_or(0);
        let document_changed = self.navigation.document.as_ref() != Some(&path);
        let reveal = document_changed || self.navigation.last_cursor != cursor;
        if document_changed {
            self.navigation.query.clear();
            self.navigation.selected = 0;
            self.navigation.cached_symbols = Default::default();
            self.navigation.filtered.clear();
            self.navigation.folders = Default::default();
        }
        self.navigation.document = Some(path.clone());
        self.navigation.last_cursor = cursor;
        let output = self.navigation.client.output.clone();
        let colors = theme::colors(ui);
        let mut open = None;
        let mut jump = None;
        let height = if super::mobile::is_compact(ui.ctx().viewport_rect().width()) {
            44.0
        } else {
            28.0
        };
        let response = egui::Frame::new()
            .fill(colors.editor)
            .inner_margin(egui::Margin::symmetric(8, 0))
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                ui.allocate_ui_with_layout(
                    egui::vec2(ui.available_width(), height),
                    egui::Layout::left_to_right(egui::Align::Center),
                    |ui| {
                        egui::ScrollArea::horizontal()
                            .id_salt(self.editor_id().with("breadcrumbs"))
                            .auto_shrink([false, true])
                            .scroll_bar_visibility(
                                egui::scroll_area::ScrollBarVisibility::AlwaysHidden,
                            )
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    ui.spacing_mut().item_spacing.x = 4.0;
                                    let parts =
                                        path_segments(&path, self.settings.workspace.as_deref());
                                    for (index, (label, location)) in parts.iter().enumerate() {
                                        if index > 0 {
                                            ui.colored_label(colors.faint, "›");
                                        }
                                        if index + 1 == parts.len() {
                                            let response = ui
                                                .push_id(
                                                    (self.editor_id(), "symbol_picker"),
                                                    |ui| {
                                                        ui.menu_button(
                                                            egui::RichText::new(label)
                                                                .strong()
                                                                .color(colors.text),
                                                            |ui| {
                                                                if let Some(target) = symbol_picker(
                                                                    &mut self.navigation,
                                                                    ui,
                                                                    &output,
                                                                ) {
                                                                    jump = Some(target);
                                                                }
                                                            },
                                                        )
                                                    },
                                                )
                                                .inner
                                                .response
                                                .on_hover_text(location.display().to_string());
                                            if reveal {
                                                response.scroll_to_me(Some(egui::Align::RIGHT));
                                            }
                                        } else {
                                            ui.push_id(location, |ui| {
                                                ui.menu_button(
                                                    egui::RichText::new(label).color(colors.muted),
                                                    |ui| {
                                                        folder_menu(
                                                            ui,
                                                            location,
                                                            0,
                                                            &mut open,
                                                            &mut self.navigation.folders,
                                                        )
                                                    },
                                                )
                                            })
                                            .inner
                                            .response
                                            .on_hover_text(location.display().to_string());
                                        }
                                    }
                                    if output.ready {
                                        let mut chain: Vec<_> = output
                                            .symbols
                                            .iter()
                                            .filter(|s| {
                                                s.enclosing && s.start <= cursor && cursor < s.end
                                            })
                                            .take(64)
                                            .collect();
                                        chain.sort_by_key(|s| (s.start, std::cmp::Reverse(s.end)));
                                        for symbol in chain {
                                            ui.colored_label(colors.faint, "›");
                                            let response = ui
                                                .button(
                                                    egui::RichText::new(&symbol.name)
                                                        .color(colors.muted),
                                                )
                                                .on_hover_text(format!(
                                                    "Go to {} · line {}",
                                                    symbol.name, symbol.line
                                                ));
                                            if response.clicked() {
                                                jump = Some(symbol.target);
                                            }
                                            if reveal {
                                                response.scroll_to_me(Some(egui::Align::RIGHT));
                                            }
                                        }
                                    }
                                });
                            });
                    },
                );
            })
            .response;
        #[cfg(test)]
        ui.ctx().data_mut(|data| {
            data.insert_temp(self.editor_id().with("breadcrumb_rect"), response.rect)
        });
        ui.painter().hline(
            response.rect.x_range(),
            response.rect.bottom(),
            egui::Stroke::new(1.0, colors.border),
        );
        if let Some(path) = open {
            self.preview_file(path);
        }
        if let Some(target) = jump {
            self.pending_editor_cursor = Some(target);
            ui.ctx().memory_mut(|m| m.request_focus(self.editor_id()));
            self.navigation.client.activate();
        }
    }
}

fn kind(kind: u32) -> &'static str {
    match kind {
        2 => "module",
        5 => "class",
        6 => "method",
        10 => "enum",
        11 => "trait",
        12 => "fn",
        13 => "var",
        14 => "const",
        23 => "struct",
        _ => "symbol",
    }
}
fn path_segments(path: &Path, root: Option<&Path>) -> Vec<(String, PathBuf)> {
    let (base, relative) = root
        .and_then(|root| path.strip_prefix(root).ok().map(|p| (root, p)))
        .unwrap_or((Path::new(""), path));
    let mut result = Vec::new();
    let mut current = base.to_path_buf();
    if !base.as_os_str().is_empty() {
        result.push((
            base.file_name()
                .unwrap_or(base.as_os_str())
                .to_string_lossy()
                .into_owned(),
            base.into(),
        ));
    }
    for part in relative.components() {
        current.push(part.as_os_str());
        result.push((
            part.as_os_str().to_string_lossy().into_owned(),
            current.clone(),
        ));
    }
    result
}

#[derive(Default)]
struct FolderCache {
    root: Option<PathBuf>,
    frame: u64,
    entries: HashMap<PathBuf, Result<Arc<Vec<FolderEntry>>, String>>,
}
struct FolderEntry {
    name: String,
    path: PathBuf,
    directory: bool,
    symlink: bool,
}
fn folder_menu(
    ui: &mut egui::Ui,
    path: &Path,
    depth: usize,
    open: &mut Option<PathBuf>,
    cache: &mut FolderCache,
) {
    if depth >= 16 {
        ui.label("Folder nesting limit reached");
        return;
    }
    if depth == 0 {
        let frame = ui.ctx().cumulative_frame_nr();
        if cache.root.as_deref() != Some(path) || frame > cache.frame + 1 {
            cache.entries.clear();
            cache.root = Some(path.into());
        }
        cache.frame = frame;
    }
    if !cache.entries.contains_key(path) {
        if cache.entries.len() >= 16 {
            ui.label("Reopen menu to browse more folders");
            return;
        }
        let entries = std::fs::read_dir(path)
            .map(|entries| {
                let mut entries: Vec<_> = entries
                    .take(1001)
                    .filter_map(Result::ok)
                    .map(|entry| {
                        let kind = entry.file_type().ok();
                        FolderEntry {
                            name: entry.file_name().to_string_lossy().into_owned(),
                            path: entry.path(),
                            directory: kind.is_some_and(|t| {
                                t.is_dir() || (t.is_symlink() && entry.path().is_dir())
                            }),
                            symlink: kind.is_some_and(|t| t.is_symlink()),
                        }
                    })
                    .collect();
                entries.sort_by(|a, b| (!a.directory, &a.name).cmp(&(!b.directory, &b.name)));
                Arc::new(entries)
            })
            .map_err(|e| format!("Cannot read folder: {e}"));
        cache.entries.insert(path.into(), entries);
    }
    let entries = match cache.entries.get(path).unwrap().clone() {
        Ok(entries) => entries,
        Err(error) => {
            ui.label(error);
            return;
        }
    };
    if entries.is_empty() {
        ui.label("Empty folder");
    }
    egui::ScrollArea::vertical()
        .id_salt(path)
        .max_height(280.0)
        .show(ui, |ui| {
            for entry in entries.iter().take(1000) {
                if entry.directory && entry.symlink {
                    ui.add_enabled(
                        false,
                        egui::Button::new(format!("{} / (symlink)", entry.name)),
                    );
                } else if entry.directory {
                    ui.menu_button(format!("{} /", entry.name), |ui| {
                        folder_menu(ui, &entry.path, depth + 1, open, cache)
                    });
                } else if ui
                    .button(&entry.name)
                    .on_hover_text(entry.path.display().to_string())
                    .clicked()
                {
                    *open = Some(entry.path.clone());
                    ui.close_kind(egui::UiKind::Menu);
                }
            }
            if entries.len() > 1000 {
                ui.label("Showing first 1,000 entries");
            }
        });
}

fn symbol_picker(nav: &mut Navigation, ui: &mut egui::Ui, output: &lsp::Output) -> Option<usize> {
    ui.set_width((ui.ctx().viewport_rect().width() - 40.0).clamp(160.0, 420.0));
    let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
    let query = ui.add(
        egui::TextEdit::singleline(&mut nav.query)
            .char_limit(256)
            .hint_text("Search symbols…")
            .desired_width(ui.available_width()),
    );
    if query.changed() {
        nav.selected = 0;
    }
    let frame = ui.ctx().cumulative_frame_nr();
    let id = ui.id().with("opened");
    let fresh = ui.ctx().data_mut(|d| {
        let old = d.get_temp::<u64>(id);
        d.insert_temp(id, frame);
        old.is_none_or(|old| old + 1 < frame)
    });
    if fresh {
        query.request_focus();
        nav.client.activate();
    }
    if !output.ready {
        ui.label(&output.status);
        if nav.client.can_retry() && ui.button("Retry").clicked() {
            nav.client.retry();
        }
        return None;
    }
    if !Arc::ptr_eq(&nav.cached_symbols, &output.symbols) || nav.cached_query != nav.query {
        nav.cached_symbols = output.symbols.clone();
        nav.cached_query = nav.query.clone();
        nav.filtered.clear();
        let filter = nav.query.to_lowercase();
        for (index, symbol) in output.symbols.iter().enumerate() {
            let mut names = vec![symbol.name.as_str()];
            let mut parent = symbol.parent;
            while let Some(index) = parent {
                if names.len() >= 8 {
                    break;
                }
                names.push(&output.symbols[index].name);
                parent = output.symbols[index].parent;
            }
            names.reverse();
            let qualified: String = names.join(" › ").chars().take(1024).collect();
            if qualified.to_lowercase().contains(&filter) {
                nav.filtered.push((index, qualified));
            }
        }
    }
    if nav.filtered.is_empty() {
        ui.label(if output.symbols.is_empty() {
            "No symbols found"
        } else {
            "No matching symbols"
        });
        return None;
    }
    let down = ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown));
    let up = ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp));
    if down {
        nav.selected = (nav.selected + 1).min(nav.filtered.len() - 1);
    }
    if up {
        nav.selected = nav.selected.saturating_sub(1);
    }
    nav.selected = nav.selected.min(nav.filtered.len() - 1);
    if enter {
        ui.close();
        return Some(output.symbols[nav.filtered[nav.selected].0].target);
    }
    let mut target = None;
    let height = ui.spacing().interact_size.y;
    let mut scroll = egui::ScrollArea::vertical()
        .id_salt("symbol_results")
        .max_height(280.0);
    if up || down {
        scroll = scroll
            .vertical_scroll_offset(nav.selected as f32 * (height + ui.spacing().item_spacing.y));
    }
    scroll.show_rows(ui, height, nav.filtered.len(), |ui, rows| {
        for row in rows {
            let (index, qualified) = &nav.filtered[row];
            let symbol = &output.symbols[*index];
            let text = format!("{} {} · {}", kind(symbol.kind), qualified, symbol.line);
            if ui
                .add(egui::Button::selectable(row == nav.selected, text).truncate())
                .clicked()
            {
                target = Some(symbol.target);
                ui.close();
            }
        }
    });
    target
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn breadcrumb_targets_are_paths_not_display_strings() {
        let parts = path_segments(Path::new("/work/src/main.rs"), Some(Path::new("/work")));
        assert_eq!(
            parts.iter().map(|p| p.0.as_str()).collect::<Vec<_>>(),
            ["work", "src", "main.rs"]
        );
        assert_eq!(parts[1].1, Path::new("/work/src"));
        let parts = path_segments(Path::new("/elsewhere/a.py"), Some(Path::new("/work")));
        assert_eq!(parts.last().unwrap().1, Path::new("/elsewhere/a.py"));
    }
}

#[cfg(test)]
mod ui_tests {
    use super::*;
    fn input(width: f32, events: Vec<egui::Event>) -> egui::RawInput {
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(width, 500.0),
            )),
            events,
            ..Default::default()
        }
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
    fn symbols() -> lsp::Output {
        lsp::Output {
            ready: true,
            symbols: Arc::new(vec![
                lsp::Symbol {
                    name: "Outer".into(),
                    kind: 5,
                    start: 0,
                    end: 100,
                    target: 6,
                    line: 1,
                    parent: None,
                    enclosing: true,
                },
                lsp::Symbol {
                    name: "method".into(),
                    kind: 6,
                    start: 20,
                    end: 80,
                    target: 24,
                    line: 2,
                    parent: Some(0),
                    enclosing: true,
                },
            ]),
            ..Default::default()
        }
    }
    #[test]
    fn picker_filters_qualified_names_and_keyboard_selects_without_rebuilding_cache() {
        let ctx = egui::Context::default();
        theme::apply(&ctx);
        let mut nav = Navigation::default();
        let output = symbols();
        let mut target = None;
        crate::test_frame(&ctx, input(500.0, vec![]), |root| {
            egui::CentralPanel::default().show(root, |ui| {
                target = symbol_picker(&mut nav, ui, &output);
            });
        });
        assert!(target.is_none());
        assert_eq!(nav.filtered.len(), 2);
        let cache = nav.filtered.as_ptr();
        crate::test_frame(
            &ctx,
            input(500.0, vec![key(egui::Key::ArrowDown)]),
            |root| {
                egui::CentralPanel::default().show(root, |ui| {
                    target = symbol_picker(&mut nav, ui, &output);
                });
            },
        );
        assert_eq!(nav.filtered.as_ptr(), cache);
        assert_eq!(nav.selected, 1);
        crate::test_frame(&ctx, input(500.0, vec![key(egui::Key::Enter)]), |root| {
            egui::CentralPanel::default().show(root, |ui| {
                target = symbol_picker(&mut nav, ui, &output);
            });
        });
        assert_eq!(target, Some(24));
        nav.query = "outer › method".into();
        crate::test_frame(&ctx, input(500.0, vec![]), |root| {
            egui::CentralPanel::default().show(root, |ui| {
                symbol_picker(&mut nav, ui, &output);
            });
        });
        assert_eq!(nav.filtered.len(), 1);
        assert_eq!(nav.filtered[0].0, 1);
    }
    #[test]
    fn breadcrumb_row_is_bounded_on_compact_and_desktop_widths() {
        for width in [320.0, 900.0] {
            let ctx = egui::Context::default();
            theme::apply(&ctx);
            let mut app = super::super::tests::test_app();
            app.settings.lsp.enabled = false;
            app.active_file = Some(PathBuf::from(format!(
                "/work/{}/main.py",
                "long-folder/".repeat(12)
            )));
            for _ in 0..3 {
                crate::test_frame(&ctx, input(width, vec![]), |root| {
                    egui::CentralPanel::default()
                        .show(root, |ui| app.document_content(ui, false, true));
                });
            }
            let rect = ctx
                .data(|d| d.get_temp::<egui::Rect>(app.editor_id().with("breadcrumb_rect")))
                .unwrap();
            assert!(rect.right() <= width, "{rect:?}");
            assert!(
                rect.height() <= if width < 600.0 { 45.0 } else { 29.0 },
                "{rect:?}"
            );
            assert!(egui::TextEdit::load_state(&ctx, app.editor_id()).is_some());
        }
    }
}
