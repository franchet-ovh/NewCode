//! Shared, bounded status presentation for desktop and compact viewports.
use super::{EditorApp, SidebarMode, logs::Tab, mobile};
use crate::{logging::session, ui_theme as theme};
use eframe::egui;

#[derive(Clone, Copy)]
enum Action {
    Git,
    Logs,
}
struct Item {
    text: String,
    details: String,
    action: Option<Action>,
    max_width: f32,
}
impl Item {
    fn label(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            details: String::new(),
            action: None,
            max_width: 260.0,
        }
    }
}

pub(super) fn cursor_position(text: &str, cursor: usize) -> (usize, usize) {
    let mut line = 1;
    let mut column = 1;
    for ch in text.chars().take(cursor) {
        if ch == '\n' {
            line += 1;
            column = 1;
        } else {
            column += 1;
        }
    }
    (line, column)
}

pub(super) fn indentation(text: &str) -> String {
    let (mut tabs, mut spaces, mut width) = (false, false, 0);
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        let mut count = 0;
        for ch in line.chars().take_while(|ch| *ch == ' ' || *ch == '\t') {
            if ch == '\t' {
                tabs = true;
            } else {
                spaces = true;
                count += 1;
            }
        }
        if count > 0 {
            let (mut a, mut b) = (width, count);
            while b != 0 {
                (a, b) = (b, a % b);
            }
            width = a;
        }
    }
    match (tabs, spaces) {
        (true, true) => "Mixed indentation".into(),
        (true, false) => "Tabs".into(),
        (false, true) => format!("Spaces: {width}"),
        _ => "Indentation: unknown".into(),
    }
}

impl EditorApp {
    fn status_items(&self, ctx: &egui::Context) -> Vec<Item> {
        let (git, git_details) = self.git_ui.status_summary();
        let (errors, warnings) = session::diagnostic_counts();
        let mut items = vec![
            Item { text: git, details: git_details, action: Some(Action::Git), max_width: 240.0 },
            Item { text: format!("Logs: {errors} errors, {warnings} warnings"), details: "Retained application logs for this session, not compiler diagnostics. Click to view warnings and errors.".into(), action: Some(Action::Logs), max_width: 230.0 },
        ];
        if let Some(path) = &self.active_file {
            let cursor = self
                .pending_editor_cursor
                .or_else(|| {
                    egui::TextEdit::load_state(ctx, self.editor_id())
                        .and_then(|state| state.cursor.char_range())
                        .map(|range| range.primary.index.0)
                })
                .unwrap_or(0);
            let (line, column) = cursor_position(&self.buffer, cursor);
            items.push(Item::label(format!("Ln {line}, Col {column}")));
            let mut indent = Item::label(
                self.editor_layout_cache
                    .lock()
                    .unwrap()
                    .indentation(&self.buffer),
            );
            indent.details =
                "Detected from the current buffer; does not change Tab insertion.".into();
            items.push(indent);
            items.push(Item::label("UTF-8"));
            items.push(Item::label(
                crate::syntax::language(
                    path.extension().and_then(|ext| ext.to_str()).unwrap_or(""),
                )
                .0,
            ));
        }
        items.push(Item::label(if self.has_unsaved_changes() {
            format!("Unsaved changes · {}", self.status)
        } else {
            self.status.clone()
        }));
        items
    }

    fn status_action(&mut self, action: Action, ctx: &egui::Context, compact: bool) {
        match action {
            Action::Git => {
                if compact {
                    self.select_mobile_panel(mobile::MobilePanel::Git, ctx);
                } else {
                    self.sidebar_mode = SidebarMode::Git;
                }
            }
            Action::Logs => {
                self.logs.show_diagnostics();
                self.bottom_tab = Tab::Logs;
                if compact {
                    self.select_mobile_panel(mobile::MobilePanel::Logs, ctx);
                }
            }
        }
    }

    pub(super) fn status_bar(&mut self, root: &mut egui::Ui) {
        let ctx = root.ctx().clone();
        let compact = mobile::is_compact(ctx.viewport_rect().width());
        let colors = theme::colors(root);
        let items = self.status_items(&ctx);
        let mut action = None;
        egui::Panel::bottom("status_bar")
            .exact_size(theme::metrics(root).status_height)
            .frame(
                egui::Frame::new()
                    .fill(colors.accent)
                    .inner_margin(egui::Margin::symmetric(8, 2)),
            )
            .show(root, |ui| {
                ui.horizontal_centered(|ui| {
                    let spacing = ui.spacing().item_spacing.x;
                    // Reserve overflow access, then fit segments and truncate long messages.
                    let mut remaining = (ui.available_width() - 56.0 - spacing).max(0.0);
                    let mut widths = vec![None; items.len()];
                    let mut priority: Vec<usize> = (0..items.len()).collect();
                    let message = items.len() - 1;
                    priority.sort_by_key(|&i| {
                        if i == message {
                            1
                        } else if i == 2 && self.active_file.is_some() {
                            0
                        } else if i == 0 {
                            if compact { 4 } else { 2 }
                        } else if i == 1 {
                            if compact { 5 } else { 3 }
                        } else {
                            6 + i
                        }
                    });
                    for i in priority {
                        let width = ui
                            .painter()
                            .layout_no_wrap(
                                items[i].text.clone(),
                                egui::TextStyle::Small.resolve(ui.style()),
                                colors.on_accent,
                            )
                            .size()
                            .x
                            .min(items[i].max_width);
                        let width = if i == message && remaining >= 48.0 + spacing {
                            width.min(remaining - spacing)
                        } else {
                            width
                        };
                        if width + spacing <= remaining {
                            widths[i] = Some(width);
                            remaining -= width + spacing;
                        }
                    }
                    for (index, (item, width)) in items.iter().zip(widths).enumerate() {
                        if index == 2 {
                            ui.add_space(remaining);
                        }
                        let Some(width) = width else {
                            continue;
                        };
                        let mut label = egui::Label::new(
                            egui::RichText::new(&item.text)
                                .small()
                                .color(colors.on_accent),
                        )
                        .truncate();
                        if item.action.is_some() {
                            label = label.sense(egui::Sense::click());
                        }
                        let response = ui
                            .add_sized([width, ui.available_height()], label)
                            .on_hover_text(format!("{}\n{}", item.text, item.details));
                        if response.clicked() {
                            action = item.action;
                        }
                    }
                    if items.len() <= 2 {
                        ui.add_space(remaining);
                    }
                    ui.menu_button(
                        egui::RichText::new("More").small().color(colors.on_accent),
                        |ui| {
                            ui.set_max_width(
                                (ctx.viewport_rect().width() - 32.0).clamp(120.0, 420.0),
                            );
                            if compact {
                                ui.spacing_mut().interact_size.y = 44.0;
                            }
                            egui::ScrollArea::vertical()
                                .max_height((ctx.viewport_rect().height() - 80.0).max(44.0))
                                .show(ui, |ui| {
                                    for item in &items {
                                        if let Some(selected) = item.action {
                                            if ui
                                                .add(egui::Button::new(&item.text).wrap())
                                                .on_hover_text(&item.details)
                                                .clicked()
                                            {
                                                action = Some(selected);
                                                ui.close();
                                            }
                                        } else {
                                            ui.add(egui::Label::new(&item.text).wrap())
                                                .on_hover_text(&item.details);
                                        }
                                    }
                                    ui.separator();
                                    ui.label(format!(
                                        "{} open files · v{}",
                                        self.open_files.len(),
                                        env!("CARGO_PKG_VERSION")
                                    ));
                                });
                        },
                    );
                });
            });
        if let Some(action) = action {
            self.status_action(action, &ctx, compact);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::text::{CCursor, CCursorRange};

    #[test]
    fn cursor_is_one_based_and_uses_characters_not_bytes() {
        for (text, offset, expected) in [
            ("", 0, (1, 1)),
            ("é🦀\n\t文", 2, (1, 3)),
            ("é🦀\n\t文", 3, (2, 1)),
            ("é🦀\n\t文", 5, (2, 3)),
            ("a\r\nb", 3, (2, 1)),
            ("a\n", 99, (2, 1)),
        ] {
            assert_eq!(cursor_position(text, offset), expected);
        }
    }

    #[test]
    fn detects_observed_indentation_and_unknown_or_mixed_buffers() {
        for (text, expected) in [
            ("", "Indentation: unknown"),
            ("plain\n \n\t\n", "Indentation: unknown"),
            ("    a\n        b\n", "Spaces: 4"),
            ("  a\n    b\n      c", "Spaces: 2"),
            ("   a\n      b", "Spaces: 3"),
            ("\ta\n\t\tb", "Tabs"),
            ("\ta\n  b", "Mixed indentation"),
            (" \ta", "Mixed indentation"),
        ] {
            assert_eq!(indentation(text), expected);
        }
    }

    fn labels(app: &EditorApp, ctx: &egui::Context) -> Vec<String> {
        app.status_items(ctx)
            .into_iter()
            .map(|item| item.text)
            .collect()
    }

    #[test]
    fn metadata_tracks_selection_navigation_cached_edits_and_document_lifecycle() {
        let ctx = egui::Context::default();
        let mut app = super::super::tests::test_app();
        let root = tempfile::tempdir().unwrap();
        let rust = root.path().join("a.rs");
        let ts = root.path().join("b.ts");
        std::fs::write(&rust, "é\n    value").unwrap();
        std::fs::write(&ts, "\tvalue").unwrap();
        app.dirty = false;
        app.open_files.clear();
        app.active_file = None;
        app.open_file(rust.clone());
        let mut state = egui::TextEdit::load_state(&ctx, app.editor_id()).unwrap_or_default();
        let mut selection = CCursorRange::one(CCursor::new(0));
        selection.primary = CCursor::new(4);
        state.cursor.set_char_range(Some(selection));
        egui::TextEdit::store_state(&ctx, app.editor_id(), state);
        assert!(labels(&app, &ctx).contains(&"Ln 2, Col 3".into()));
        assert!(labels(&app, &ctx).contains(&"Rust".into()));
        app.pending_editor_cursor = Some(1);
        assert!(labels(&app, &ctx).contains(&"Ln 1, Col 2".into()));
        let _ = crate::test_frame(&ctx, Default::default(), |root| {
            egui::CentralPanel::default().show(root, |ui| app.text_view(ui, &ctx));
        });
        assert!(labels(&app, &ctx).contains(&"Spaces: 4".into()));
        app.buffer = "\tunsaved".into();
        app.mark_modified();
        assert!(labels(&app, &ctx).contains(&"Tabs".into()));
        app.open_file(ts.clone());
        assert!(labels(&app, &ctx).contains(&"TypeScript".into()));
        app.activate_file(rust.clone());
        assert_eq!(app.buffer, "\tunsaved");
        assert!(labels(&app, &ctx).contains(&"Tabs".into()));
        app.finish_close_file(ts);
        app.finish_close_file(rust);
        let labels = labels(&app, &ctx);
        assert!(
            !labels
                .iter()
                .any(|text| text.starts_with("Ln ") || text == "UTF-8" || text == "Rust")
        );
    }

    fn text_rect(output: &egui::FullOutput, label: &str) -> egui::Rect {
        fn visit(shape: &egui::Shape, label: &str) -> Option<egui::Rect> {
            match shape {
                egui::Shape::Text(text) if text.galley.job.text.starts_with(label) => {
                    Some(text.visual_bounding_rect())
                }
                egui::Shape::Vec(shapes) => shapes.iter().find_map(|shape| visit(shape, label)),
                _ => None,
            }
        }
        output
            .shapes
            .iter()
            .rev()
            .find_map(|shape| visit(&shape.shape, label))
            .unwrap_or_else(|| panic!("missing status text: {label}"))
    }

    #[test]
    fn status_fits_viewports_and_overflow_opens_logs_and_git() {
        for width in [320.0, 600.0, 900.0, 1440.0] {
            let ctx = egui::Context::default();
            theme::apply(&ctx);
            let mut app = super::super::tests::test_app();
            app.status = "Saved /very/long/path/".repeat(30);
            let mut frame = |events| {
                crate::test_frame(
                    &ctx,
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(width, 600.0),
                        )),
                        events,
                        ..Default::default()
                    },
                    |root| {
                        app.status_bar(root);
                        assert!(
                            ctx.globally_used_rect().right() <= width + 1.0,
                            "status overflows at {width}: {:?}",
                            ctx.globally_used_rect()
                        );
                    },
                )
            };
            let output = frame(vec![]);
            let more = text_rect(&output, "More").center();
            let click = |pos| {
                vec![
                    egui::Event::PointerMoved(pos),
                    egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed: true,
                        modifiers: egui::Modifiers::NONE,
                    },
                    egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed: false,
                        modifiers: egui::Modifiers::NONE,
                    },
                ]
            };
            frame(click(more));
            let output = frame(vec![]);
            let git = text_rect(&output, "No workspace").center();
            frame(click(git));
            assert!(
                app.sidebar_mode == SidebarMode::Git,
                "Git navigation failed at width {width}"
            );
            if width < mobile::BREAKPOINT {
                assert!(app.mobile.panel == Some(mobile::MobilePanel::Git));
            }
        }
        // Navigation restores a useful log filter even after the viewer was filtered elsewhere.
        let ctx = egui::Context::default();
        let mut app = super::super::tests::test_app();
        app.logs.source = Some(session::Source::Git);
        app.status_action(Action::Logs, &ctx, true);
        assert!(app.logs.source.is_none());
        assert!(app.bottom_tab == Tab::Logs);
        assert!(app.mobile.panel == Some(mobile::MobilePanel::Logs));
    }
}
