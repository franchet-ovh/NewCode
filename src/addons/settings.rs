//! Manifest-declared workspace text settings; saving never touches the running process.
use super::{Addon, BTreeMap, egui};
use crate::ui_theme as theme;

pub(crate) struct SettingsUpdate {
    pub addon: String,
    pub values: BTreeMap<String, String>,
}

pub(super) struct Draft {
    addon: String,
    generation: u64,
    values: BTreeMap<String, String>,
    result: Option<Result<(), String>>,
}

pub(super) fn show(
    ui: &mut egui::Ui,
    draft: &mut Option<Draft>,
    addon: &Addon,
    generation: u64,
    enabled: bool,
    container: bool,
) -> Option<SettingsUpdate> {
    if addon.manifest.settings.is_empty() {
        return None;
    }
    let draft = draft.get_or_insert_with(|| Draft::new(addon, generation));
    if draft.addon != addon.manifest.id || draft.generation != generation {
        *draft = Draft::new(addon, generation);
    }
    let mut update = None;
    theme::card(ui, |ui| {
        ui.strong("Workspace settings");
        ui.label(if container {
            "Saved for this workspace. Values are used inside the active Dev Container."
        } else {
            "Saved for this workspace."
        });
        ui.label("Restart the addon after saving to apply changes.");
        ui.add_enabled_ui(enabled, |ui| {
            for setting in &addon.manifest.settings {
                let value = draft.values.entry(setting.key.clone()).or_default();
                if theme::text_field(
                    ui,
                    (&addon.manifest.id, &setting.key),
                    &setting.label,
                    value,
                    false,
                    &setting.placeholder,
                    &setting.description,
                )
                .changed()
                {
                    draft.result = None;
                }
            }
            if theme::role_button(ui, "Save settings", theme::ButtonRole::Primary).clicked() {
                draft.result = None;
                update = Some(SettingsUpdate {
                    addon: draft.addon.clone(),
                    values: draft.values.clone(),
                });
            }
        });
        if let Some(result) = &draft.result {
            match result {
                Ok(()) => theme::text(
                    ui,
                    "Settings saved. Restart the addon to apply changes.",
                    focus_addon_sdk::TextRole::Success,
                ),
                Err(error) => theme::text(ui, error, focus_addon_sdk::TextRole::Error),
            }
        }
    });
    ui.add_space(16.0);
    update
}

impl Draft {
    fn new(addon: &Addon, generation: u64) -> Self {
        Self {
            addon: addon.manifest.id.clone(),
            generation,
            values: addon
                .manifest
                .settings
                .iter()
                .map(|setting| {
                    (
                        setting.key.clone(),
                        addon.seed[&setting.key].as_str().unwrap_or_default().into(),
                    )
                })
                .collect(),
            result: None,
        }
    }

    pub(super) fn saved(&mut self, result: Result<(), String>) {
        self.result = Some(result);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use focus_addon_sdk::{Manifest, Setting};

    #[test]
    fn disabled_addon_settings_remain_editable_and_failed_drafts_can_be_retried() {
        let addon = Addon {
            manifest: Manifest {
                id: "opencode".into(),
                settings: vec![Setting {
                    key: "binary_path".into(),
                    label: "OpenCode binary".into(),
                    description: "Executable inside the active workspace runtime".into(),
                    placeholder: "opencode".into(),
                }],
                ..Default::default()
            },
            directory: Default::default(),
            storage: Default::default(),
            enabled: false,
            process: None,
            error: None,
            view: Default::default(),
            values: Default::default(),
            icon: None,
            working: false,
            incarnation: 0,
            surface: Default::default(),
            seed: serde_json::json!({"binary_path":"/old/opencode"}),
            status: String::new(),
        };
        let ctx = egui::Context::default();
        crate::ui_theme::apply(&ctx, crate::ui_theme::Appearance::Light);
        let mut draft = Some(Draft::new(&addon, 1));
        draft
            .as_mut()
            .unwrap()
            .values
            .insert("binary_path".into(), "/container tools/opencode".into());
        draft
            .as_mut()
            .unwrap()
            .saved(Err("Cannot write settings".into()));
        let mut update = None;
        let frame = |draft: &mut Option<Draft>,
                     update: &mut Option<SettingsUpdate>,
                     events,
                     enabled,
                     generation| {
            crate::test_frame(
                &ctx,
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(360.0, 700.0),
                    )),
                    events,
                    ..Default::default()
                },
                |root| {
                    egui::CentralPanel::default().show(root, |ui| {
                        if let Some(saved) = show(ui, draft, &addon, generation, enabled, true) {
                            *update = Some(saved);
                        }
                        assert!(ui.min_rect().right() <= 360.0);
                    });
                },
            )
        };
        let output = frame(&mut draft, &mut update, vec![], true, 1);
        let text_pos = |output: &egui::FullOutput, label: &str| {
            output
                .shapes
                .iter()
                .find_map(|shape| match &shape.shape {
                    egui::Shape::Text(text) if text.galley.job.text == label => {
                        Some(text.pos + text.galley.size() * 0.5)
                    }
                    _ => None,
                })
                .unwrap_or_else(|| panic!("Missing text: {label}"))
        };
        text_pos(&output, "Cannot write settings");
        for enabled in [false, true] {
            let output = frame(&mut draft, &mut update, vec![], enabled, 1);
            let pos = text_pos(&output, "Save settings");
            for pressed in [true, false] {
                frame(
                    &mut draft,
                    &mut update,
                    vec![
                        egui::Event::PointerMoved(pos),
                        egui::Event::PointerButton {
                            pos,
                            pressed,
                            button: egui::PointerButton::Primary,
                            modifiers: Default::default(),
                        },
                    ],
                    enabled,
                    1,
                );
            }
            assert_eq!(update.is_some(), enabled);
        }
        assert_eq!(
            update.unwrap().values["binary_path"],
            "/container tools/opencode"
        );
        frame(&mut draft, &mut None, vec![], true, 2);
        assert_eq!(draft.unwrap().values["binary_path"], "/old/opencode");
        assert!(!addon.enabled);
    }
}
