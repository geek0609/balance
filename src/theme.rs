//! Visual design for Balance: palette, typography, widget styling and a few
//! shared building blocks (cards, section headings, badges, stat tiles).

use eframe::egui::{
    self, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Margin, RichText,
    Shadow, Stroke, StrokeKind, TextStyle, Theme, Vec2,
};
use std::sync::Arc;

/// Error text colour used by older screens; readable on light and dark surfaces.
pub const DANGER: Color32 = Color32::from_rgb(214, 69, 65);

pub const SEMIBOLD: &str = "semibold";

pub fn semibold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(SEMIBOLD.into()))
}

#[derive(Clone, Copy)]
pub struct Palette {
    pub bg: Color32,
    pub surface: Color32,
    pub surface_alt: Color32,
    pub hover: Color32,
    pub pressed: Color32,
    pub border: Color32,
    pub border_strong: Color32,
    pub text: Color32,
    pub text_muted: Color32,
    pub text_faint: Color32,
    pub accent: Color32,
    pub accent_soft: Color32,
    pub on_accent: Color32,
    pub sidebar_bg: Color32,
    pub sidebar_text: Color32,
    pub sidebar_muted: Color32,
    pub sidebar_hover: Color32,
    pub sidebar_active: Color32,
    pub success: Color32,
    pub success_soft: Color32,
    pub warning: Color32,
    pub warning_soft: Color32,
    pub danger: Color32,
    pub danger_soft: Color32,
    pub info: Color32,
    pub info_soft: Color32,
    pub shadow: Color32,
}

const fn rgb(r: u8, g: u8, b: u8) -> Color32 {
    Color32::from_rgb(r, g, b)
}

pub const LIGHT: Palette = Palette {
    bg: rgb(242, 244, 247),
    surface: rgb(255, 255, 255),
    surface_alt: rgb(247, 248, 250),
    hover: rgb(238, 241, 246),
    pressed: rgb(226, 231, 239),
    border: rgb(228, 231, 236),
    border_strong: rgb(205, 210, 219),
    text: rgb(27, 34, 48),
    text_muted: rgb(91, 100, 116),
    text_faint: rgb(138, 147, 163),
    accent: rgb(59, 91, 219),
    accent_soft: rgb(232, 237, 255),
    on_accent: rgb(255, 255, 255),
    sidebar_bg: rgb(17, 24, 39),
    sidebar_text: rgb(209, 213, 219),
    sidebar_muted: rgb(110, 118, 135),
    sidebar_hover: rgb(31, 41, 59),
    sidebar_active: rgb(37, 53, 99),
    success: rgb(18, 128, 92),
    success_soft: rgb(227, 246, 238),
    warning: rgb(180, 83, 9),
    warning_soft: rgb(254, 243, 199),
    danger: rgb(201, 48, 44),
    danger_soft: rgb(253, 236, 236),
    info: rgb(37, 99, 235),
    info_soft: rgb(230, 240, 255),
    shadow: Color32::from_black_alpha(28),
};

pub const DARK: Palette = Palette {
    bg: rgb(13, 17, 23),
    surface: rgb(22, 27, 34),
    surface_alt: rgb(28, 34, 43),
    hover: rgb(34, 41, 52),
    pressed: rgb(44, 52, 65),
    border: rgb(42, 49, 60),
    border_strong: rgb(60, 68, 82),
    text: rgb(230, 233, 239),
    text_muted: rgb(162, 170, 184),
    text_faint: rgb(110, 119, 135),
    accent: rgb(109, 139, 255),
    accent_soft: rgb(30, 42, 79),
    on_accent: rgb(255, 255, 255),
    sidebar_bg: rgb(9, 12, 17),
    sidebar_text: rgb(201, 206, 214),
    sidebar_muted: rgb(107, 114, 128),
    sidebar_hover: rgb(24, 30, 40),
    sidebar_active: rgb(33, 46, 86),
    success: rgb(63, 185, 138),
    success_soft: rgb(16, 41, 31),
    warning: rgb(224, 163, 58),
    warning_soft: rgb(46, 35, 16),
    danger: rgb(240, 106, 106),
    danger_soft: rgb(51, 22, 22),
    info: rgb(91, 155, 255),
    info_soft: rgb(18, 35, 61),
    shadow: Color32::from_black_alpha(90),
};

pub fn palette(ctx: &egui::Context) -> Palette {
    match ctx.theme() {
        Theme::Dark => DARK,
        Theme::Light => LIGHT,
    }
}

pub fn ui_palette(ui: &egui::Ui) -> Palette {
    if ui.visuals().dark_mode {
        DARK
    } else {
        LIGHT
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Neutral,
    Accent,
    Success,
    Warning,
    Danger,
    Info,
}

impl Tone {
    pub fn colors(self, p: &Palette) -> (Color32, Color32) {
        match self {
            Tone::Neutral => (p.text_muted, p.surface_alt),
            Tone::Accent => (p.accent, p.accent_soft),
            Tone::Success => (p.success, p.success_soft),
            Tone::Warning => (p.warning, p.warning_soft),
            Tone::Danger => (p.danger, p.danger_soft),
            Tone::Info => (p.info, p.info_soft),
        }
    }
}

/// Install fonts and the light and dark styles. Call once at start-up.
pub fn install(ctx: &egui::Context) {
    ctx.set_fonts(fonts());
    ctx.set_style_of(Theme::Light, style(&LIGHT, false));
    ctx.set_style_of(Theme::Dark, style(&DARK, true));
}

/// Use the platform UI font where it exists (Segoe UI on Windows) and fall back
/// to egui's bundled fonts, which also provide the icon glyphs.
fn fonts() -> FontDefinitions {
    let mut fonts = FontDefinitions::default();
    let fallback = fonts
        .families
        .get(&FontFamily::Proportional)
        .cloned()
        .unwrap_or_default();
    let mut regular = None;
    let mut strong = None;
    if let Some(dir) = system_font_dir() {
        for (name, files, slot) in [
            ("system-regular", &["segoeui.ttf"][..], &mut regular),
            (
                "system-semibold",
                &["seguisb.ttf", "segoeuib.ttf"][..],
                &mut strong,
            ),
        ] {
            if let Some(bytes) = files.iter().find_map(|f| std::fs::read(dir.join(f)).ok()) {
                fonts
                    .font_data
                    .insert(name.to_owned(), Arc::new(FontData::from_owned(bytes)));
                *slot = Some(name.to_owned());
            }
        }
    }
    let mut proportional = fallback.clone();
    if let Some(name) = &regular {
        proportional.insert(0, name.clone());
    }
    let mut semibold = fallback;
    if let Some(name) = strong.or(regular) {
        semibold.insert(0, name);
    }
    fonts
        .families
        .insert(FontFamily::Proportional, proportional);
    fonts
        .families
        .insert(FontFamily::Name(SEMIBOLD.into()), semibold);
    fonts
}

fn system_font_dir() -> Option<std::path::PathBuf> {
    if cfg!(windows) {
        let root = std::env::var_os("WINDIR").unwrap_or_else(|| "C:\\Windows".into());
        Some(std::path::PathBuf::from(root).join("Fonts"))
    } else {
        None
    }
}

fn style(p: &Palette, dark: bool) -> egui::Style {
    let mut style = egui::Style::default();
    style.text_styles = [
        (TextStyle::Small, FontId::proportional(12.0)),
        (TextStyle::Body, FontId::proportional(14.0)),
        (TextStyle::Button, FontId::proportional(14.0)),
        (TextStyle::Monospace, FontId::monospace(13.0)),
        (TextStyle::Heading, semibold(20.0)),
    ]
    .into();

    let spacing = &mut style.spacing;
    spacing.item_spacing = Vec2::new(8.0, 8.0);
    spacing.button_padding = Vec2::new(12.0, 5.0);
    spacing.interact_size = Vec2::new(40.0, 28.0);
    spacing.combo_width = 200.0;
    spacing.text_edit_width = 240.0;
    spacing.indent = 20.0;
    spacing.window_margin = Margin::same(16);
    spacing.menu_margin = Margin::same(6);
    spacing.icon_width = 16.0;
    spacing.icon_width_inner = 9.0;
    spacing.icon_spacing = 6.0;

    let mut v = if dark {
        egui::Visuals::dark()
    } else {
        egui::Visuals::light()
    };
    v.panel_fill = p.bg;
    v.window_fill = p.surface;
    v.extreme_bg_color = p.surface;
    v.text_edit_bg_color = Some(p.surface);
    v.faint_bg_color = p.surface_alt;
    v.code_bg_color = p.surface_alt;
    v.window_stroke = Stroke::new(1.0, p.border);
    v.window_corner_radius = CornerRadius::same(12);
    v.menu_corner_radius = CornerRadius::same(10);
    v.window_shadow = Shadow {
        offset: [0, 10],
        blur: 30,
        spread: 0,
        color: p.shadow,
    };
    v.popup_shadow = Shadow {
        offset: [0, 6],
        blur: 18,
        spread: 0,
        color: p.shadow,
    };
    v.hyperlink_color = p.accent;
    v.warn_fg_color = p.warning;
    v.error_fg_color = p.danger;
    v.selection.bg_fill = p.accent_soft;
    v.selection.stroke = Stroke::new(1.5, p.accent);
    v.collapsing_header_frame = true;
    v.striped = false;
    v.slider_trailing_fill = true;

    let radius = CornerRadius::same(7);
    let w = &mut v.widgets;
    w.noninteractive.bg_fill = p.surface;
    w.noninteractive.weak_bg_fill = p.surface;
    w.noninteractive.bg_stroke = Stroke::new(1.0, p.border);
    w.noninteractive.fg_stroke = Stroke::new(1.0, p.text);
    w.noninteractive.corner_radius = radius;

    w.inactive.bg_fill = p.surface_alt;
    w.inactive.weak_bg_fill = p.surface;
    w.inactive.bg_stroke = Stroke::new(1.0, p.border_strong);
    w.inactive.fg_stroke = Stroke::new(1.0, p.text);
    w.inactive.corner_radius = radius;
    w.inactive.expansion = 0.0;

    w.hovered.bg_fill = p.hover;
    w.hovered.weak_bg_fill = p.hover;
    w.hovered.bg_stroke = Stroke::new(1.0, p.accent.gamma_multiply(0.7));
    w.hovered.fg_stroke = Stroke::new(1.5, p.text);
    w.hovered.corner_radius = radius;
    w.hovered.expansion = 0.0;

    w.active.bg_fill = p.pressed;
    w.active.weak_bg_fill = p.pressed;
    w.active.bg_stroke = Stroke::new(1.0, p.accent);
    w.active.fg_stroke = Stroke::new(1.5, p.text);
    w.active.corner_radius = radius;
    w.active.expansion = 0.0;

    w.open.bg_fill = p.surface_alt;
    w.open.weak_bg_fill = p.surface_alt;
    w.open.bg_stroke = Stroke::new(1.0, p.accent);
    w.open.fg_stroke = Stroke::new(1.0, p.text);
    w.open.corner_radius = radius;

    style.visuals = v;
    style
}

const PAGE_TITLE: &str = "balance-page-title";

/// Record the title shown in the page header, so a page's own first heading
/// with the same text is not repeated directly underneath it.
pub fn set_page_title(ctx: &egui::Context, title: &str) {
    ctx.data_mut(|d| d.insert_temp(egui::Id::new(PAGE_TITLE), title.to_lowercase()));
}

/// A section heading within a page.
pub fn section(ui: &mut egui::Ui, text: impl Into<RichText>) -> egui::Response {
    let text = text.into().text().to_owned();
    let title: Option<String> = ui.ctx().data(|d| d.get_temp(egui::Id::new(PAGE_TITLE)));
    if title.as_deref() == Some(text.to_lowercase().as_str()) {
        return ui.allocate_response(Vec2::ZERO, egui::Sense::hover());
    }
    let p = ui_palette(ui);
    let first = ui.min_rect().height() < 4.0;
    ui.add_space(if first { 0.0 } else { 14.0 });
    let response = ui
        .horizontal(|ui| {
            let (bar, _) = ui.allocate_exact_size(Vec2::new(3.0, 18.0), egui::Sense::hover());
            ui.painter()
                .rect_filled(bar, CornerRadius::same(2), p.accent);
            ui.add_space(2.0);
            ui.label(RichText::new(text).font(semibold(16.5)).color(p.text))
        })
        .inner;
    ui.add_space(2.0);
    response
}

pub fn card_frame(p: &Palette) -> egui::Frame {
    egui::Frame::new()
        .fill(p.surface)
        .stroke(Stroke::new(1.0, p.border))
        .corner_radius(CornerRadius::same(12))
        .inner_margin(Margin::symmetric(22, 18))
        .shadow(Shadow {
            offset: [0, 1],
            blur: 3,
            spread: 0,
            color: Color32::from_black_alpha(8),
        })
}

/// A white card that fills the available width.
pub fn card<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let p = ui_palette(ui);
    card_frame(&p)
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            add(ui)
        })
        .inner
}

/// A small heading used inside cards.
pub fn card_title(ui: &mut egui::Ui, title: &str, subtitle: Option<&str>) {
    let p = ui_palette(ui);
    ui.label(RichText::new(title).font(semibold(15.5)).color(p.text));
    if let Some(subtitle) = subtitle {
        ui.label(RichText::new(subtitle).size(12.5).color(p.text_muted));
    }
    ui.add_space(6.0);
}

/// Uppercase caption text.
pub fn caption(text: &str, color: Color32) -> RichText {
    RichText::new(text.to_uppercase())
        .font(semibold(10.5))
        .color(color)
        .extra_letter_spacing(0.8)
}

pub fn badge(ui: &mut egui::Ui, text: &str, tone: Tone) -> egui::Response {
    let p = ui_palette(ui);
    let (fg, bg) = tone.colors(&p);
    egui::Frame::new()
        .fill(bg)
        .corner_radius(CornerRadius::same(255))
        .inner_margin(Margin::symmetric(8, 2))
        .show(ui, |ui| {
            ui.label(RichText::new(text).font(semibold(11.5)).color(fg));
        })
        .response
}

/// A keyboard key cap, e.g. "Ctrl K".
pub fn kbd(ui: &mut egui::Ui, keys: &str, dark_surface: bool) {
    let p = ui_palette(ui);
    let (fg, bg, stroke) = if dark_surface {
        (
            p.sidebar_muted,
            Color32::from_white_alpha(10),
            Color32::from_white_alpha(24),
        )
    } else {
        (p.text_muted, p.surface_alt, p.border_strong)
    };
    egui::Frame::new()
        .fill(bg)
        .stroke(Stroke::new(1.0, stroke))
        .corner_radius(CornerRadius::same(4))
        .inner_margin(Margin::symmetric(5, 0))
        .show(ui, |ui| {
            ui.label(RichText::new(keys).size(11.0).color(fg));
        });
}

/// A headline number with a caption and an optional footnote.
pub fn stat_tile(
    ui: &mut egui::Ui,
    width: f32,
    icon: &str,
    label: &str,
    value: &str,
    note: &str,
    tone: Tone,
) -> egui::Response {
    let p = ui_palette(ui);
    let (fg, bg) = tone.colors(&p);
    let inner = card_frame(&p)
        .inner_margin(Margin::symmetric(18, 16))
        .show(ui, |ui| {
            ui.set_width(width - 38.0);
            ui.horizontal(|ui| {
                egui::Frame::new()
                    .fill(bg)
                    .corner_radius(CornerRadius::same(8))
                    .inner_margin(Margin::symmetric(7, 4))
                    .show(ui, |ui| {
                        ui.label(RichText::new(icon).size(14.0).color(fg));
                    });
                ui.label(caption(label, p.text_muted));
            });
            ui.add_space(4.0);
            ui.label(RichText::new(value).font(semibold(22.0)).color(p.text));
            ui.label(RichText::new(note).size(12.0).color(p.text_muted));
        });
    inner.response
}

/// A full-width, prominent action button.
pub fn primary_button(ui: &mut egui::Ui, text: impl Into<String>) -> egui::Response {
    let p = ui_palette(ui);
    ui.add(
        egui::Button::new(
            RichText::new(text.into())
                .color(p.on_accent)
                .font(semibold(14.0)),
        )
        .fill(p.accent)
        .stroke(Stroke::NONE)
        .corner_radius(CornerRadius::same(8))
        .min_size(Vec2::new(0.0, 32.0)),
    )
}

/// A quiet, borderless button.
pub fn ghost_button(ui: &mut egui::Ui, text: impl Into<RichText>) -> egui::Response {
    ui.add(egui::Button::new(text.into()).frame_when_inactive(false))
}

pub fn paint_outline(ui: &egui::Ui, rect: egui::Rect, color: Color32, radius: u8) {
    ui.painter().rect_stroke(
        rect,
        CornerRadius::same(radius),
        Stroke::new(1.0, color),
        StrokeKind::Inside,
    );
}

/// Format hundredths with digit grouping. Indian rupee amounts use lakh/crore
/// grouping; everything else uses groups of three.
pub fn amount(value: i128, currency: Option<&str>) -> String {
    let negative = value < 0;
    let n = value.unsigned_abs();
    let whole = (n / 100).to_string();
    let cents = n % 100;
    let indian = currency.is_some_and(|c| c.eq_ignore_ascii_case("INR"));
    let mut groups: Vec<&str> = Vec::new();
    let mut rest = whole.as_str();
    if rest.len() > 3 {
        let (head, tail) = rest.split_at(rest.len() - 3);
        groups.push(tail);
        rest = head;
        let size = if indian { 2 } else { 3 };
        while rest.len() > size {
            let (head, tail) = rest.split_at(rest.len() - size);
            groups.push(tail);
            rest = head;
        }
    }
    groups.push(rest);
    groups.reverse();
    format!(
        "{}{}.{:02}",
        if negative { "-" } else { "" },
        groups.join(","),
        cents
    )
}

pub fn currency_symbol(code: Option<&str>) -> &'static str {
    match code.map(|c| c.to_ascii_uppercase()).as_deref() {
        Some("INR") => "₹",
        Some("USD") => "$",
        Some("EUR") => "€",
        Some("GBP") => "£",
        Some("JPY") => "¥",
        Some("AED") => "AED ",
        _ => "",
    }
}

#[cfg(test)]
mod tests {
    use super::amount;

    #[test]
    fn groups_amounts() {
        assert_eq!(amount(0, None), "0.00");
        assert_eq!(amount(-5, None), "-0.05");
        assert_eq!(amount(123_456, None), "1,234.56");
        assert_eq!(amount(123_456_789_00, None), "123,456,789.00");
        assert_eq!(amount(123_456_789_00, Some("INR")), "12,34,56,789.00");
        assert_eq!(amount(-100_000_00, Some("inr")), "-1,00,000.00");
        assert_eq!(amount(99_900, Some("INR")), "999.00");
    }
}
