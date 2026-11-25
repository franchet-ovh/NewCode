use crate::assistant_ui::content::{CommonMarkCache, markdown};
use crate::ui_theme as theme;
use eframe::egui;
use focus_addon_sdk::{RowLayout, Widget};
use serde_json::Value;
use std::collections::BTreeMap;

/// Reactive choices are owned by the addon so dependent defaults can change
/// without clearing unrelated text fields in the same form.
pub(super) fn sync_selections(widgets: &[Widget], values: &mut BTreeMap<String, Value>) {
    for widget in widgets {
        match widget {
            Widget::Select {
                id,
                value,
                on_change: Some(_),
                ..
            } => {
                values.insert(id.clone(), Value::String(value.clone()));
            }
            Widget::Row { children, .. } => sync_selections(children, values),
            _ => (),
        }
    }
}

pub(super) fn render(
    ui: &mut egui::Ui,
    widgets: &[Widget],
    values: &mut BTreeMap<String, Value>,
    cache: &mut CommonMarkCache,
    action: &mut Option<String>,
    busy: bool,
) {
    for widget in widgets {
        match widget {
            Widget::Text { text, role } => theme::text(ui, text, *role),
            Widget::Markdown { text } => markdown(ui, cache, text),
            Widget::Separator => {
                ui.separator();
            }
            Widget::Button {
                id,
                label,
                enabled,
                style,
            } => {
                let response = ui
                    .push_id(id, |ui| {
                        ui.add_enabled_ui(*enabled && !busy, |ui| {
                            if !style.description.is_empty() {
                                theme::list_row(ui, label, &style.description, style.selected)
                            } else if style.selected {
                                ui.add(theme::control_button(label).selected(true))
                            } else {
                                theme::role_button(ui, label, style.role)
                            }
                        })
                        .inner
                    })
                    .inner;
                if response.clicked() {
                    *action = Some(id.clone());
                }
            }
            Widget::Input {
                id,
                label,
                value,
                multiline,
                options,
            } => {
                let entry = values
                    .entry(id.clone())
                    .or_insert_with(|| Value::String(value.clone()));
                let mut text = entry.as_str().unwrap_or_default().to_owned();
                let response = theme::text_field(
                    ui,
                    id,
                    label,
                    &mut text,
                    *multiline,
                    &options.placeholder,
                    &options.help,
                );
                *entry = Value::String(text);
                if (response.changed() || entry.as_str() != Some(value.as_str())) && !busy {
                    if let Some(id) = &options.on_change {
                        *action = Some(id.clone());
                    }
                }
            }
            Widget::Checkbox { id, label, checked } => {
                let entry = values.entry(id.clone()).or_insert(Value::Bool(*checked));
                let mut checked = entry.as_bool().unwrap_or(false);
                ui.push_id(id, |ui| {
                    ui.checkbox(&mut checked, label);
                });
                *entry = Value::Bool(checked);
            }
            Widget::Select {
                id,
                label,
                value: value_default,
                options,
                on_change,
            } => {
                let entry = values
                    .entry(id.clone())
                    .or_insert_with(|| Value::String(value_default.clone()));
                let mut value = entry.as_str().unwrap_or_default().to_owned();
                if !options.iter().any(|(key, _)| key == &value) {
                    value = value_default.clone();
                }
                let previous = value.clone();
                let label_response = ui.label(egui::RichText::new(label).small().strong());
                let response = egui::ComboBox::from_id_salt(id)
                    .selected_text(
                        options
                            .iter()
                            .find(|(key, _)| key == &value)
                            .map_or(value.as_str(), |(_, label)| label),
                    )
                    .show_ui(ui, |ui| {
                        for (key, label) in options {
                            ui.selectable_value(&mut value, key.clone(), label);
                        }
                    });
                response.response.labelled_by(label_response.id);
                *entry = Value::String(value);
                if entry.as_str() != Some(previous.as_str()) {
                    if let Some(id) = on_change {
                        *action = Some(id.clone());
                    }
                }
            }
            Widget::Link { label, url } => {
                if reqwest::Url::parse(url).is_ok_and(|u| matches!(u.scheme(), "http" | "https")) {
                    ui.hyperlink_to(label, url);
                }
            }
            Widget::Row { children, layout } => match layout {
                RowLayout::Wrap => {
                    ui.horizontal_wrapped(|ui| render(ui, children, values, cache, action, busy));
                }
                RowLayout::Card => {
                    theme::card(ui, |ui| render(ui, children, values, cache, action, busy));
                    ui.add_space(theme::metrics(ui).spacing);
                }
                RowLayout::Columns if ui.available_width() >= 560.0 && !children.is_empty() => {
                    ui.columns(children.len(), |columns| {
                        for (column, child) in columns.iter_mut().zip(children) {
                            render(
                                column,
                                std::slice::from_ref(child),
                                values,
                                cache,
                                action,
                                busy,
                            );
                        }
                    });
                }
                _ => {
                    ui.vertical(|ui| render(ui, children, values, cache, action, busy));
                }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dependent_select_defaults_update_without_losing_branch_edits() {
        let mut values = BTreeMap::from([
            ("branch".into(), Value::String("my-edited-branch".into())),
            ("reasoning".into(), Value::String(String::new())),
        ]);
        sync_selections(
            &[Widget::Row {
                layout: Default::default(),
                children: vec![Widget::Select {
                    id: "reasoning".into(),
                    label: "Reasoning".into(),
                    value: "high".into(),
                    options: vec![("high".into(), "High".into())],
                    on_change: Some("options".into()),
                }],
            }],
            &mut values,
        );
        assert_eq!(values["reasoning"], "high");
        assert_eq!(values["branch"], "my-edited-branch");
    }
    #[test]
    fn reactive_input_delivers_latest_edit_after_busy_update() {
        let ctx = egui::Context::default();
        crate::ui_theme::apply(&ctx, crate::ui_theme::Appearance::Light);
        let widgets =
            [
                Widget::input("query", "Filter", "old").input_options(
                    "Search…",
                    "",
                    Some("filter"),
                ),
            ];
        let mut values = BTreeMap::from([
            ("query".into(), Value::String("latest edit".into())),
            ("branch".into(), Value::String("keep-me".into())),
        ]);
        let mut cache = CommonMarkCache::default();
        for busy in [true, false] {
            let mut action = None;
            crate::test_frame(&ctx, Default::default(), |root| {
                egui::CentralPanel::default().show(root, |ui| {
                    render(ui, &widgets, &mut values, &mut cache, &mut action, busy)
                });
            });
            assert_eq!(action.as_deref(), if busy { None } else { Some("filter") });
            assert_eq!(values["branch"], "keep-me");
        }
    }

    #[test]
    fn grouped_forms_and_navigation_fit_narrow_panels() {
        let widgets = [
            Widget::row(
                RowLayout::Card,
                vec![
                    Widget::heading("Issue branch"),
                    Widget::row(
                        RowLayout::Columns,
                        vec![
                            Widget::input("branch", "Branch name", "issue-75"),
                            Widget::input("base", "Base branch", "main"),
                        ],
                    ),
                    Widget::Input {
                        id: "body".into(),
                        label: "Description".into(),
                        value: "Long draft line\n".repeat(300),
                        multiline: true,
                        options: Default::default(),
                    },
                    Widget::button("start", "Create issue branch and checkout")
                        .styled(focus_addon_sdk::ButtonRole::Primary),
                ],
            ),
            Widget::button(
                "item:75",
                "A long issue title that should wrap within the panel",
            )
            .description("#75 · selected issue")
            .selected(true),
        ];
        for width in [150.0, 320.0, 700.0] {
            let ctx = egui::Context::default();
            crate::ui_theme::apply(&ctx, crate::ui_theme::Appearance::Dark);
            let mut values = BTreeMap::new();
            let mut cache = CommonMarkCache::default();
            crate::test_frame(
                &ctx,
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(width, 900.0),
                    )),
                    ..Default::default()
                },
                |root| {
                    egui::CentralPanel::default().show(root, |ui| {
                        let mut action = None;
                        render(ui, &widgets, &mut values, &mut cache, &mut action, false);
                        assert!(
                            ui.min_rect().bottom() <= 900.0,
                            "Long text areas must scroll within the panel"
                        );
                        assert!(
                            ui.min_rect().right() <= width,
                            "{width}: {:?}",
                            ui.min_rect()
                        );
                    });
                },
            );
            assert_eq!(values["branch"], "issue-75");
            assert_eq!(values["body"], "Long draft line\n".repeat(300));
        }
    }
}
