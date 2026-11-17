//! A single content pane and a touch-sized drawer for narrow viewports.
use super::{EditorApp, SidebarMode, logs::Tab, terminal_input};
use crate::ui_theme as theme;
use eframe::egui;

pub(super) const BREAKPOINT: f32 = 900.0;

pub(super) fn is_compact(width: f32) -> bool {
    width < BREAKPOINT
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum MobilePanel {
    Editor,
    Explorer,
    Search,
    Git,
    Terminal,
    Logs,
    Manager,
    LeftAddon(String),
    RightAddon(String),
}

#[derive(Default)]
pub(super) struct MobileState {
    pub panel: Option<MobilePanel>,
    pub drawer_open: bool,
    pub was_compact: bool,
    pub git_detail: bool,
}

impl EditorApp {
    fn default_mobile_panel(&self) -> MobilePanel {
        if self.active_file.is_some() {
            MobilePanel::Editor
        } else {
            MobilePanel::Explorer
        }
    }

    pub(super) fn mobile_document_opened(&mut self) {
        if self.mobile.was_compact {
            self.mobile.panel = Some(MobilePanel::Editor);
        }
    }

    pub(super) fn select_mobile_panel(&mut self, panel: MobilePanel, ctx: &egui::Context) {
        if let Some(id) = ctx.memory(|memory| memory.focused()) {
            ctx.memory_mut(|memory| memory.surrender_focus(id));
        }
        match &panel {
            MobilePanel::Explorer => self.sidebar_mode = SidebarMode::Explorer,
            MobilePanel::Search => self.sidebar_mode = SidebarMode::Search,
            MobilePanel::Git => self.sidebar_mode = SidebarMode::Git,
            MobilePanel::Manager => {
                self.sidebar_mode = SidebarMode::AddonManager;
                let had_detail = self.addons.manager_has_detail();
                self.addons.open_manager();
                if !had_detail {
                    self.addons.dismiss_manager_detail();
                }
                self.addons.take_manager_request();
            }
            MobilePanel::LeftAddon(id) => {
                self.sidebar_mode = SidebarMode::Addon;
                if self.addons.active.as_ref() != Some(id) {
                    self.addons.activate(id.clone(), ctx);
                }
            }
            MobilePanel::RightAddon(id) => {
                self.settings.selected_right_addon = Some(id.clone());
            }
            MobilePanel::Terminal => self.bottom_tab = Tab::Terminal,
            MobilePanel::Logs => self.bottom_tab = Tab::Logs,
            MobilePanel::Editor => {
                // Desktop central content follows the sidebar selection. Returning
                // from the phone's Editor must keep the document in the center.
                self.sidebar_mode = SidebarMode::Explorer;
            }
        }
        self.mobile.panel = Some(panel);
        self.mobile.drawer_open = false;
        self.prepare_panel_input(ctx, true);
    }

    /// Clear geometry as well as focus: input routing happens before the next layout.
    pub(super) fn prepare_panel_input(&mut self, ctx: &egui::Context, compact: bool) {
        let changed_layout = compact != self.mobile.was_compact;
        let terminal_visible = !changed_layout
            && (!compact
                || (!self.mobile.drawer_open && self.mobile.panel == Some(MobilePanel::Terminal)));
        if !terminal_visible {
            let id = terminal_input::terminal_id();
            if let Some(widget) = ctx.data(|data| data.get_temp::<egui::Id>(id.with("widget"))) {
                ctx.memory_mut(|memory| memory.surrender_focus(widget));
            }
            ctx.data_mut(|data| data.remove::<egui::Rect>(id.with("rect")));
        }
        if changed_layout || (compact && self.mobile.drawer_open) {
            self.addons.hide_surfaces_except(None);
        } else if compact {
            let selected = match &self.mobile.panel {
                Some(MobilePanel::RightAddon(id)) => Some(id.as_str()),
                _ => None,
            };
            self.addons.hide_surfaces_except(selected);
        }
    }

    pub(super) fn mobile_layout(&mut self, root: &mut egui::Ui, opened_find: bool) {
        let ctx = root.ctx().clone();
        if self.mobile.panel.is_none() {
            self.mobile.panel = Some(self.default_mobile_panel());
        }
        if !self.mobile.was_compact {
            self.select_mobile_panel(self.mobile.panel.clone().unwrap(), &ctx);
        }
        self.mobile.was_compact = true;
        let addons = self.addons.panel_entries();
        if let Some(MobilePanel::LeftAddon(id) | MobilePanel::RightAddon(id)) = &self.mobile.panel {
            if !addons.iter().any(|(available, _, _)| available == id) {
                self.select_mobile_panel(self.default_mobile_panel(), &ctx);
            }
        }
        let panel = self.mobile.panel.clone().unwrap();
        let title = match &panel {
            MobilePanel::Editor => "Editor",
            MobilePanel::Explorer => "Explorer",
            MobilePanel::Search => "Search",
            MobilePanel::Git => "Source Control",
            MobilePanel::Terminal => "Terminal",
            MobilePanel::Logs => "Logs",
            MobilePanel::Manager => "Addon Manager",
            MobilePanel::LeftAddon(id) | MobilePanel::RightAddon(id) => addons
                .iter()
                .find(|(available, _, _)| available == id)
                .map(|(_, name, _)| name.as_str())
                .unwrap_or("Addon"),
        };
        egui::Panel::top("mobile_header")
            .exact_size(52.0)
            .frame(
                egui::Frame::new()
                    .fill(theme::colors(root).chrome)
                    .inner_margin(4),
            )
            .show(root, |ui| {
                ui.spacing_mut().interact_size = egui::vec2(44.0, 44.0);
                ui.horizontal_centered(|ui| {
                    if ui
                        .add_sized(
                            [76.0, 44.0],
                            egui::Button::new(egui::RichText::new("☰ Panels").size(14.0)),
                        )
                        .clicked()
                    {
                        self.mobile.drawer_open = true;
                        if let Some(id) = ctx.memory(|memory| memory.focused()) {
                            ctx.memory_mut(|memory| memory.surrender_focus(id));
                        }
                    }
                    let title_width = (ui.available_width() - 108.0).max(1.0);
                    ui.add_sized(
                        [title_width, 44.0],
                        egui::Label::new(egui::RichText::new(title).strong()).truncate(),
                    );
                    if ui
                        .add_enabled(
                            self.active_file.is_some(),
                            egui::Button::new("Save").min_size(egui::vec2(48.0, 44.0)),
                        )
                        .clicked()
                    {
                        self.save_file();
                    }
                    ui.menu_button("More", |ui| {
                        ui.set_max_width((ctx.viewport_rect().width() - 32.0).min(300.0));
                        if ui.button("Workspace…").clicked() {
                            self.choose_workspace(&ctx);
                            ui.close();
                        }
                        ui.vertical(|ui| self.toolbar_actions(ui));
                    });
                });
            });
        egui::Panel::bottom("mobile_status")
            .exact_size(22.0)
            .frame(
                egui::Frame::new()
                    .fill(theme::colors(root).chrome)
                    .inner_margin(egui::Margin::symmetric(8, 2)),
            )
            .show(root, |ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(
                        egui::RichText::new(format!("v{}", env!("CARGO_PKG_VERSION"))).small(),
                    );
                    ui.add(egui::Label::new(&self.status).truncate());
                });
            });
        // Header actions can change the selected panel, so use the current value.
        let panel = self.mobile.panel.clone().unwrap();
        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(theme::colors(root).editor)
                    .inner_margin(0),
            )
            .show(root, |ui| {
                ui.add_enabled_ui(!self.mobile.drawer_open, |ui| match panel {
                    MobilePanel::Editor => self.document_content(ui, opened_find, false),
                    MobilePanel::Explorer | MobilePanel::Search => self.sidebar_content(ui),
                    MobilePanel::Git => {
                        egui::Frame::new().inner_margin(8).show(ui, |ui| {
                            if self.mobile.git_detail {
                                self.git_ui.take_open_review_request();
                                if ui
                                    .add_sized(
                                        [ui.available_width(), 44.0],
                                        egui::Button::new("← Back to changes"),
                                    )
                                    .clicked()
                                {
                                    self.mobile.git_detail = false;
                                }
                                if !self.git_ui.show_diff(ui) {
                                    self.mobile.git_detail = false;
                                }
                            } else {
                                self.sidebar_content(ui);
                                if self.git_ui.take_open_review_request() {
                                    self.mobile.git_detail = true;
                                }
                            }
                        });
                    }
                    MobilePanel::Manager => {
                        egui::Frame::new().inner_margin(8).show(ui, |ui| {
                            if self.addons.manager_has_detail() {
                                if ui
                                    .add_sized(
                                        [ui.available_width(), 44.0],
                                        egui::Button::new("← Back to addons"),
                                    )
                                    .clicked()
                                {
                                    self.addons.dismiss_manager_detail();
                                }
                                self.addons
                                    .manager_central(ui, &mut self.settings.addon_enabled);
                            } else {
                                self.addons
                                    .manager_sidebar(ui, &mut self.settings.addon_enabled);
                            }
                        });
                    }
                    MobilePanel::LeftAddon(_) => {
                        egui::Frame::new().inner_margin(8).show(ui, |ui| {
                            if self.addons.detail {
                                if ui
                                    .add_sized(
                                        [ui.available_width(), 44.0],
                                        egui::Button::new("← Back to addon"),
                                    )
                                    .clicked()
                                {
                                    self.addons.detail = false;
                                }
                                self.addons.central_content(ui, false);
                            } else {
                                self.addons.sidebar(ui);
                            }
                        });
                    }
                    MobilePanel::RightAddon(id) => {
                        egui::Frame::new()
                            .inner_margin(8)
                            .show(ui, |ui| self.addons.right_content(ui, &id));
                    }
                    MobilePanel::Terminal | MobilePanel::Logs => {
                        let cwd = self.settings.workspace.clone();
                        let runtime = self.dev_container.runtime().cloned();
                        egui::Frame::new().inner_margin(8).show(ui, |ui| {
                            self.shell_content(ui, cwd.as_deref(), runtime.as_ref())
                        });
                        self.mobile.panel = Some(if self.bottom_tab == Tab::Terminal {
                            MobilePanel::Terminal
                        } else {
                            MobilePanel::Logs
                        });
                    }
                });
            });
        self.mobile_drawer(&ctx, &addons);
    }

    pub(super) fn mobile_drawer(
        &mut self,
        ctx: &egui::Context,
        addons: &[(String, String, focus_addon_sdk::Panel)],
    ) {
        if !self.mobile.drawer_open {
            return;
        }
        let mut chosen = None;
        let mut close = false;
        let width = (ctx.viewport_rect().width() - 44.0).clamp(200.0, 320.0);
        let id = egui::Id::new("mobile_panel_drawer");
        let response = egui::Modal::new(id)
            .area(
                egui::Modal::default_area(id)
                    .anchor(egui::Align2::LEFT_TOP, egui::Vec2::ZERO)
                    .fade_in(false),
            )
            .frame(
                egui::Frame::new()
                    .fill(theme::current(ctx).colors.panel)
                    .inner_margin(12),
            )
            .show(ctx, |ui| {
                ui.set_width(width - 24.0);
                ui.set_height((ctx.viewport_rect().height() - 24.0).max(44.0));
                ui.spacing_mut().interact_size.y = 44.0;
                ui.horizontal(|ui| {
                    ui.add_sized(
                        [width - 76.0, 44.0],
                        egui::Label::new(egui::RichText::new("Panels").strong()),
                    );
                    close = ui.add_sized([44.0, 44.0], egui::Button::new("×")).clicked();
                });
                ui.separator();
                egui::ScrollArea::vertical()
                    .max_height((ctx.viewport_rect().height() - 88.0).max(44.0))
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        let mut entries = vec![
                            (MobilePanel::Editor, "Editor".to_owned()),
                            (MobilePanel::Explorer, "Explorer".to_owned()),
                            (MobilePanel::Search, "Search".to_owned()),
                            (MobilePanel::Git, "Source Control".to_owned()),
                            (MobilePanel::Terminal, "Terminal".to_owned()),
                            (MobilePanel::Logs, "Logs".to_owned()),
                            (MobilePanel::Manager, "Addon Manager".to_owned()),
                        ];
                        entries.extend(addons.iter().map(|(id, name, side)| {
                            (
                                if *side == focus_addon_sdk::Panel::Left {
                                    MobilePanel::LeftAddon(id.clone())
                                } else {
                                    MobilePanel::RightAddon(id.clone())
                                },
                                name.clone(),
                            )
                        }));
                        for (panel, name) in entries {
                            if ui
                                .add_sized(
                                    [ui.available_width(), 44.0],
                                    egui::Button::new(egui::RichText::new(name).size(14.0))
                                        .selected(self.mobile.panel.as_ref() == Some(&panel)),
                                )
                                .clicked()
                            {
                                chosen = Some(panel);
                            }
                        }
                    });
            });
        if let Some(panel) = chosen {
            self.select_mobile_panel(panel, ctx);
        }
        if close || response.should_close() {
            self.mobile.drawer_open = false;
        }
    }
}
