//! Visual design for Balance: palette, typography, widget styling and a few
//! shared building blocks (cards, section headings, badges, stat tiles).

use eframe::egui::{
    self, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Margin, RichText,
    Shadow, Stroke, StrokeKind, TextStyle, Theme, Vec2,
};
use std::sync::Arc;

pub const SEMIBOLD: &str = "semibold";
pub const CORNER: u8 = 4;

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
    pub selection: Color32,
    pub on_selection: Color32,
    pub sidebar_bg: Color32,
    pub sidebar_surface: Color32,
    pub sidebar_border: Color32,
    pub sidebar_text: Color32,
    pub sidebar_muted: Color32,
    pub sidebar_hover: Color32,
    pub success: Color32,
    pub success_soft: Color32,
    pub warning: Color32,
    pub warning_soft: Color32,
    pub danger: Color32,
    pub danger_soft: Color32,
    pub info: Color32,
    pub info_soft: Color32,
}

const fn rgb(r: u8, g: u8, b: u8) -> Color32 {
    Color32::from_rgb(r, g, b)
}

pub const LIGHT: Palette = Palette {
    bg: rgb(239, 245, 249),
    surface: rgb(255, 254, 250),
    surface_alt: rgb(233, 242, 249),
    hover: rgb(222, 236, 246),
    pressed: rgb(207, 226, 241),
    border: rgb(188, 205, 219),
    border_strong: rgb(130, 155, 178),
    text: rgb(27, 43, 58),
    text_muted: rgb(60, 79, 96),
    text_faint: rgb(76, 94, 111),
    accent: rgb(35, 91, 145),
    accent_soft: rgb(225, 238, 249),
    on_accent: rgb(255, 255, 255),
    selection: rgb(247, 218, 131),
    on_selection: rgb(39, 43, 40),
    sidebar_bg: rgb(35, 79, 117),
    sidebar_surface: rgb(39, 87, 127),
    sidebar_border: rgb(98, 142, 177),
    sidebar_text: rgb(246, 251, 255),
    sidebar_muted: rgb(211, 231, 246),
    sidebar_hover: rgb(47, 100, 144),
    success: rgb(40, 100, 71),
    success_soft: rgb(232, 241, 233),
    warning: rgb(117, 77, 19),
    warning_soft: rgb(250, 238, 205),
    danger: rgb(159, 43, 45),
    danger_soft: rgb(250, 233, 231),
    info: rgb(36, 86, 126),
    info_soft: rgb(225, 238, 248),
};

pub const DARK: Palette = Palette {
    bg: rgb(22, 31, 42),
    surface: rgb(30, 43, 56),
    surface_alt: rgb(37, 53, 69),
    hover: rgb(45, 65, 85),
    pressed: rgb(52, 77, 101),
    border: rgb(63, 86, 107),
    border_strong: rgb(108, 136, 159),
    text: rgb(236, 243, 248),
    text_muted: rgb(194, 211, 225),
    text_faint: rgb(165, 186, 204),
    accent: rgb(152, 197, 236),
    accent_soft: rgb(38, 67, 93),
    on_accent: rgb(19, 37, 54),
    selection: rgb(221, 192, 102),
    on_selection: rgb(34, 38, 35),
    sidebar_bg: rgb(25, 52, 78),
    sidebar_surface: rgb(31, 61, 89),
    sidebar_border: rgb(78, 113, 145),
    sidebar_text: rgb(236, 246, 253),
    sidebar_muted: rgb(181, 207, 227),
    sidebar_hover: rgb(38, 76, 110),
    success: rgb(153, 216, 178),
    success_soft: rgb(31, 61, 47),
    warning: rgb(244, 213, 139),
    warning_soft: rgb(66, 53, 31),
    danger: rgb(246, 170, 167),
    danger_soft: rgb(68, 39, 41),
    info: rgb(168, 207, 237),
    info_soft: rgb(35, 61, 84),
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
    v.window_corner_radius = CornerRadius::same(CORNER);
    v.menu_corner_radius = CornerRadius::same(CORNER);
    v.window_shadow = Shadow::NONE;
    v.popup_shadow = Shadow::NONE;
    v.hyperlink_color = p.accent;
    v.warn_fg_color = p.warning;
    v.error_fg_color = p.danger;
    v.weak_text_color = Some(p.text_faint);
    let (selection, selection_text) = if dark {
        (rgb(92, 74, 41), p.text)
    } else {
        (p.selection, p.on_selection)
    };
    v.selection.bg_fill = selection;
    v.selection.stroke = Stroke::new(1.5, selection_text);
    v.collapsing_header_frame = true;
    v.striped = false;
    v.slider_trailing_fill = true;

    let radius = CornerRadius::same(CORNER);
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
    w.hovered.bg_stroke = Stroke::new(1.0, p.accent);
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
        .corner_radius(CornerRadius::same(CORNER))
        .inner_margin(Margin::symmetric(22, 18))
}

/// A bordered panel that fills the available width.
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
        .corner_radius(CornerRadius::same(CORNER))
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
        (p.sidebar_muted, p.sidebar_surface, p.sidebar_border)
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
    label: &str,
    value: &str,
    note: &str,
    tone: Tone,
) -> egui::Response {
    let p = ui_palette(ui);
    let note_color = match tone {
        Tone::Danger | Tone::Warning => tone.colors(&p).0,
        _ => p.text_muted,
    };
    let inner = card_frame(&p)
        .inner_margin(Margin::symmetric(18, 16))
        .show(ui, |ui| {
            ui.set_width(width - 38.0);
            ui.label(
                RichText::new(label)
                    .font(semibold(13.0))
                    .color(p.text_muted),
            );
            ui.add_space(4.0);
            ui.label(RichText::new(value).font(semibold(22.0)).color(p.text));
            ui.label(RichText::new(note).size(12.0).color(note_color));
        });
    inner.response
}

/// A full-width, prominent action button.
pub fn primary_button(ui: &mut egui::Ui, text: impl Into<String>) -> egui::Response {
    let p = ui_palette(ui);
    let response = ui.add(
        egui::Button::new(
            RichText::new(text.into())
                .color(p.on_accent)
                .font(semibold(14.0)),
        )
        .fill(p.accent)
        .stroke(Stroke::NONE)
        .corner_radius(CornerRadius::same(CORNER))
        .min_size(Vec2::new(0.0, 32.0)),
    );
    if response.hovered() || response.has_focus() {
        paint_outline(ui, response.rect.shrink(2.0), p.on_accent, CORNER);
    }
    response
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
    use super::{amount, style, Color32, Tone, DARK, LIGHT};

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

    #[test]
    fn text_colors_are_readable_in_both_themes() {
        fn luminance(color: Color32) -> f64 {
            let channel = |value: u8| {
                let encoded = f64::from(value) / 255.0;
                if encoded <= 0.04045 {
                    encoded / 12.92
                } else {
                    ((encoded + 0.055) / 1.055).powf(2.4)
                }
            };
            0.2126 * channel(color.r()) + 0.7152 * channel(color.g()) + 0.0722 * channel(color.b())
        }
        let readable = |foreground, background| {
            let a = luminance(foreground);
            let b = luminance(background);
            assert!(
                (a.max(b) + 0.05) / (a.min(b) + 0.05) >= 4.5,
                "low text contrast for {foreground:?} on {background:?}",
            );
        };
        for (palette, dark) in [(LIGHT, false), (DARK, true)] {
            for background in [palette.bg, palette.surface, palette.surface_alt] {
                readable(palette.text, background);
                readable(palette.text_muted, background);
                readable(palette.text_faint, background);
                readable(palette.danger, background);
                readable(palette.accent, background);
            }
            for background in [
                palette.sidebar_bg,
                palette.sidebar_surface,
                palette.sidebar_hover,
            ] {
                readable(palette.sidebar_text, background);
                readable(palette.sidebar_muted, background);
            }
            for tone in [
                Tone::Neutral,
                Tone::Accent,
                Tone::Success,
                Tone::Warning,
                Tone::Danger,
                Tone::Info,
            ] {
                let (text, background) = tone.colors(&palette);
                readable(text, background);
            }
            readable(palette.on_accent, palette.accent);
            readable(palette.on_selection, palette.selection);
            let visuals = style(&palette, dark).visuals;
            readable(visuals.selection.stroke.color, visuals.selection.bg_fill);
            readable(
                visuals.widgets.inactive.fg_stroke.color,
                visuals.selection.bg_fill,
            );
        }
    }
}
