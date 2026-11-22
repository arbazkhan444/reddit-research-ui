use eframe::egui::{
    self, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Margin, Response,
    RichText, Stroke, StrokeKind, Vec2,
};
use std::sync::Arc;

#[derive(Clone, Copy)]
pub struct Palette {
    pub bg: Color32,
    pub panel: Color32,
    pub surface: Color32,
    pub surface_hover: Color32,
    pub border: Color32,
    pub text: Color32,
    pub muted: Color32,
    pub faint: Color32,
    pub accent: Color32,
    pub accent_hover: Color32,
    pub on_accent: Color32,
    pub success: Color32,
    pub danger: Color32,
    pub danger_hover: Color32,
    pub post: Color32,
    pub comment: Color32,
}

pub fn palette(dark: bool) -> Palette {
    if dark {
        Palette {
            bg: Color32::from_rgb(10, 12, 16),
            panel: Color32::from_rgb(15, 18, 24),
            surface: Color32::from_rgb(22, 26, 34),
            surface_hover: Color32::from_rgb(30, 35, 46),
            border: Color32::from_rgb(39, 45, 58),
            text: Color32::from_rgb(232, 237, 245),
            muted: Color32::from_rgb(142, 152, 170),
            faint: Color32::from_rgb(96, 106, 124),
            accent: Color32::from_rgb(110, 155, 255),
            accent_hover: Color32::from_rgb(132, 172, 255),
            on_accent: Color32::from_rgb(8, 13, 24),
            success: Color32::from_rgb(61, 204, 145),
            danger: Color32::from_rgb(255, 107, 107),
            danger_hover: Color32::from_rgb(255, 132, 132),
            post: Color32::from_rgb(255, 170, 96),
            comment: Color32::from_rgb(96, 196, 255),
        }
    } else {
        Palette {
            bg: Color32::from_rgb(244, 246, 250),
            panel: Color32::from_rgb(255, 255, 255),
            surface: Color32::from_rgb(255, 255, 255),
            surface_hover: Color32::from_rgb(240, 243, 248),
            border: Color32::from_rgb(225, 230, 239),
            text: Color32::from_rgb(17, 24, 39),
            muted: Color32::from_rgb(102, 112, 133),
            faint: Color32::from_rgb(152, 162, 179),
            accent: Color32::from_rgb(37, 99, 235),
            accent_hover: Color32::from_rgb(29, 78, 216),
            on_accent: Color32::WHITE,
            success: Color32::from_rgb(18, 128, 92),
            danger: Color32::from_rgb(217, 45, 32),
            danger_hover: Color32::from_rgb(185, 28, 28),
            post: Color32::from_rgb(181, 71, 8),
            comment: Color32::from_rgb(2, 106, 162),
        }
    }
}

pub fn tint(color: Color32, alpha: u8) -> Color32 {
    Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), alpha)
}

pub fn bold_font(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("bold".into()))
}

pub fn bold(text: impl Into<String>, size: f32) -> RichText {
    RichText::new(text).font(bold_font(size))
}

pub fn install_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    if let Ok(bytes) = std::fs::read(r"C:\Windows\Fonts\segoeui.ttf") {
        fonts
            .font_data
            .insert("segoe-ui".to_owned(), Arc::new(FontData::from_owned(bytes)));
        if let Some(chain) = fonts.families.get_mut(&FontFamily::Proportional) {
            chain.insert(0, "segoe-ui".to_owned());
        }
    }
    let fallback = fonts
        .families
        .get(&FontFamily::Proportional)
        .cloned()
        .unwrap_or_default();
    let mut bold_chain = Vec::new();
    if let Ok(bytes) = std::fs::read(r"C:\Windows\Fonts\segoeuib.ttf") {
        fonts.font_data.insert(
            "segoe-ui-bold".to_owned(),
            Arc::new(FontData::from_owned(bytes)),
        );
        bold_chain.push("segoe-ui-bold".to_owned());
    }
    bold_chain.extend(fallback);
    fonts
        .families
        .insert(FontFamily::Name("bold".into()), bold_chain);
    ctx.set_fonts(fonts);
}

fn widget(bg: Color32, border: Color32, fg: Color32, radius: u8) -> egui::style::WidgetVisuals {
    egui::style::WidgetVisuals {
        bg_fill: bg,
        weak_bg_fill: bg,
        bg_stroke: Stroke::new(1.0, border),
        corner_radius: CornerRadius::same(radius),
        fg_stroke: Stroke::new(1.0, fg),
        expansion: 0.0,
    }
}

pub fn apply(ctx: &egui::Context, dark: bool) {
    let p = palette(dark);
    let theme = if dark {
        egui::Theme::Dark
    } else {
        egui::Theme::Light
    };
    let mut style = (*ctx.style_of(theme)).clone();
    let mut visuals = if dark {
        egui::Visuals::dark()
    } else {
        egui::Visuals::light()
    };
    visuals.dark_mode = dark;
    visuals.override_text_color = Some(p.text);
    visuals.weak_text_color = Some(p.muted);
    visuals.panel_fill = p.bg;
    visuals.window_fill = p.panel;
    visuals.window_stroke = Stroke::new(1.0, p.border);
    visuals.window_corner_radius = CornerRadius::same(14);
    visuals.menu_corner_radius = CornerRadius::same(10);
    visuals.extreme_bg_color = p.surface;
    visuals.text_edit_bg_color = Some(p.surface);
    visuals.faint_bg_color = p.surface;
    visuals.code_bg_color = p.surface;
    visuals.hyperlink_color = p.accent;
    visuals.warn_fg_color = p.post;
    visuals.error_fg_color = p.danger;
    visuals.selection.bg_fill = tint(p.accent, if dark { 110 } else { 70 });
    visuals.selection.stroke = Stroke::new(1.0, p.text);
    visuals.slider_trailing_fill = true;
    visuals.button_frame = true;
    visuals.collapsing_header_frame = false;
    visuals.indent_has_left_vline = false;
    visuals.striped = false;
    visuals.window_highlight_topmost = false;
    visuals.interact_cursor = Some(egui::CursorIcon::PointingHand);
    visuals.disabled_alpha = 0.45;
    visuals.widgets.noninteractive = widget(p.panel, p.border, p.text, 8);
    visuals.widgets.inactive = widget(p.surface, p.border, p.text, 8);
    visuals.widgets.hovered = widget(p.surface_hover, tint(p.accent, 150), p.text, 8);
    visuals.widgets.active = widget(p.surface_hover, p.accent, p.text, 8);
    visuals.widgets.open = visuals.widgets.hovered;
    style.visuals = visuals;
    style.spacing.item_spacing = Vec2::new(8.0, 8.0);
    style.spacing.button_padding = Vec2::new(12.0, 7.0);
    style.spacing.window_margin = Margin::symmetric(18, 16);
    style.spacing.menu_margin = Margin::symmetric(8, 8);
    style.spacing.interact_size = Vec2::new(0.0, 34.0);
    style.spacing.slider_width = 190.0;
    style.spacing.text_edit_width = 260.0;
    style.spacing.combo_width = 170.0;
    style.animation_time = 0.12;
    style.text_styles = [
        (egui::TextStyle::Heading, bold_font(22.0)),
        (egui::TextStyle::Body, FontId::proportional(14.0)),
        (egui::TextStyle::Button, FontId::proportional(14.0)),
        (egui::TextStyle::Small, FontId::proportional(11.5)),
        (egui::TextStyle::Monospace, FontId::monospace(12.0)),
    ]
    .into();
    ctx.set_style_of(theme, style);
    ctx.set_theme(theme);
}

pub fn section_label(ui: &mut egui::Ui, text: &str) {
    let p = palette(ui.visuals().dark_mode);
    ui.add_space(2.0);
    ui.label(
        RichText::new(text.to_uppercase())
            .font(bold_font(10.0))
            .color(p.muted),
    );
    ui.add_space(1.0);
}

fn paint_button(
    ui: &mut egui::Ui,
    text: &str,
    width: f32,
    height: f32,
    fill: Color32,
    hover_fill: Color32,
    text_color: Color32,
    stroke: Stroke,
    font_size: f32,
) -> Response {
    let p = palette(ui.visuals().dark_mode);
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, height), egui::Sense::click());
    let enabled = ui.is_enabled();
    let fill = if !enabled {
        p.surface
    } else if response.hovered() {
        hover_fill
    } else {
        fill
    };
    let stroke = if enabled {
        stroke
    } else {
        Stroke::new(1.0, p.border)
    };
    let text_color = if enabled { text_color } else { p.faint };
    ui.painter().rect(rect, 9, fill, stroke, StrokeKind::Inside);
    let galley = ui
        .painter()
        .layout_no_wrap(text.to_owned(), bold_font(font_size), text_color);
    ui.painter()
        .galley(rect.center() - galley.size() / 2.0, galley, text_color);
    if enabled {
        response.on_hover_cursor(egui::CursorIcon::PointingHand)
    } else {
        response
    }
}

pub fn primary_button(ui: &mut egui::Ui, text: &str, width: f32) -> Response {
    let p = palette(ui.visuals().dark_mode);
    paint_button(
        ui,
        text,
        width,
        40.0,
        p.accent,
        p.accent_hover,
        p.on_accent,
        Stroke::new(0.0, Color32::TRANSPARENT),
        14.5,
    )
}

pub fn danger_button(ui: &mut egui::Ui, text: &str, width: f32) -> Response {
    let p = palette(ui.visuals().dark_mode);
    paint_button(
        ui,
        text,
        width,
        40.0,
        p.danger,
        p.danger_hover,
        Color32::WHITE,
        Stroke::new(0.0, Color32::TRANSPARENT),
        14.5,
    )
}

pub fn secondary_button(ui: &mut egui::Ui, text: &str, width: f32) -> Response {
    let p = palette(ui.visuals().dark_mode);
    paint_button(
        ui,
        text,
        width,
        34.0,
        p.surface,
        p.surface_hover,
        p.text,
        Stroke::new(1.0, p.border),
        12.5,
    )
}

pub fn ghost_button(ui: &mut egui::Ui, text: &str) -> Response {
    let p = palette(ui.visuals().dark_mode);
    let font = FontId::proportional(12.5);
    let measure = ui
        .painter()
        .layout_no_wrap(text.to_owned(), font.clone(), p.muted);
    let (rect, response) = ui.allocate_exact_size(
        Vec2::new(measure.size().x + 18.0, 30.0),
        egui::Sense::click(),
    );
    let hovered = response.hovered();
    let color = if hovered { p.text } else { p.muted };
    if hovered {
        ui.painter().rect_filled(
            rect,
            7,
            tint(p.text, if ui.visuals().dark_mode { 14 } else { 10 }),
        );
    }
    let galley = ui.painter().layout_no_wrap(text.to_owned(), font, color);
    ui.painter()
        .galley(rect.center() - galley.size() / 2.0, galley, color);
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

fn chip_impl(
    ui: &mut egui::Ui,
    label: &str,
    selected: bool,
    enabled: bool,
    width: Option<f32>,
) -> Response {
    let p = palette(ui.visuals().dark_mode);
    let font = FontId::proportional(13.0);
    let measure = ui
        .painter()
        .layout_no_wrap(label.to_owned(), font.clone(), p.text);
    let width = width
        .unwrap_or(measure.size().x + 22.0)
        .max(measure.size().x + 18.0);
    let sense = if enabled {
        egui::Sense::click()
    } else {
        egui::Sense::hover()
    };
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, 31.0), sense);
    let hovered = enabled && response.hovered();
    let (fill, stroke, text_color) = if !enabled {
        (p.surface, Stroke::new(1.0, p.border), p.faint)
    } else if selected {
        (
            p.accent,
            Stroke::new(0.0, Color32::TRANSPARENT),
            p.on_accent,
        )
    } else if hovered {
        (p.surface_hover, Stroke::new(1.0, p.border), p.text)
    } else {
        (p.surface, Stroke::new(1.0, p.border), p.text)
    };
    ui.painter().rect(rect, 8, fill, stroke, StrokeKind::Inside);
    let galley = ui
        .painter()
        .layout_no_wrap(label.to_owned(), font, text_color);
    ui.painter()
        .galley(rect.center() - galley.size() / 2.0, galley, text_color);
    if enabled {
        response.on_hover_cursor(egui::CursorIcon::PointingHand)
    } else {
        response
    }
}

pub fn chip(ui: &mut egui::Ui, label: &str, selected: bool, enabled: bool) -> Response {
    chip_impl(ui, label, selected, enabled, None)
}

pub fn chip_sized(
    ui: &mut egui::Ui,
    label: &str,
    selected: bool,
    enabled: bool,
    width: f32,
) -> Response {
    chip_impl(ui, label, selected, enabled, Some(width))
}

pub fn pill(ui: &mut egui::Ui, text: &str, fg: Color32, bg: Color32) {
    egui::Frame::new()
        .fill(bg)
        .corner_radius(5)
        .inner_margin(Margin::symmetric(7, 3))
        .show(ui, |ui| {
            // A pill is one chip. Never let a narrow row break its label.
            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
            ui.label(RichText::new(text).font(bold_font(10.0)).color(fg));
        });
}

pub fn count_badge(ui: &mut egui::Ui, count: usize) {
    let p = palette(ui.visuals().dark_mode);
    egui::Frame::new()
        .fill(p.surface)
        .stroke(Stroke::new(1.0, p.border))
        .corner_radius(10)
        .inner_margin(Margin::symmetric(8, 2))
        .show(ui, |ui| {
            ui.label(
                RichText::new(count.to_string())
                    .font(bold_font(10.5))
                    .color(p.muted),
            );
        });
}
