//! Reusable controls for editor panels and independently rendered addons.
use crate::ui_theme::{colors, metrics};
use egui::{self, Response, Ui};
use focus_addon_sdk::{ButtonRole, TextRole};

pub fn control_button<'a>(label: impl Into<egui::WidgetText>) -> egui::Button<'a> {
    // Size, padding, radius, and states come from the current host style.
    egui::Button::new(label).wrap()
}

pub fn role_button(ui: &mut Ui, label: &str, role: ButtonRole) -> Response {
    let c = colors(ui);
    let button = match role {
        ButtonRole::Primary => control_button(egui::RichText::new(label).color(c.on_accent))
            .fill(c.accent)
            .stroke(egui::Stroke::new(1.0, c.accent)),
        ButtonRole::Destructive => control_button(egui::RichText::new(label).color(c.error))
            .stroke(egui::Stroke::new(1.0, c.error)),
        ButtonRole::Quiet => control_button(egui::RichText::new(label).color(c.muted)).frame(false),
        ButtonRole::Secondary => control_button(label),
    };
    ui.add(button)
}

pub const FIELD_MARGIN: egui::Margin = egui::Margin::symmetric(10, 8);

pub fn singleline(text: &mut dyn egui::TextBuffer) -> egui::TextEdit<'_> {
    egui::TextEdit::singleline(text).margin(FIELD_MARGIN)
}
pub fn multiline(text: &mut dyn egui::TextBuffer) -> egui::TextEdit<'_> {
    egui::TextEdit::multiline(text)
        .margin(FIELD_MARGIN)
        .desired_rows(5)
}

/// Field labels are separate from hints, so populated fields remain identifiable.
pub fn text_field(
    ui: &mut Ui,
    id: impl std::hash::Hash + std::fmt::Debug,
    label: &str,
    text: &mut String,
    multiline_field: bool,
    placeholder: &str,
    help: &str,
) -> Response {
    ui.vertical(|ui| {
        let label = ui.label(egui::RichText::new(label).small().strong());
        let edit = if multiline_field {
            multiline(text)
        } else {
            singleline(text)
        };
        let id = ui.make_persistent_id(id);
        let edit = edit
            .id(id)
            .hint_text(placeholder)
            .desired_width(f32::INFINITY);
        let response = if multiline_field {
            egui::ScrollArea::vertical()
                .id_salt(id.with("scroll"))
                .max_height(metrics(ui).text_area_max_height)
                .min_scrolled_height(0.0)
                .show(ui, |ui| ui.add(edit))
                .inner
        } else {
            ui.add(edit)
        };
        let response = response.labelled_by(label.id);
        if !help.is_empty() {
            ui.label(egui::RichText::new(help).small().color(colors(ui).muted));
        }
        response
    })
    .inner
}

pub fn card<R>(ui: &mut Ui, content: impl FnOnce(&mut Ui) -> R) -> egui::InnerResponse<R> {
    let c = colors(ui);
    egui::Frame::new()
        .fill(c.raised)
        .stroke(egui::Stroke::new(1.0, c.border))
        .corner_radius(metrics(ui).corner_radius)
        .inner_margin(16)
        .show(ui, content)
}

pub fn text(ui: &mut Ui, value: &str, role: TextRole) {
    let c = colors(ui);
    match role {
        TextRole::Heading => {
            ui.add_space(8.0);
            ui.heading(value);
        }
        TextRole::Body => {
            ui.add(egui::Label::new(value).wrap().selectable(true));
        }
        TextRole::Muted => {
            ui.add(egui::Label::new(egui::RichText::new(value).small().color(c.muted)).wrap());
        }
        _ => {
            let (color, fill) = match role {
                TextRole::Success => (c.success, c.success_soft),
                TextRole::Warning => (c.warning, c.warning_soft),
                _ => (c.error, c.error_soft),
            };
            egui::Frame::new()
                .fill(fill)
                .corner_radius(metrics(ui).corner_radius)
                .inner_margin(10)
                .show(ui, |ui| {
                    ui.add(
                        egui::Label::new(egui::RichText::new(value).color(color))
                            .wrap()
                            .selectable(true),
                    );
                });
        }
    }
}

pub fn list_row(ui: &mut Ui, title: &str, description: &str, selected: bool) -> Response {
    let c = colors(ui);
    let mut job = egui::text::LayoutJob::default();
    job.append(
        title,
        0.0,
        egui::TextFormat::simple(egui::TextStyle::Body.resolve(ui.style()), c.text),
    );
    if !description.is_empty() {
        job.append(
            &format!("\n{description}"),
            0.0,
            egui::TextFormat::simple(egui::TextStyle::Small.resolve(ui.style()), c.muted),
        );
    }
    let button = control_button(job)
        .selected(selected)
        .min_size(egui::vec2(ui.available_width(), 48.0));
    ui.add(button)
}

pub fn empty_state(ui: &mut Ui, title: &str, help: &str) {
    card(ui, |ui| {
        ui.strong(title);
        ui.add(egui::Label::new(egui::RichText::new(help).small().color(colors(ui).muted)).wrap());
    });
}

pub fn panel_header(ui: &mut Ui, title: &str) {
    let c = colors(ui);
    egui::Frame::new()
        .fill(c.chrome)
        .inner_margin(egui::Margin::symmetric(12, 10))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.strong(title);
        });
    ui.add_space(metrics(ui).spacing);
}
