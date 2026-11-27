use super::*;
use crate::ui_theme as theme;
use std::fs;

impl Addon {
    pub(super) fn load_icon(&mut self, ctx: &egui::Context) {
        if self.icon.is_none() {
            if let Ok(bytes) = fs::read(self.directory.join(&self.manifest.icon)) {
                if let Ok(image) =
                    image::load_from_memory_with_format(&bytes, image::ImageFormat::Png)
                {
                    let image = image.to_rgba8();
                    let image = egui::ColorImage::from_rgba_unmultiplied(
                        [image.width() as usize, image.height() as usize],
                        image.as_raw(),
                    );
                    self.icon =
                        Some(ctx.load_texture(&self.manifest.id, image, Default::default()));
                }
            }
        }
    }
}

impl Addons {
    pub fn panel_button(
        &mut self,
        ui: &mut egui::Ui,
        id: &str,
        label: String,
        selected: bool,
    ) -> egui::Response {
        let icon = self
            .entries
            .iter_mut()
            .find(|addon| addon.manifest.id == id)
            .and_then(|addon| {
                addon.load_icon(ui.ctx());
                addon.icon.clone()
            });
        let button = if let Some(icon) = icon {
            egui::Button::image_and_text(
                egui::Image::new((icon.id(), egui::vec2(20.0, 20.0))),
                label,
            )
        } else {
            focus_addon_support::ui_theme::control_button(label)
        };
        ui.add(button.selected(selected))
    }

    pub fn panel_entries(&self) -> Vec<(String, String, focus_addon_sdk::Panel)> {
        self.entries
            .iter()
            .filter(|addon| addon.enabled && addon.manifest.panel != focus_addon_sdk::Panel::None)
            .map(|addon| {
                (
                    addon.manifest.id.clone(),
                    addon.manifest.name.clone(),
                    addon.manifest.panel,
                )
            })
            .collect()
    }
    pub fn menu(&mut self, ui: &mut egui::Ui, _enabled: &mut BTreeMap<String, bool>) {
        if ui
            .add(focus_addon_support::ui_theme::control_button(
                "Manage addons…",
            ))
            .clicked()
        {
            self.open_manager();
            ui.close();
        }
    }
    pub fn sidebar(&mut self, ui: &mut egui::Ui) {
        let Some(id) = self.active.clone() else {
            ui.label("Select an addon");
            return;
        };
        if self
            .entries
            .iter()
            .any(|a| a.manifest.id == id && a.enabled && a.process.is_none() && a.error.is_none())
        {
            self.activate(id.clone(), ui.ctx());
        }
        let Some(addon) = self.entries.iter_mut().find(|a| a.manifest.id == id) else {
            return;
        };
        theme::panel_header(ui, &addon.manifest.name);
        if let Some(error) = &addon.error {
            theme::text(ui, error, focus_addon_sdk::TextRole::Error);
            if ui
                .add(focus_addon_support::ui_theme::control_button("Restart"))
                .clicked()
            {
                addon.error = None;
            }
            return;
        }
        if addon.working || addon.process.as_ref().is_none_or(|p| !p.ready) {
            ui.spinner();
        }
        let mut action = None;
        egui::ScrollArea::vertical()
            .id_salt(("addon-sidebar", &id))
            .show(ui, |ui| {
                widgets::render(
                    ui,
                    &addon.view.sidebar,
                    &mut addon.values,
                    &mut self.cache,
                    &mut action,
                    addon.working,
                );
            });
        if let Some(action) = action {
            self.action(&id, action);
        }
    }
    pub fn central(&mut self, ui: &mut egui::Ui) -> bool {
        self.central_content(ui, true)
    }

    pub fn central_content(&mut self, ui: &mut egui::Ui, show_header: bool) -> bool {
        if !self.detail {
            return false;
        }
        let Some(id) = self.active.clone() else {
            return false;
        };
        let Some(addon) = self.entries.iter_mut().find(|a| a.manifest.id == id) else {
            return false;
        };
        if show_header {
            ui.horizontal(|ui| {
                ui.heading(&addon.view.title);
                if ui
                    .add(focus_addon_support::ui_theme::control_button(
                        "Back to editor",
                    ))
                    .clicked()
                {
                    self.detail = false;
                }
            });
        }
        let mut action = None;
        egui::ScrollArea::vertical()
            .id_salt(("addon-detail", &id))
            .show(ui, |ui| {
                widgets::render(
                    ui,
                    &addon.view.detail,
                    &mut addon.values,
                    &mut self.cache,
                    &mut action,
                    addon.working,
                );
            });
        if let Some(action) = action {
            self.action(&id, action);
        }
        true
    }
    pub(super) fn action(&mut self, id: &str, action: String) {
        if self.installation_pending(id) {
            return;
        }
        if action == "host.credentials" {
            self.credentials = Some(Credentials {
                addon: id.to_owned(),
                secret: String::new(),
                result: String::new(),
                pending: None,
            });
            return;
        }
        if let Some(addon) = self.entries.iter_mut().find(|a| a.manifest.id == id) {
            if let Some(process) = &addon.process {
                let message = Action {
                    generation: self.context.generation,
                    id: action,
                    values: addon.values.clone(),
                };
                if let Err(error) = process.send(Message::notification("action", message)) {
                    addon.error = Some(error);
                }
                addon.working = true;
            }
        }
    }
    pub fn activity(&mut self, ui: &mut egui::Ui, selected: bool) -> bool {
        let mut chosen = None;
        for addon in self
            .entries
            .iter_mut()
            .filter(|a| a.enabled && a.manifest.panel == focus_addon_sdk::Panel::Left)
        {
            addon.load_icon(ui.ctx());
            let active = selected && self.active.as_ref() == Some(&addon.manifest.id);
            let response = if let Some(icon) = &addon.icon {
                theme::image_nav_button(ui, icon, active, &addon.manifest.name)
            } else {
                theme::nav_button(ui, theme::Icon::Addons, active, &addon.manifest.name)
            };
            if response.on_hover_text(&addon.manifest.name).clicked() {
                chosen = Some(addon.manifest.id.clone());
            }
        }
        if let Some(id) = chosen {
            self.activate(id, ui.ctx());
            true
        } else {
            false
        }
    }
}
