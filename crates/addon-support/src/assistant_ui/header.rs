use crate::ui_theme::{self as theme, Icon};
use egui;

pub struct Session {
    pub id: String,
    pub label: String,
    pub detail: String,
}
pub struct Header<'a> {
    pub name: &'a str,
    pub state: &'a str,
    pub status: &'a str,
    pub connected: bool,
    pub enabled: bool,
    pub selected: Option<&'a str>,
    pub sessions: &'a [Session],
    pub more: bool,
}
pub enum Action {
    Start,
    Stop,
    New,
    Select(String),
    Refresh,
    More,
}

pub fn show(ui: &mut egui::Ui, view: Header<'_>) -> Option<Action> {
    let mut action = None;
    ui.horizontal_wrapped(|ui| {
        theme::icon(ui, Icon::Spark, theme::colors(ui).accent);
        ui.label(
            egui::RichText::new(view.name)
                .strong()
                .color(theme::colors(ui).accent),
        );
        let (color, fill) = match view.state {
            "Running" => (theme::colors(ui).accent, theme::colors(ui).accent_soft),
            "Ready" => (theme::colors(ui).success, theme::colors(ui).panel),
            "Waiting for you" => (theme::colors(ui).warning, theme::colors(ui).panel),
            _ => (theme::colors(ui).muted, theme::colors(ui).panel),
        };
        theme::badge(ui, view.state, color, fill).on_hover_text(view.status);
        if view.connected {
            if theme::action_button(ui, Icon::Stop, "", focus_addon_sdk::ButtonRole::Secondary)
                .on_hover_text(format!("Stop {}", view.name))
                .clicked()
            {
                action = Some(Action::Stop);
            }
        } else if theme::action_button(
            ui,
            Icon::Spark,
            "Start",
            focus_addon_sdk::ButtonRole::Primary,
        )
        .clicked()
        {
            action = Some(Action::Start);
        }
    });
    ui.add_enabled_ui(view.enabled, |ui| {
        ui.horizontal(|ui| {
            let control = ui.spacing().interact_size.y;
            let font = egui::TextStyle::Button.resolve(ui.style());
            let color = theme::colors(ui).text;
            let more_width = ui
                .fonts_mut(|fonts| fonts.layout_no_wrap("…".into(), font, color).size().x)
                + 2.0 * ui.spacing().button_padding.x;
            let more_width = more_width.max(control);
            let width = (ui.available_width()
                - 2.0 * control
                - more_width
                - 3.0 * ui.spacing().item_spacing.x)
                .max(1.0);
            if theme::icon_button(ui, Icon::Plus, "New session").clicked() {
                action = Some(Action::New);
            }
            let current = view
                .sessions
                .iter()
                .find(|s| Some(s.id.as_str()) == view.selected);
            ui.allocate_ui_with_layout(
                egui::vec2(width, ui.spacing().interact_size.y),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    egui::ComboBox::from_id_salt("assistant_session")
                        .width(width)
                        .truncate()
                        .selected_text(current.map_or("Choose session…", |s| s.label.as_str()))
                        .show_ui(ui, |ui| {
                            if view.sessions.is_empty() {
                                ui.weak("No saved sessions");
                            }
                            for session in view.sessions {
                                if ui
                                    .selectable_label(
                                        Some(session.id.as_str()) == view.selected,
                                        &session.detail,
                                    )
                                    .on_hover_text(&session.detail)
                                    .clicked()
                                    && Some(session.id.as_str()) != view.selected
                                {
                                    action = Some(Action::Select(session.id.clone()));
                                }
                            }
                        });
                },
            );
            if theme::icon_button(ui, Icon::Refresh, "Refresh sessions and conversation").clicked()
            {
                action = Some(Action::Refresh);
            }
            if ui
                .add_enabled(
                    view.more,
                    crate::ui_theme::control_button("…").min_size(egui::vec2(more_width, control)),
                )
                .on_hover_text("Load older sessions")
                .clicked()
            {
                action = Some(Action::More);
            }
        });
    });
    if !view.connected || view.state == "Connecting" {
        ui.add(
            egui::Label::new(
                egui::RichText::new(view.status)
                    .small()
                    .color(theme::colors(ui).muted),
            )
            .wrap(),
        );
    }
    action
}
