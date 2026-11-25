//! Editor-owned presets and terminal font registration. Addons receive resolved appearance.
use eframe::egui::{self, Color32, FontId, Stroke};
pub(crate) use focus_addon_support::ui_theme::*;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Appearance {
    #[default]
    Light,
    Dark,
}

pub(crate) fn preset(appearance: Appearance) -> Theme {
    let rgb = |hex: u32| Color32::from_rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8);
    // Approved reference: ui-mocks/ui-upgrade.html.
    let mut colors = Colors {
        text: rgb(0x263746),
        muted: rgb(0x617587),
        faint: rgb(0x91a2ae),
        accent: rgb(0x436d94),
        accent_soft: rgb(0xe3edf5),
        accent_strong: rgb(0x355b7f),
        on_accent: Color32::WHITE,
        window: rgb(0xf8fafb),
        chrome: rgb(0xedf2f5),
        panel: rgb(0xf3f6f8),
        raised: rgb(0xffffff),
        editor: rgb(0xfcfdfe),
        border: rgb(0xdce4ea),
        border_strong: rgb(0xbdcbd6),
        activity: rgb(0xedf2f5),
        activity_text: rgb(0x4d6373),
        selection: rgb(0xdce8f1),
        hover: rgb(0xe2e9ee),
        terminal: rgb(0xe9eff3),
        terminal_text: rgb(0x314756),
        success: rgb(0x327864),
        warning: rgb(0x98652c),
        error: rgb(0xaf4654),
        purple: rgb(0x6c68a8),
        success_soft: rgb(0xe6f2ec),
        warning_soft: rgb(0xfbf1df),
        error_soft: rgb(0xf9e9ec),
        keyword: rgb(0x7959a8),
        string: rgb(0x387e5b),
        type_name: rgb(0xa26025),
    };
    if appearance == Appearance::Dark {
        colors = Colors {
            text: rgb(0xe0e8ef),
            muted: rgb(0xa4b5c5),
            faint: rgb(0x8296a9),
            accent: rgb(0x91b9dc),
            accent_soft: rgb(0x354b60),
            accent_strong: rgb(0xb0d0eb),
            on_accent: rgb(0x222d38),
            window: rgb(0x242f3c),
            chrome: rgb(0x293644),
            panel: rgb(0x26323f),
            raised: rgb(0x303e4c),
            editor: rgb(0x222d38),
            border: rgb(0x3b4a59),
            border_strong: rgb(0x526477),
            activity: rgb(0x293644),
            activity_text: rgb(0xa4b5c5),
            selection: rgb(0x354b60),
            hover: rgb(0x354b60),
            terminal: rgb(0x26323f),
            terminal_text: rgb(0xe0e8ef),
            success: rgb(0x8bc5ad),
            warning: rgb(0xdfbd86),
            error: rgb(0xed9fa9),
            purple: rgb(0xc3ade2),
            success_soft: rgb(0x304a43),
            warning_soft: rgb(0x4c4434),
            error_soft: rgb(0x503740),
            keyword: rgb(0xc3ade2),
            string: rgb(0x8bc5ad),
            type_name: rgb(0xdfbd86),
        };
    }
    let metrics = Metrics::default();
    let mut style = egui::Style::default();
    let v = &mut style.visuals;
    *v = if appearance == Appearance::Dark {
        egui::Visuals::dark()
    } else {
        egui::Visuals::light()
    };
    v.override_text_color = Some(colors.text);
    v.weak_text_color = Some(colors.muted);
    v.panel_fill = colors.panel;
    v.window_fill = colors.raised;
    v.extreme_bg_color = colors.raised;
    v.faint_bg_color = colors.hover;
    v.code_bg_color = colors.chrome;
    v.window_corner_radius = egui::CornerRadius::same(metrics.corner_radius);
    v.menu_corner_radius = egui::CornerRadius::same(metrics.corner_radius);
    v.window_stroke = Stroke::new(1.0, colors.border_strong);
    v.selection.bg_fill = colors.selection;
    v.selection.stroke = Stroke::new(1.0, colors.accent);
    v.hyperlink_color = colors.accent_strong;
    v.warn_fg_color = colors.warning;
    v.error_fg_color = colors.error;
    v.text_cursor.stroke = Stroke::new(2.0, colors.accent);
    v.text_cursor.blink = false;
    for widget in [
        &mut v.widgets.inactive,
        &mut v.widgets.noninteractive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
        &mut v.widgets.open,
    ] {
        widget.corner_radius = egui::CornerRadius::same(metrics.corner_radius);
        widget.bg_stroke = Stroke::new(1.0, colors.border);
        widget.fg_stroke = Stroke::new(1.0, colors.text);
        widget.expansion = 0.0;
    }
    v.widgets.inactive.bg_fill = colors.raised;
    v.widgets.inactive.weak_bg_fill = Color32::TRANSPARENT;
    v.widgets.noninteractive.bg_fill = colors.panel;
    v.widgets.noninteractive.weak_bg_fill = Color32::TRANSPARENT;
    v.widgets.hovered.bg_fill = colors.hover;
    v.widgets.hovered.weak_bg_fill = colors.hover;
    v.widgets.hovered.bg_stroke.color = colors.border_strong;
    v.widgets.active.bg_fill = colors.selection;
    v.widgets.active.weak_bg_fill = colors.selection;
    v.widgets.active.bg_stroke.color = colors.accent;
    v.widgets.open = v.widgets.active;
    style.spacing.item_spacing = egui::vec2(metrics.spacing, metrics.spacing);
    style.spacing.button_padding = egui::vec2(10.0, 7.0);
    style.spacing.interact_size = egui::vec2(metrics.control_height, metrics.control_height);
    style.spacing.indent = 14.0;
    for (kind, font) in [
        (egui::TextStyle::Body, FontId::proportional(14.0)),
        (egui::TextStyle::Button, FontId::proportional(12.0)),
        (egui::TextStyle::Small, FontId::proportional(11.0)),
        (egui::TextStyle::Heading, FontId::proportional(19.0)),
        (egui::TextStyle::Monospace, FontId::monospace(12.0)),
    ] {
        style.text_styles.insert(kind, font);
    }
    let mut theme = Theme::from_style(style);
    theme.colors = colors;
    theme
}

pub(crate) fn apply(ctx: &egui::Context, appearance: Appearance) {
    crate::terminal_fonts::apply(ctx);
    apply_theme(ctx, &preset(appearance));
}
