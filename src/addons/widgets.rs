use crate::assistant_ui::content::{CommonMarkCache, markdown};
use eframe::egui;
use focus_addon_sdk::Widget;
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
            Widget::Row { children } => sync_selections(children, values),
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
            Widget::Text { text } => {
                ui.add(egui::Label::new(text).wrap().selectable(true));
            }
            Widget::Markdown { text } => markdown(ui, cache, text),
            Widget::Separator => {
                ui.separator();
            }
            Widget::Button { id, label, enabled } => {
                if ui
                    .add_enabled(*enabled && !busy, egui::Button::new(label).wrap())
                    .clicked()
                {
                    *action = Some(id.clone());
                }
            }
            Widget::Input {
                id,
                label,
                value,
                multiline,
            } => {
                ui.label(label);
                let entry = values
                    .entry(id.clone())
                    .or_insert_with(|| Value::String(value.clone()));
                let mut text = entry.as_str().unwrap_or_default().to_owned();
                ui.push_id(id, |ui| {
                    if *multiline {
                        egui::ScrollArea::vertical()
                            .max_height(180.0)
                            .show(ui, |ui| {
                                ui.add(
                                    egui::TextEdit::multiline(&mut text)
                                        .desired_rows(5)
                                        .desired_width(f32::INFINITY),
                                );
                            });
                    } else {
                        ui.add(egui::TextEdit::singleline(&mut text).desired_width(f32::INFINITY));
                    }
                });
                *entry = Value::String(text);
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
                egui::ComboBox::from_id_salt(id)
                    .selected_text(
                        options
                            .iter()
                            .find(|(key, _)| key == &value)
                            .map_or(value.as_str(), |(_, label)| label),
                    )
                    .show_ui(ui, |ui| {
                        ui.label(label);
                        for (key, label) in options {
                            ui.selectable_value(&mut value, key.clone(), label);
                        }
                    });
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
            Widget::Row { children } => {
                ui.horizontal_wrapped(|ui| render(ui, children, values, cache, action, busy));
            }
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
}
