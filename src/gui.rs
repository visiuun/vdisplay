//! Desktop controller UI (egui): OLED-black theme, custom widgets, live panel preview, tray.

use crate::loader;
use crate::lyrics;
use crate::render::LyricsUi;
use crate::sensors::{self, SensorState};
use crate::settings::{BgStyle, IdleMode, MediaFit, Mode};
use crate::shared::{LinkState, Shared};
use crate::winutil;
use eframe::egui::{
    self, Align, Align2, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId,
    Layout, Margin, Pos2, Rect, Response, Sense, Stroke, StrokeKind, TextureHandle, TextureOptions,
    Ui, Vec2, ViewportCommand,
};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::time::{Duration, Instant};
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem};
use tray_icon::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

// Design tokens

const BG: Color32 = Color32::from_rgb(0, 0, 0);
const CARD: Color32 = Color32::from_rgb(9, 9, 11);
const CARD_HI: Color32 = Color32::from_rgb(14, 14, 17);
const WELL: Color32 = Color32::from_rgb(17, 17, 21);
const WELL_HOVER: Color32 = Color32::from_rgb(25, 25, 31);
const SEL: Color32 = Color32::from_rgb(34, 34, 41);
const BORDER: Color32 = Color32::from_rgb(26, 26, 31);
const BORDER_HI: Color32 = Color32::from_rgb(40, 40, 48);
const TEXT: Color32 = Color32::from_rgb(244, 244, 245);
const MUTED: Color32 = Color32::from_rgb(161, 161, 170);
const DIM: Color32 = Color32::from_rgb(113, 113, 122);
const FAINT: Color32 = Color32::from_rgb(63, 63, 70);
const ACCENT: Color32 = Color32::from_rgb(255, 45, 70);
const ACCENT_HOVER: Color32 = Color32::from_rgb(255, 77, 98);
const GREEN: Color32 = Color32::from_rgb(34, 197, 94);
const AMBER: Color32 = Color32::from_rgb(234, 179, 8);
const BLUE: Color32 = Color32::from_rgb(56, 189, 248);
const PURPLE: Color32 = Color32::from_rgb(168, 85, 247);

// Segoe Fluent Icons / MDL2 Assets code points.
const I_MUSIC: &str = "\u{E8D6}";
const I_HUD: &str = "\u{E9D9}";
const I_PHOTO: &str = "\u{E91B}";
const I_PLAY: &str = "\u{E768}";
const I_STOP: &str = "\u{E71A}";
const I_REFRESH: &str = "\u{E72C}";
const I_FOLDER: &str = "\u{E838}";
const I_COLOR: &str = "\u{E790}";
const I_SETTINGS: &str = "\u{E713}";
const I_MONITOR: &str = "\u{E7F4}";
const I_MINUS: &str = "\u{E738}";
const I_PLUS: &str = "\u{E710}";
const I_RESET: &str = "\u{E777}";
const I_PLUG: &str = "\u{E88E}";

fn semi(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("semi".into()))
}
fn bold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("bold".into()))
}
fn reg(size: f32) -> FontId {
    FontId::new(size, FontFamily::Proportional)
}
fn icon(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("icons".into()))
}
fn mono(size: f32) -> FontId {
    FontId::new(size, FontFamily::Monospace)
}

fn install_fonts(ctx: &egui::Context) {
    let dir = r"C:\Windows\Fonts";
    let read = |n: &str| std::fs::read(format!(r"{dir}\{n}")).ok();
    let mut defs = FontDefinitions::default();
    let add = |defs: &mut FontDefinitions, key: &str, names: &[&str]| -> bool {
        for n in names {
            if let Some(b) = read(n) {
                defs.font_data
                    .insert(key.into(), Arc::new(FontData::from_owned(b)));
                return true;
            }
        }
        false
    };
    let has_reg = add(&mut defs, "reg", &["segoeui.ttf"]);
    let has_semi = add(&mut defs, "semi", &["seguisb.ttf", "segoeuib.ttf"]);
    let has_bold = add(&mut defs, "bold", &["segoeuib.ttf"]);
    let has_icons = add(&mut defs, "icons", &["SegoeIcons.ttf", "segmdl2.ttf"]);
    let has_sym = add(&mut defs, "sym", &["seguisym.ttf"]);
    let has_mono = add(&mut defs, "mono", &["CascadiaMono.ttf", "consola.ttf"]);
    let has_cjk = add(&mut defs, "cjk", &["YuGothM.ttc", "msyh.ttc", "malgun.ttf"]);

    let chain = |primary: Option<&str>| -> Vec<String> {
        let mut v = Vec::new();
        if let Some(p) = primary {
            v.push(p.to_string());
        }
        if has_reg {
            v.push("reg".into());
        }
        if has_sym {
            v.push("sym".into());
        }
        if has_cjk {
            v.push("cjk".into());
        }
        v
    };
    let prop = chain(None);
    let semi_chain = chain(has_semi.then_some("semi"));
    let bold_chain = chain(has_bold.then_some("bold"));
    let mut mono_chain = chain(has_mono.then_some("mono"));
    for fam in [FontFamily::Proportional, FontFamily::Monospace] {
        let e = defs.families.entry(fam.clone()).or_default();
        let base = if fam == FontFamily::Monospace {
            &mut mono_chain
        } else {
            &mut prop.clone()
        };
        let mut v = base.clone();
        v.append(e);
        *e = v;
    }
    let egui_default = defs
        .families
        .get(&FontFamily::Proportional)
        .cloned()
        .unwrap_or_default();
    let with_default = |mut v: Vec<String>| {
        for d in &egui_default {
            if !v.contains(d) {
                v.push(d.clone());
            }
        }
        v
    };
    defs.families
        .insert(FontFamily::Name("semi".into()), with_default(semi_chain));
    defs.families
        .insert(FontFamily::Name("bold".into()), with_default(bold_chain));
    let mut icons = Vec::new();
    if has_icons {
        icons.push("icons".to_string());
    }
    defs.families
        .insert(FontFamily::Name("icons".into()), with_default(icons));
    ctx.set_fonts(defs);
}

fn install_style(ctx: &egui::Context) {
    ctx.set_theme(egui::ThemePreference::Dark);
    ctx.all_styles_mut(|s| {
        let v = &mut s.visuals;
        v.dark_mode = true;
        v.panel_fill = BG;
        v.window_fill = CARD;
        v.window_stroke = Stroke::new(1.0, BORDER);
        v.extreme_bg_color = WELL;
        v.faint_bg_color = CARD_HI;
        v.override_text_color = Some(TEXT);
        v.selection.bg_fill = ACCENT.gamma_multiply(0.45);
        v.selection.stroke = Stroke::new(1.0, ACCENT);
        v.hyperlink_color = BLUE;
        v.window_corner_radius = CornerRadius::same(14);
        for w in [
            &mut v.widgets.noninteractive,
            &mut v.widgets.inactive,
            &mut v.widgets.hovered,
            &mut v.widgets.active,
            &mut v.widgets.open,
        ] {
            w.corner_radius = CornerRadius::same(8);
        }
        v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, BORDER);
        v.widgets.inactive.bg_fill = WELL;
        v.widgets.inactive.weak_bg_fill = WELL;
        v.widgets.inactive.bg_stroke = Stroke::new(1.0, BORDER_HI);
        v.widgets.hovered.bg_fill = WELL_HOVER;
        v.widgets.hovered.weak_bg_fill = WELL_HOVER;
        v.widgets.hovered.bg_stroke = Stroke::new(1.0, BORDER_HI);
        v.widgets.active.bg_fill = SEL;
        v.widgets.active.weak_bg_fill = SEL;
        s.spacing.item_spacing = Vec2::new(8.0, 8.0);
        s.spacing.button_padding = Vec2::new(12.0, 6.0);
        s.spacing.interact_size.y = 30.0;
        s.spacing.scroll.bar_width = 6.0;
        s.spacing.scroll.floating = true;
    });
}

// Widgets

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Primary,
    Danger,
    Secondary,
    Ghost,
}

fn text_w(ui: &Ui, s: &str, f: FontId) -> f32 {
    ui.painter().layout_no_wrap(s.to_string(), f, TEXT).size().x
}

/// Button with optional icon glyph. `w` = None to fit the content.
fn button(ui: &mut Ui, ic: &str, label: &str, kind: Kind, w: Option<f32>, h: f32) -> Response {
    let font = semi(13.0);
    let iw = if ic.is_empty() { 0.0 } else { 14.0 + 8.0 };
    let cw = text_w(ui, label, font.clone()) + iw + 28.0;
    let (rect, resp) = ui.allocate_exact_size(
        Vec2::new(w.unwrap_or(cw).max(cw.min(w.unwrap_or(cw))), h),
        Sense::click(),
    );
    let hov = resp.hovered();
    let down = resp.is_pointer_button_down_on();
    let (fill, stroke, fg) = match kind {
        Kind::Primary => (
            if hov { ACCENT_HOVER } else { ACCENT },
            Color32::TRANSPARENT,
            Color32::WHITE,
        ),
        Kind::Danger => (
            if hov {
                Color32::from_rgb(46, 14, 20)
            } else {
                Color32::from_rgb(30, 10, 14)
            },
            Color32::from_rgb(80, 22, 32),
            ACCENT,
        ),
        Kind::Secondary => (if hov { WELL_HOVER } else { WELL }, BORDER_HI, TEXT),
        Kind::Ghost => (
            if hov { WELL } else { Color32::TRANSPARENT },
            if hov { BORDER_HI } else { BORDER },
            MUTED,
        ),
    };
    let rect = if down { rect.shrink(0.5) } else { rect };
    ui.painter().rect(
        rect,
        CornerRadius::same(9),
        fill,
        Stroke::new(1.0, stroke),
        StrokeKind::Inside,
    );
    if label.is_empty() {
        // Icon-only: centre the glyph itself (no space reserved for a label).
        ui.painter()
            .text(rect.center(), Align2::CENTER_CENTER, ic, icon(15.0), fg);
        return resp.on_hover_cursor(egui::CursorIcon::PointingHand);
    }
    let total = iw + text_w(ui, label, font.clone());
    let mut x = rect.center().x - total / 2.0;
    if !ic.is_empty() {
        ui.painter().text(
            Pos2::new(x + 7.0, rect.center().y),
            Align2::CENTER_CENTER,
            ic,
            icon(14.0),
            fg,
        );
        x += iw;
    }
    ui.painter().text(
        Pos2::new(x, rect.center().y),
        Align2::LEFT_CENTER,
        label,
        font,
        fg,
    );
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// Pill-shaped segmented control. Returns true if the value changed.
fn segmented<T: PartialEq + Copy>(
    ui: &mut Ui,
    value: &mut T,
    opts: &[(T, &str)],
    width: f32,
) -> bool {
    let h = 32.0;
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, h), Sense::hover());
    ui.painter().rect(
        rect,
        CornerRadius::same(10),
        WELL,
        Stroke::new(1.0, BORDER),
        StrokeKind::Inside,
    );
    let inner = rect.shrink(3.0);
    let seg_w = inner.width() / opts.len() as f32;
    let mut changed = false;
    for (i, (v, label)) in opts.iter().enumerate() {
        let r = Rect::from_min_size(
            Pos2::new(inner.min.x + seg_w * i as f32, inner.min.y),
            Vec2::new(seg_w, inner.height()),
        );
        let resp = ui.interact(
            r,
            ui.make_persistent_id(("seg", rect.min.x as i32, rect.min.y as i32, i)),
            Sense::click(),
        );
        let sel = *v == *value;
        if sel {
            ui.painter().rect(
                r,
                CornerRadius::same(8),
                SEL,
                Stroke::new(1.0, BORDER_HI),
                StrokeKind::Inside,
            );
        } else if resp.hovered() {
            ui.painter()
                .rect_filled(r, CornerRadius::same(8), WELL_HOVER);
        }
        let col = if sel {
            TEXT
        } else if resp.hovered() {
            TEXT.gamma_multiply(0.85)
        } else {
            DIM
        };
        ui.painter()
            .text(r.center(), Align2::CENTER_CENTER, *label, semi(12.5), col);
        if resp
            .on_hover_cursor(egui::CursorIcon::PointingHand)
            .clicked()
            && !sel
        {
            *value = *v;
            changed = true;
        }
    }
    changed
}

fn toggle(ui: &mut Ui, on: &mut bool) -> Response {
    let (rect, mut resp) = ui.allocate_exact_size(Vec2::new(40.0, 22.0), Sense::click());
    if resp.clicked() {
        *on = !*on;
        resp.mark_changed();
    }
    let t = ui.ctx().animate_bool_responsive(resp.id, *on);
    let fill = lerp_color(WELL_HOVER, ACCENT, t);
    let stroke = lerp_color(BORDER_HI, ACCENT, t);
    ui.painter().rect(
        rect,
        CornerRadius::same(11),
        fill,
        Stroke::new(1.0, stroke),
        StrokeKind::Inside,
    );
    let x = egui::lerp((rect.left() + 11.0)..=(rect.right() - 11.0), t);
    ui.painter().circle_filled(
        Pos2::new(x, rect.center().y),
        8.0,
        if *on { Color32::WHITE } else { MUTED },
    );
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

fn lerp_color(a: Color32, b: Color32, t: f32) -> Color32 {
    let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgb(l(a.r(), b.r()), l(a.g(), b.g()), l(a.b(), b.b()))
}

/// Flat slider with label + value readout in one compact block; double-click resets.
#[allow(clippy::too_many_arguments)]
fn slider(
    ui: &mut Ui,
    label: &str,
    v: &mut i32,
    min: i32,
    max: i32,
    default: i32,
    suffix: &str,
    col: Color32,
) -> bool {
    let w = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(w, 46.0), Sense::click_and_drag());
    let p = ui.painter();
    p.text(
        rect.left_top() + Vec2::new(0.0, 9.0),
        Align2::LEFT_CENTER,
        label,
        semi(13.0),
        TEXT,
    );
    p.text(
        rect.right_top() + Vec2::new(0.0, 9.0),
        Align2::RIGHT_CENTER,
        format!("{v}{suffix}"),
        mono(12.0),
        col,
    );
    let rail = Rect::from_min_max(
        Pos2::new(rect.left() + 9.0, rect.bottom() - 15.0),
        Pos2::new(rect.right() - 9.0, rect.bottom() - 9.0),
    );
    let mut changed = false;
    if resp.double_clicked() {
        *v = default;
        changed = true;
    } else if let Some(pt) = resp
        .interact_pointer_pos()
        .filter(|_| resp.dragged() || resp.is_pointer_button_down_on())
    {
        let t = ((pt.x - rail.left()) / rail.width()).clamp(0.0, 1.0);
        let nv = (min as f32 + t * (max - min) as f32).round() as i32;
        if nv != *v {
            *v = nv;
            changed = true;
        }
    }
    let t = (*v - min) as f32 / (max - min) as f32;
    let p = ui.painter();
    p.rect_filled(rail, CornerRadius::same(3), WELL_HOVER);
    // Fill from the default value so +/- offsets read naturally (hue around 0, etc.).
    let d = (default - min) as f32 / (max - min) as f32;
    let (a, b) = if t >= d { (d, t) } else { (t, d) };
    let fill = Rect::from_min_max(
        Pos2::new(rail.left() + a * rail.width(), rail.top()),
        Pos2::new(rail.left() + b * rail.width(), rail.bottom()),
    );
    p.rect_filled(fill, CornerRadius::same(3), col);
    let kx = rail.left() + t * rail.width();
    let hov = resp.hovered() || resp.dragged();
    p.circle_filled(
        Pos2::new(kx, rail.center().y),
        if hov { 9.0 } else { 8.0 },
        Color32::WHITE,
    );
    p.circle_filled(Pos2::new(kx, rail.center().y), 3.0, col);
    resp.on_hover_cursor(egui::CursorIcon::ResizeHorizontal)
        .on_hover_text("Drag to adjust · double-click to reset");
    changed
}

/// A card with a small uppercase header.
fn card<R>(ui: &mut Ui, ic: &str, title: &str, add: impl FnOnce(&mut Ui) -> R) -> R {
    egui::Frame::new()
        .fill(CARD)
        .stroke(Stroke::new(1.0, BORDER))
        .corner_radius(CornerRadius::same(16))
        .inner_margin(Margin::same(16))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 8.0;
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(ic).font(icon(13.0)).color(ACCENT));
                ui.add_space(2.0);
                ui.label(
                    egui::RichText::new(title.to_uppercase())
                        .font(semi(11.5))
                        .color(MUTED)
                        .extra_letter_spacing(1.2),
                );
            });
            ui.add_space(6.0);
            add(ui)
        })
        .inner
}

/// Label + optional hint on the left, a control on the right. Height follows the content.
fn row(ui: &mut Ui, label: &str, hint: &str, ctrl_w: f32, ctrl: impl FnOnce(&mut Ui)) {
    let w = ui.available_width();
    ui.horizontal(|ui| {
        ui.vertical(|ui| {
            ui.set_width((w - ctrl_w - 16.0).max(80.0));
            ui.spacing_mut().item_spacing.y = 1.0;
            ui.label(egui::RichText::new(label).font(semi(13.0)).color(TEXT));
            if !hint.is_empty() {
                ui.label(egui::RichText::new(hint).font(reg(11.5)).color(DIM));
            }
        });
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ctrl(ui);
        });
    });
}

fn mode_tile(ui: &mut Ui, w: f32, selected: bool, ic: &str, title: &str, sub: &str) -> Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(w, 92.0), Sense::click());
    let hov = resp.hovered();
    let t = ui.ctx().animate_bool_responsive(resp.id, selected);
    let fill = lerp_color(
        if hov { WELL_HOVER } else { WELL },
        Color32::from_rgb(28, 10, 14),
        t,
    );
    let stroke = lerp_color(if hov { BORDER_HI } else { BORDER }, ACCENT, t);
    ui.painter().rect(
        rect,
        CornerRadius::same(12),
        fill,
        Stroke::new(if selected { 1.5 } else { 1.0 }, stroke),
        StrokeKind::Inside,
    );
    let p = rect.min + Vec2::new(14.0, 14.0);
    ui.painter().text(
        p + Vec2::new(0.0, 10.0),
        Align2::LEFT_CENTER,
        ic,
        icon(18.0),
        lerp_color(MUTED, ACCENT, t),
    );
    ui.painter().text(
        p + Vec2::new(0.0, 40.0),
        Align2::LEFT_CENTER,
        title,
        semi(14.0),
        TEXT,
    );
    ui.painter().text(
        p + Vec2::new(0.0, 60.0),
        Align2::LEFT_CENTER,
        sub,
        reg(11.5),
        DIM,
    );
    if selected {
        ui.painter().circle_filled(
            Pos2::new(rect.right() - 14.0, rect.top() + 14.0),
            3.5,
            ACCENT,
        );
    }
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

fn chip(ui: &mut Ui, dot: Color32, text: &str, pulse: bool) {
    let font = semi(12.0);
    let w = text_w(ui, text, font.clone()) + 34.0;
    let (rect, _) = ui.allocate_exact_size(Vec2::new(w, 28.0), Sense::hover());
    ui.painter().rect(
        rect,
        CornerRadius::same(14),
        CARD_HI,
        Stroke::new(1.0, BORDER),
        StrokeKind::Inside,
    );
    let c = Pos2::new(rect.left() + 15.0, rect.center().y);
    if pulse {
        let t = ui.input(|i| i.time) as f32;
        let a = (t * 2.4).sin() * 0.5 + 0.5;
        ui.painter().circle_filled(
            c,
            4.0 + a * 3.0,
            dot.gamma_multiply(0.25 * (1.0 - a) + 0.05),
        );
        ui.ctx().request_repaint_after(Duration::from_millis(33));
    }
    ui.painter().circle_filled(c, 4.0, dot);
    ui.painter().text(
        Pos2::new(rect.left() + 26.0, rect.center().y),
        Align2::LEFT_CENTER,
        text,
        font,
        TEXT,
    );
}

fn metric(ui: &mut Ui, w: f32, label: &str, value: String, col: Color32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(w, 52.0), Sense::hover());
    ui.painter().rect(
        rect,
        CornerRadius::same(10),
        WELL,
        Stroke::new(1.0, BORDER),
        StrokeKind::Inside,
    );
    ui.painter().text(
        rect.min + Vec2::new(12.0, 15.0),
        Align2::LEFT_CENTER,
        label.to_uppercase(),
        semi(10.0),
        DIM,
    );
    ui.painter().text(
        rect.min + Vec2::new(12.0, 35.0),
        Align2::LEFT_CENTER,
        value,
        mono(13.5),
        col,
    );
}

// App

#[derive(Clone, Default)]
struct SensorUi {
    state: SensorState,
    source: &'static str,
    busy: bool,
    checking: bool,
    checked_at: Option<Instant>,
    message: Option<String>,
}

pub enum GuiCmd {
    Show,
    Menu(MenuId),
}

struct TrayItems {
    _tray: TrayIcon,
    open: MenuItem,
    stream: MenuItem,
    radiant: CheckMenuItem,
    lyrics: CheckMenuItem,
    debug: CheckMenuItem,
    fullscreen: CheckMenuItem,
    color: MenuItem,
    hide: MenuItem,
    exit: MenuItem,
}

pub struct App {
    sh: Arc<Shared>,
    rx: Receiver<GuiCmd>,
    tray: Option<TrayItems>,
    engine: Option<std::thread::JoinHandle<()>>,
    visible: bool,
    allow_close: bool,
    preview_tex: Option<TextureHandle>,
    preview_seq: u64,
    art_tex: Option<(u64, TextureHandle)>,
    logo_tex: TextureHandle,
    run_at_startup: bool,
    loading: Arc<AtomicBool>,
    sensor_ui: Arc<parking_lot::Mutex<SensorUi>>,
    flash_color: Option<Instant>,
    scroll_to_color: bool,
    toast: Option<(String, Instant)>,
    /// Radiant Lyrics key editor (token id, token) while it's open.
    rl_edit: Option<(String, String)>,
    /// Start-in-tray: frames left during which we keep re-hiding (eframe force-shows the
    /// window after its first paint), and the cloaked HWND to reveal afterwards.
    pending_hide: u8,
    cloaked_hwnd: Option<isize>,
}

pub fn tray_icon_image() -> tray_icon::Icon {
    let px = crate::render::icons::logo_rgba(32);
    tray_icon::Icon::from_rgba(px, 32, 32).expect("tray icon")
}

impl App {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        sh: Arc<Shared>,
        engine: std::thread::JoinHandle<()>,
        start_visible: bool,
        tx: Sender<GuiCmd>,
        rx: Receiver<GuiCmd>,
        instance: Option<Arc<winutil::SingleInstance>>,
    ) -> Self {
        let ctx = cc.egui_ctx.clone();
        install_fonts(&ctx);
        install_style(&ctx);
        *sh.egui_ctx.lock() = Some(ctx.clone());

        // Tray + event forwarding. Handlers run on the UI thread's message pump, so they just
        // forward to `logic()` (which eframe runs even while the window is hidden).
        let tray = Self::build_tray(&sh);
        {
            let (tx, c) = (tx.clone(), ctx.clone());
            MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
                let _ = tx.send(GuiCmd::Menu(e.id));
                c.request_repaint();
            }));
        }
        {
            let (tx, c) = (tx.clone(), ctx.clone());
            TrayIconEvent::set_event_handler(Some(move |e: TrayIconEvent| {
                let show = matches!(
                    e,
                    TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } | TrayIconEvent::DoubleClick {
                        button: MouseButton::Left,
                        ..
                    }
                );
                if show {
                    let _ = tx.send(GuiCmd::Show);
                    c.request_repaint();
                }
            }));
        }
        if let Some(inst) = instance {
            let (tx, c) = (tx, ctx.clone());
            std::thread::Builder::new()
                .name("instance-listener".into())
                .spawn(move || {
                    loop {
                        if inst.wait_show_request() {
                            let _ = tx.send(GuiCmd::Show);
                            c.request_repaint();
                        }
                    }
                })
                .ok();
        }

        let mut cloaked_hwnd = None;
        if !start_visible {
            use raw_window_handle::{HasWindowHandle, RawWindowHandle};
            if let Ok(h) = cc.window_handle()
                && let RawWindowHandle::Win32(w) = h.as_raw()
            {
                winutil::set_cloaked(w.hwnd.get(), true);
                cloaked_hwnd = Some(w.hwnd.get());
            }
        }

        let logo = crate::render::icons::logo_rgba(64);
        let logo_tex = ctx.load_texture(
            "logo",
            egui::ColorImage::from_rgba_unmultiplied([64, 64], &logo),
            TextureOptions::LINEAR,
        );
        sh.gui_visible.store(start_visible, Ordering::Relaxed);

        Self {
            sh,
            rx,
            tray,
            engine: Some(engine),
            visible: start_visible,
            allow_close: false,
            preview_tex: None,
            preview_seq: 0,
            art_tex: None,
            logo_tex,
            run_at_startup: winutil::run_at_startup_enabled(),
            loading: Arc::new(AtomicBool::new(false)),
            sensor_ui: Arc::new(parking_lot::Mutex::new(SensorUi::default())),
            flash_color: None,
            scroll_to_color: false,
            toast: None,
            rl_edit: None,
            pending_hide: if start_visible { 0 } else { 3 },
            cloaked_hwnd,
        }
    }

    fn build_tray(sh: &Shared) -> Option<TrayItems> {
        let s = sh.settings();
        let open = MenuItem::new("Open Vdisplay", true, None);
        let stream = MenuItem::new(
            if sh.stream.load(Ordering::Relaxed) {
                "Stop streaming"
            } else {
                "Start streaming"
            },
            true,
            None,
        );
        let radiant = CheckMenuItem::new(
            "Radiant background",
            true,
            s.bg_style == BgStyle::Radiant,
            None,
        );
        let lyrics = CheckMenuItem::new("Show synced lyrics", true, s.show_lyrics, None);
        let debug = CheckMenuItem::new("Debug overlay", true, s.show_debug_overlay, None);
        let fullscreen = CheckMenuItem::new(
            "Pause during fullscreen apps",
            true,
            s.pause_on_fullscreen,
            None,
        );
        let color = MenuItem::new("Color calibration…", true, None);
        let hide = MenuItem::new("Hide to tray", true, None);
        let exit = MenuItem::new("Exit Vdisplay", true, None);
        let menu = Menu::new();
        let sep = PredefinedMenuItem::separator();
        let sep2 = PredefinedMenuItem::separator();
        let sep3 = PredefinedMenuItem::separator();
        menu.append_items(&[
            &open,
            &sep,
            &stream,
            &sep2,
            &radiant,
            &lyrics,
            &debug,
            &fullscreen,
            &color,
            &sep3,
            &hide,
            &exit,
        ])
        .ok()?;
        let tray = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip("Vdisplay")
            .with_icon(tray_icon_image())
            .with_menu_on_left_click(false)
            .build()
            .ok()?;
        Some(TrayItems {
            _tray: tray,
            open,
            stream,
            radiant,
            lyrics,
            debug,
            fullscreen,
            color,
            hide,
            exit,
        })
    }

    fn show(&mut self, ctx: &egui::Context) {
        self.pending_hide = 0;
        if let Some(h) = self.cloaked_hwnd.take() {
            winutil::set_cloaked(h, false);
        }
        self.visible = true;
        ctx.send_viewport_cmd(ViewportCommand::Visible(true));
        ctx.send_viewport_cmd(ViewportCommand::Minimized(false));
        ctx.send_viewport_cmd(ViewportCommand::Focus);
    }

    fn hide(&mut self, ctx: &egui::Context) {
        self.visible = false;
        ctx.send_viewport_cmd(ViewportCommand::Visible(false));
    }

    fn quit(&mut self, ctx: &egui::Context) {
        self.sh.quit.store(true, Ordering::Release);
        self.sh.poke();
        if let Some(h) = self.engine.take() {
            let _ = h.join();
        }
        self.sh.save_now();
        winutil::end_timer_resolution();
        self.allow_close = true;
        // Safety net in case the close command isn't processed while hidden.
        std::thread::spawn(|| {
            std::thread::sleep(Duration::from_secs(3));
            std::process::exit(0);
        });
        ctx.send_viewport_cmd(ViewportCommand::Close);
    }

    fn toggle_stream(&self) {
        let on = !self.sh.stream.load(Ordering::Relaxed);
        self.sh.stream.store(on, Ordering::Release);
        if on {
            self.sh.rescan.store(true, Ordering::Release);
        }
        self.sh.poke();
    }

    fn set_mode(&self, m: Mode) {
        self.sh.update(|s| s.current_mode = m);
        // Picking a mode while idle starts streaming.
        if !self.sh.stream.load(Ordering::Relaxed) {
            self.toggle_stream();
        }
    }

    fn handle_menu(&mut self, ctx: &egui::Context, id: MenuId) {
        let Some(t) = &self.tray else { return };
        if id == *t.open.id() {
            self.show(ctx);
        } else if id == *t.stream.id() {
            self.toggle_stream();
        } else if id == *t.radiant.id() {
            self.sh.update(|s| {
                s.bg_style = if s.bg_style == BgStyle::Radiant {
                    BgStyle::Gradient
                } else {
                    BgStyle::Radiant
                }
            });
        } else if id == *t.lyrics.id() {
            self.sh.update(|s| s.show_lyrics = !s.show_lyrics);
        } else if id == *t.debug.id() {
            self.sh
                .update(|s| s.show_debug_overlay = !s.show_debug_overlay);
        } else if id == *t.fullscreen.id() {
            self.sh
                .update(|s| s.pause_on_fullscreen = !s.pause_on_fullscreen);
        } else if id == *t.color.id() {
            self.flash_color = Some(Instant::now());
            self.scroll_to_color = true;
            self.show(ctx);
        } else if id == *t.hide.id() {
            self.hide(ctx);
        } else if id == *t.exit.id() {
            self.quit(ctx);
        }
    }

    fn sync_tray(&self) {
        let Some(t) = &self.tray else { return };
        let s = self.sh.settings.read();
        t.radiant.set_checked(s.bg_style == BgStyle::Radiant);
        t.lyrics.set_checked(s.show_lyrics);
        t.debug.set_checked(s.show_debug_overlay);
        t.fullscreen.set_checked(s.pause_on_fullscreen);
        t.stream
            .set_text(if self.sh.stream.load(Ordering::Relaxed) {
                "Stop streaming"
            } else {
                "Start streaming"
            });
    }

    fn load_file_dialog(&self) {
        if self.loading.swap(true, Ordering::AcqRel) {
            return;
        }
        let (sh, loading) = (self.sh.clone(), self.loading.clone());
        std::thread::spawn(move || {
            let pick = rfd::FileDialog::new()
                .set_title("Choose an image or animated GIF")
                .add_filter(
                    "Images & animations",
                    &["gif", "png", "jpg", "jpeg", "bmp", "webp"],
                )
                .add_filter("All files", &["*"])
                .pick_file();
            if let Some(p) = pick {
                *sh.media_message.lock() = "Loading…".into();
                sh.repaint();
                let fit = sh.settings.read().media_fit;
                match loader::load_path(&p, fit) {
                    Ok(m) => {
                        sh.set_media(m);
                        let path = p.to_string_lossy().into_owned();
                        sh.update(|s| {
                            s.media_path = path;
                            s.current_mode = Mode::MediaFile;
                        });
                    }
                    Err(e) => {
                        *sh.media_message.lock() = format!("Couldn't load {}: {e}", p.display())
                    }
                }
            }
            loading.store(false, Ordering::Release);
            sh.repaint();
        });
    }

    fn reload_media(&self) {
        let (sh, loading) = (self.sh.clone(), self.loading.clone());
        if loading.swap(true, Ordering::AcqRel) {
            return;
        }
        std::thread::spawn(move || {
            let (src, fit) = {
                let s = sh.settings.read();
                (s.media_path.clone(), s.media_fit)
            };
            *sh.media_message.lock() = "Loading…".into();
            sh.repaint();
            match loader::load_source(&src, fit) {
                Ok(m) => sh.set_media(m),
                Err(e) => *sh.media_message.lock() = format!("Couldn't load media: {e}"),
            }
            loading.store(false, Ordering::Release);
            sh.repaint();
        });
    }

    // Sections

    fn top_bar(&mut self, ui: &mut Ui) {
        let st = self.sh.status.lock().clone();
        let streaming = self.sh.stream.load(Ordering::Relaxed);
        ui.horizontal(|ui| {
            ui.set_height(44.0);
            ui.add(egui::Image::new((self.logo_tex.id(), Vec2::splat(34.0))));
            ui.add_space(4.0);
            ui.vertical(|ui| {
                ui.add_space(3.0);
                ui.spacing_mut().item_spacing.y = 0.0;
                ui.label(egui::RichText::new("Vdisplay").font(bold(19.0)).color(TEXT));
                ui.label(
                    egui::RichText::new("MC-3TCORELCD CONTROLLER")
                        .font(semi(10.0))
                        .color(DIM)
                        .extra_letter_spacing(1.0),
                );
            });
            ui.add_space(18.0);

            let (pc, pt) = match &st.port {
                Some(p) => (GREEN, format!("Screen on {p}")),
                None => (ACCENT, "Screen not detected".to_string()),
            };
            chip(ui, pc, &pt, false);
            let (sc, stx, pulse) = match st.state {
                LinkState::Idle => (FAINT, "Standby".to_string(), false),
                LinkState::NoDevice => (AMBER, "Waiting for screen".to_string(), true),
                LinkState::Connecting => (AMBER, "Connecting…".to_string(), true),
                LinkState::Live => (GREEN, format!("Live · {:.0} FPS", st.fps), true),
                LinkState::Paused => (BLUE, "Paused for fullscreen app".to_string(), false),
                LinkState::Error => (ACCENT, "Connection error".to_string(), false),
            };
            chip(ui, sc, &stx, pulse);

            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let r = if streaming {
                    button(
                        ui,
                        I_STOP,
                        "Stop streaming",
                        Kind::Danger,
                        Some(160.0),
                        38.0,
                    )
                } else {
                    button(
                        ui,
                        I_PLAY,
                        "Start streaming",
                        Kind::Primary,
                        Some(160.0),
                        38.0,
                    )
                };
                if r.clicked() {
                    self.toggle_stream();
                }
                if button(ui, I_REFRESH, "Re-scan", Kind::Ghost, None, 38.0)
                    .on_hover_text("Look for the screen's COM port again")
                    .clicked()
                {
                    self.sh.rescan.store(true, Ordering::Release);
                    self.sh.poke();
                }
            });
        });
        if !st.message.is_empty()
            && matches!(
                st.state,
                LinkState::Error | LinkState::NoDevice | LinkState::Connecting | LinkState::Idle
            )
        {
            ui.label(egui::RichText::new(&st.message).font(reg(12.0)).color(
                if st.state == LinkState::Error {
                    ACCENT
                } else {
                    DIM
                },
            ));
        }
    }

    fn mode_card(&mut self, ui: &mut Ui) {
        let mode = self.sh.settings.read().current_mode;
        card(ui, I_MONITOR, "Display mode", |ui| {
            let gap = 8.0;
            let w = (ui.available_width() - gap * 2.0) / 3.0;
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = gap;
                if mode_tile(
                    ui,
                    w,
                    mode == Mode::Media,
                    I_MUSIC,
                    "Music",
                    "Cover & lyrics",
                )
                .clicked()
                {
                    self.set_mode(Mode::Media);
                }
                if mode_tile(
                    ui,
                    w,
                    mode == Mode::Hardware,
                    I_HUD,
                    "System",
                    "CPU · GPU · RAM",
                )
                .clicked()
                {
                    self.set_mode(Mode::Hardware);
                }
                if mode_tile(
                    ui,
                    w,
                    mode == Mode::MediaFile,
                    I_PHOTO,
                    "Custom",
                    "GIF or picture",
                )
                .clicked()
                {
                    self.set_mode(Mode::MediaFile);
                }
            });
        });
    }

    fn now_playing_card(&mut self, ui: &mut Ui, ctx: &egui::Context) {
        let info = self.sh.media.latest();
        let lyr = self.sh.status.lock().lyrics;
        card(ui, I_MUSIC, "Now playing", |ui| {
            let Some(info) = info else {
                ui.label(egui::RichText::new("Nothing is playing. Start music in Spotify, TIDAL, Apple Music, a browser…").font(reg(12.5)).color(DIM));
                return;
            };
            if let Some(a) = &info.art
                && self.art_tex.as_ref().is_none_or(|(id, _)| *id != a.id)
            {
                let img = egui::ColorImage::from_rgb(
                    [a.img.width() as usize, a.img.height() as usize],
                    a.img.as_raw(),
                );
                self.art_tex = Some((a.id, ctx.load_texture("art", img, TextureOptions::LINEAR)));
            }
            ui.horizontal(|ui| {
                let (r, _) = ui.allocate_exact_size(Vec2::splat(64.0), Sense::hover());
                match (&info.art, &self.art_tex) {
                    (Some(_), Some((_, t))) => {
                        egui::Image::new((t.id(), r.size()))
                            .corner_radius(CornerRadius::same(10))
                            .paint_at(ui, r);
                    }
                    _ => {
                        ui.painter().rect(
                            r,
                            CornerRadius::same(10),
                            WELL,
                            Stroke::new(1.0, BORDER),
                            StrokeKind::Inside,
                        );
                        ui.painter().text(
                            r.center(),
                            Align2::CENTER_CENTER,
                            I_MUSIC,
                            icon(20.0),
                            FAINT,
                        );
                    }
                }
                ui.add_space(4.0);
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(&info.title)
                                .font(semi(15.0))
                                .color(TEXT),
                        )
                        .truncate(),
                    );
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(&info.artist)
                                .font(reg(12.5))
                                .color(MUTED),
                        )
                        .truncate(),
                    );
                    let app = info
                        .source_app
                        .split('!')
                        .next()
                        .unwrap_or("")
                        .trim_end_matches(".exe")
                        .to_string();
                    let (lc, lt) = match lyr {
                        LyricsUi::WordSynced(src) => (GREEN, format!("Word-synced lyrics · {src}")),
                        LyricsUi::Synced(src) => (GREEN, format!("Synced lyrics · {src}")),
                        LyricsUi::Loading => (AMBER, "Finding lyrics…".to_string()),
                        LyricsUi::Missing => (DIM, "No synced lyrics found".to_string()),
                        LyricsUi::Off => (
                            DIM,
                            if self.sh.settings.read().show_lyrics {
                                "Lyrics idle".into()
                            } else {
                                "Lyrics off".to_string()
                            },
                        ),
                    };
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new(if info.is_playing {
                                "● Playing"
                            } else {
                                "❚❚ Paused"
                            })
                            .font(semi(11.5))
                            .color(if info.is_playing {
                                GREEN
                            } else {
                                DIM
                            }),
                        );
                        ui.label(
                            egui::RichText::new(format!("· {app}"))
                                .font(reg(11.5))
                                .color(DIM),
                        );
                    });
                    ui.label(egui::RichText::new(lt).font(reg(11.5)).color(lc));
                });
            });
            ui.add_space(4.0);
            let pos = info.position_now();
            let frac = if info.duration_ms > 0.0 {
                (pos / info.duration_ms) as f32
            } else {
                0.0
            };
            let (r, _) =
                ui.allocate_exact_size(Vec2::new(ui.available_width(), 4.0), Sense::hover());
            ui.painter()
                .rect_filled(r, CornerRadius::same(2), WELL_HOVER);
            ui.painter().rect_filled(
                Rect::from_min_size(r.min, Vec2::new(r.width() * frac.clamp(0.0, 1.0), 4.0)),
                CornerRadius::same(2),
                ACCENT,
            );
            ui.horizontal(|ui| {
                let f = |ms: f64| {
                    format!(
                        "{}:{:02}",
                        (ms / 60000.0) as i64,
                        ((ms / 1000.0) as i64) % 60
                    )
                };
                ui.label(egui::RichText::new(f(pos)).font(mono(11.0)).color(DIM));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.label(
                        egui::RichText::new(f(info.duration_ms))
                            .font(mono(11.0))
                            .color(DIM),
                    );
                });
            });
        });
    }

    fn music_card(&mut self, ui: &mut Ui) {
        let mut s = self.sh.settings();
        let before = (
            s.bg_style,
            s.music_fps,
            s.show_lyrics,
            s.lyrics_offset_sec,
            s.idle_mode,
        );
        card(ui, I_SETTINGS, "Music screen", |ui| {
            row(ui, "Background", "Fluid artwork or gradient", 190.0, |ui| {
                segmented(
                    ui,
                    &mut s.bg_style,
                    &[
                        (BgStyle::Radiant, "Radiant"),
                        (BgStyle::Gradient, "Gradient"),
                    ],
                    190.0,
                );
            });
            row(ui, "Frame rate", "Smoother vs. lighter", 190.0, |ui| {
                segmented(
                    ui,
                    &mut s.music_fps,
                    &[(15, "15"), (30, "30"), (60, "60")],
                    190.0,
                );
            });
            row(
                ui,
                "Synced lyrics",
                "Radiant Lyrics · LRCLIB",
                190.0,
                |ui| {
                    toggle(ui, &mut s.show_lyrics);
                },
            );
            let has_key = self.sh.lyrics.has_tokens();
            let mut open_editor = false;
            row(
                ui,
                "Radiant Lyrics key",
                if has_key {
                    "Word-synced lyrics enabled"
                } else {
                    "Optional, for word-synced lyrics"
                },
                190.0,
                |ui| {
                    if self.rl_edit.is_none() {
                        open_editor = button(
                            ui,
                            "",
                            if has_key { "Change key" } else { "Add key" },
                            Kind::Secondary,
                            Some(110.0),
                            30.0,
                        )
                        .clicked();
                    }
                },
            );
            if open_editor {
                self.rl_edit = Some(Default::default());
            }
            if let Some((id, token)) = &mut self.rl_edit {
                let field = |ui: &mut Ui, v: &mut String, hint: &str, secret: bool| {
                    ui.add_sized(
                        Vec2::new(ui.available_width(), 30.0),
                        egui::TextEdit::singleline(v)
                            .hint_text(hint)
                            .password(secret)
                            .font(mono(12.5))
                            .margin(Margin::symmetric(10, 7)),
                    );
                };
                field(ui, id, "Token ID", false);
                field(ui, token, "Token", true);
                let mut action = 0;
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    let ready = !id.trim().is_empty() && !token.trim().is_empty();
                    if button(
                        ui,
                        "",
                        "Save",
                        if ready {
                            Kind::Primary
                        } else {
                            Kind::Secondary
                        },
                        Some(80.0),
                        30.0,
                    )
                    .clicked()
                        && ready
                    {
                        action = 1;
                    }
                    if button(ui, "", "Cancel", Kind::Ghost, Some(80.0), 30.0).clicked() {
                        action = 2;
                    }
                    if has_key
                        && button(ui, "", "Remove key", Kind::Danger, Some(110.0), 30.0).clicked()
                    {
                        action = 3;
                    }
                });
                let keys = (id.trim().to_string(), token.trim().to_string());
                match action {
                    1 | 3 => {
                        let keys = if action == 1 {
                            keys
                        } else {
                            (String::new(), String::new())
                        };
                        self.sh.update(|t| {
                            for (k, v) in [
                                (lyrics::TOKEN_ID_KEY, &keys.0),
                                (lyrics::TOKEN_KEY, &keys.1),
                            ] {
                                if v.is_empty() {
                                    t.extra.remove(k);
                                } else {
                                    t.extra.insert(k.into(), v.clone().into());
                                }
                            }
                        });
                        self.sh
                            .lyrics
                            .set_tokens(lyrics::resolve_tokens(&self.sh.settings().extra));
                        self.rl_edit = None;
                    }
                    2 => self.rl_edit = None,
                    _ => {}
                }
            }
            row(
                ui,
                "Lyrics offset",
                "Positive shows lines earlier",
                190.0,
                |ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    if button(ui, I_RESET, "", Kind::Ghost, Some(32.0), 30.0)
                        .on_hover_text("Reset to 0")
                        .clicked()
                    {
                        s.lyrics_offset_sec = 0.0;
                    }
                    if button(ui, I_PLUS, "", Kind::Secondary, Some(32.0), 30.0).clicked() {
                        s.lyrics_offset_sec = ((s.lyrics_offset_sec + 0.1) * 10.0).round() / 10.0;
                    }
                    ui.add_sized(
                        Vec2::new(70.0, 30.0),
                        egui::DragValue::new(&mut s.lyrics_offset_sec)
                            .speed(0.01)
                            .range(-10.0..=10.0)
                            .fixed_decimals(2)
                            .suffix(" s"),
                    );
                    if button(ui, I_MINUS, "", Kind::Secondary, Some(32.0), 30.0).clicked() {
                        s.lyrics_offset_sec = ((s.lyrics_offset_sec - 0.1) * 10.0).round() / 10.0;
                    }
                },
            );
            row(ui, "When nothing plays", "", 250.0, |ui| {
                segmented(
                    ui,
                    &mut s.idle_mode,
                    &[
                        (IdleMode::Hardware, "System"),
                        (IdleMode::MediaFile, "Custom"),
                        (IdleMode::Standby, "Clock"),
                    ],
                    250.0,
                );
            });
        });
        s.lyrics_offset_sec = s.lyrics_offset_sec.clamp(-10.0, 10.0);
        if before
            != (
                s.bg_style,
                s.music_fps,
                s.show_lyrics,
                s.lyrics_offset_sec,
                s.idle_mode,
            )
        {
            self.sh.update(|t| {
                t.bg_style = s.bg_style;
                t.music_fps = s.music_fps;
                t.show_lyrics = s.show_lyrics;
                t.lyrics_offset_sec = s.lyrics_offset_sec;
                t.idle_mode = s.idle_mode;
            });
        }
    }

    fn media_card(&mut self, ui: &mut Ui) {
        let mut s = self.sh.settings();
        let msg = self.sh.media_message.lock().clone();
        let loading = self.loading.load(Ordering::Relaxed);
        card(ui, I_PHOTO, "Custom media", |ui| {
            ui.horizontal(|ui| {
                if button(
                    ui,
                    I_FOLDER,
                    if loading {
                        "Loading…"
                    } else {
                        "Open image or GIF…"
                    },
                    Kind::Secondary,
                    None,
                    34.0,
                )
                .clicked()
                {
                    self.load_file_dialog();
                }
            });
            ui.add(
                egui::Label::new(egui::RichText::new(msg).font(reg(12.0)).color(DIM)).truncate(),
            );
            ui.add_space(2.0);
            let fit_before = s.media_fit;
            row(ui, "Fit", "Crop, letterbox or stretch", 210.0, |ui| {
                segmented(
                    ui,
                    &mut s.media_fit,
                    &[
                        (MediaFit::Fill, "Fill"),
                        (MediaFit::Fit, "Fit"),
                        (MediaFit::Stretch, "Stretch"),
                    ],
                    210.0,
                );
            });
            let gif_before = s.gif_fps;
            row(
                ui,
                "Animation speed",
                "Auto uses the file timing",
                210.0,
                |ui| {
                    segmented(
                        ui,
                        &mut s.gif_fps,
                        &[
                            (None, "Auto"),
                            (Some(15), "15"),
                            (Some(30), "30"),
                            (Some(60), "60"),
                        ],
                        210.0,
                    );
                },
            );
            ui.add_space(2.0);
            ui.label(
                egui::RichText::new("PRESETS")
                    .font(semi(10.5))
                    .color(DIM)
                    .extra_letter_spacing(1.0),
            );
            ui.horizontal(|ui| {
                for (key, label) in [
                    ("crimson", "OLED Crimson"),
                    ("gradient", "Cyber Gradient"),
                    ("black", "Pure Black"),
                ] {
                    if button(ui, "", label, Kind::Ghost, None, 30.0).clicked() {
                        self.sh.set_media(loader::load_preset(key));
                        self.sh.update(|t| {
                            t.media_path = format!("preset:{key}");
                            t.current_mode = Mode::MediaFile;
                        });
                    }
                }
            });
            if fit_before != s.media_fit {
                self.sh.update(|t| t.media_fit = s.media_fit);
                self.reload_media();
            }
            if gif_before != s.gif_fps {
                self.sh.update(|t| t.gif_fps = s.gif_fps);
            }
        });
    }

    fn color_card(&mut self, ui: &mut Ui) {
        let mut s = self.sh.settings();
        let before = (s.brightness, s.saturation, s.hue);
        let flash = self
            .flash_color
            .map(|t| t.elapsed().as_secs_f32())
            .filter(|t| *t < 1.6);
        let resp = egui::Frame::new()
            .stroke(Stroke::new(
                1.5,
                match flash {
                    Some(t) => ACCENT.gamma_multiply(1.0 - t / 1.6),
                    None => Color32::TRANSPARENT,
                },
            ))
            .corner_radius(CornerRadius::same(17))
            .show(ui, |ui| {
                card(ui, I_COLOR, "Color calibration", |ui| {
                    slider(
                        ui,
                        "Brightness",
                        &mut s.brightness,
                        10,
                        200,
                        100,
                        "%",
                        ACCENT,
                    );
                    slider(ui, "Saturation", &mut s.saturation, 0, 200, 100, "%", BLUE);
                    slider(ui, "Hue shift", &mut s.hue, -180, 180, 0, "°", PURPLE);
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        for (label, (b, sa, h)) in [
                            ("Default", (100, 100, 0)),
                            ("Vivid OLED", (110, 135, 0)),
                            ("Warm", (95, 110, -12)),
                            ("Cool", (100, 110, 12)),
                        ] {
                            let active = (s.brightness, s.saturation, s.hue) == (b, sa, h);
                            if button(
                                ui,
                                "",
                                label,
                                if active { Kind::Secondary } else { Kind::Ghost },
                                None,
                                30.0,
                            )
                            .clicked()
                            {
                                (s.brightness, s.saturation, s.hue) = (b, sa, h);
                            }
                        }
                    });
                });
            });
        if std::mem::take(&mut self.scroll_to_color) {
            resp.response.scroll_to_me(Some(Align::Center));
            self.flash_color = Some(Instant::now());
        }
        if flash.is_some() {
            ui.ctx().request_repaint_after(Duration::from_millis(16));
        }
        if before != (s.brightness, s.saturation, s.hue) {
            self.sh.update(|t| {
                t.brightness = s.brightness;
                t.saturation = s.saturation;
                t.hue = s.hue;
            });
        }
    }

    fn system_card(&mut self, ui: &mut Ui) {
        let mut s = self.sh.settings();
        let before = (
            s.pause_on_fullscreen,
            s.show_debug_overlay,
            s.start_minimized,
            s.auto_stream,
        );
        let mut startup = self.run_at_startup;
        self.refresh_sensor_ui(false);
        let sensor = self.sensor_ui.lock().clone();
        let mut sensor_click = false;
        card(ui, I_SETTINGS, "System", |ui| {
            let (label, hint, col) = match (sensor.state, sensor.busy) {
                (_, true) => (
                    "Working…".to_string(),
                    "Approve the administrator prompt",
                    AMBER,
                ),
                (SensorState::Live, _) => (
                    format!("Live · {}", sensor.source),
                    "CPU temperature, power and fans",
                    GREEN,
                ),
                (SensorState::HwinfoNeedsSetup, _) => (
                    "Set up HWiNFO".into(),
                    "Needed for CPU temperature and fans",
                    TEXT,
                ),
                (SensorState::HwinfoNoSharedMemory, _) => (
                    "Set up HWiNFO".into(),
                    "HWiNFO runs, but sensor sharing is off",
                    TEXT,
                ),
                (SensorState::HwinfoStarting, _) => (
                    "Start HWiNFO".into(),
                    "Set up, starts automatically when needed",
                    TEXT,
                ),
                (SensorState::NoProvider, _) => (
                    "Install HWiNFO".into(),
                    "Free sensor tool, installed for you",
                    TEXT,
                ),
                (SensorState::Unknown, _) => ("Checking…".into(), "", DIM),
            };
            row(ui, "Hardware sensors", hint, 170.0, |ui| {
                let actionable = !sensor.busy
                    && !matches!(sensor.state, SensorState::Live | SensorState::Unknown);
                if actionable {
                    sensor_click =
                        button(ui, "", &label, Kind::Secondary, Some(150.0), 30.0).clicked();
                } else {
                    ui.label(egui::RichText::new(&label).font(semi(12.5)).color(col));
                }
            });
            row(
                ui,
                "Pause during fullscreen games",
                "Frees CPU/GPU while gaming",
                44.0,
                |ui| {
                    toggle(ui, &mut s.pause_on_fullscreen);
                },
            );
            row(
                ui,
                "Performance overlay",
                "FPS and latency on the panel",
                44.0,
                |ui| {
                    toggle(ui, &mut s.show_debug_overlay);
                },
            );
            row(
                ui,
                "Stream on launch",
                "Take over the screen at start",
                44.0,
                |ui| {
                    toggle(ui, &mut s.auto_stream);
                },
            );
            row(
                ui,
                "Start in the tray",
                "Open without this window",
                44.0,
                |ui| {
                    toggle(ui, &mut s.start_minimized);
                },
            );
            row(
                ui,
                "Launch with Windows",
                "Starts minimized at sign-in",
                44.0,
                |ui| {
                    toggle(ui, &mut startup);
                },
            );
            ui.add_space(4.0);
            if button(
                ui,
                I_PLUG,
                "Release COM port (close Mars Gaming software)",
                Kind::Ghost,
                Some(ui.available_width()),
                32.0,
            )
            .clicked()
            {
                self.sh.release_port.store(true, Ordering::Release);
                self.sh.poke();
            }
        });
        if before
            != (
                s.pause_on_fullscreen,
                s.show_debug_overlay,
                s.start_minimized,
                s.auto_stream,
            )
        {
            self.sh.update(|t| {
                t.pause_on_fullscreen = s.pause_on_fullscreen;
                t.show_debug_overlay = s.show_debug_overlay;
                t.start_minimized = s.start_minimized;
                t.auto_stream = s.auto_stream;
            });
        }
        if sensor_click {
            self.sensor_action(sensor.state);
        }
        if let Some(msg) = self.sensor_ui.lock().message.take() {
            self.toast = Some((format!("HWiNFO setup: {msg}"), Instant::now()));
        }
        if startup != self.run_at_startup {
            match winutil::set_run_at_startup(startup) {
                Ok(()) => self.run_at_startup = startup,
                Err(e) => {
                    self.toast = Some((
                        format!("Couldn't change startup entry: {e}"),
                        Instant::now(),
                    ))
                }
            }
        }
    }

    /// Re-checks sensor availability in the background (every 8 s while the window is open).
    fn refresh_sensor_ui(&self, force: bool) {
        let mut st = self.sensor_ui.lock();
        if st.busy
            || st.checking
            || (!force
                && st
                    .checked_at
                    .is_some_and(|t| t.elapsed() < Duration::from_secs(8)))
        {
            return;
        }
        st.checking = true;
        drop(st);
        let (ui_state, sh) = (self.sensor_ui.clone(), self.sh.clone());
        std::thread::spawn(move || {
            let (state, source) = sensors::quick_state();
            let mut st = ui_state.lock();
            st.state = state;
            st.source = source;
            st.checked_at = Some(Instant::now());
            st.checking = false;
            drop(st);
            sh.repaint();
        });
    }

    fn sensor_action(&mut self, state: SensorState) {
        match state {
            SensorState::HwinfoStarting => {
                sensors::run_task();
                self.sensor_ui.lock().checked_at = None;
            }
            _ => {
                self.sensor_ui.lock().busy = true;
                let (ui_state, sh) = (self.sensor_ui.clone(), self.sh.clone());
                std::thread::spawn(move || {
                    let res = sensors::setup_hwinfo();
                    sh.hw.env_changed.store(true, Ordering::Release);
                    // Give HWiNFO a moment to publish its first readings.
                    std::thread::sleep(Duration::from_secs(if res.is_ok() { 6 } else { 0 }));
                    let mut st = ui_state.lock();
                    st.busy = false;
                    st.checked_at = None;
                    st.message = res.err();
                    drop(st);
                    sh.poke();
                    sh.repaint();
                });
            }
        }
    }

    fn preview_panel(&mut self, ui: &mut Ui, ctx: &egui::Context) {
        {
            let p = self.sh.preview.lock();
            if p.seq != self.preview_seq
                && p.rgb.len() == (crate::render::W * crate::render::H * 3) as usize
            {
                self.preview_seq = p.seq;
                let img = egui::ColorImage::from_rgb(
                    [crate::render::W as usize, crate::render::H as usize],
                    &p.rgb,
                );
                match &mut self.preview_tex {
                    Some(t) => t.set(img, TextureOptions::LINEAR),
                    None => {
                        self.preview_tex =
                            Some(ctx.load_texture("preview", img, TextureOptions::LINEAR))
                    }
                }
            }
        }
        let st = self.sh.status.lock().clone();
        egui::Frame::new()
            .fill(CARD)
            .stroke(Stroke::new(1.0, BORDER))
            .corner_radius(CornerRadius::same(16))
            .inner_margin(Margin::same(16))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.set_height(ui.available_height());
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new(I_MONITOR)
                            .font(icon(13.0))
                            .color(ACCENT),
                    );
                    ui.label(
                        egui::RichText::new("LIVE PREVIEW")
                            .font(semi(11.5))
                            .color(MUTED)
                            .extra_letter_spacing(1.2),
                    );
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let (t, c) = match st.state {
                            LinkState::Live => ("ON PANEL", GREEN),
                            LinkState::Paused => ("PAUSED", BLUE),
                            _ => ("PREVIEW ONLY", DIM),
                        };
                        ui.label(
                            egui::RichText::new(t)
                                .font(semi(10.5))
                                .color(c)
                                .extra_letter_spacing(0.8),
                        );
                    });
                });
                ui.add_space(6.0);
                let metrics_h = 2.0 * 52.0 + 8.0 + 12.0;
                let avail = ui.available_size();
                let h = (avail.y - metrics_h)
                    .min(avail.x * 960.0 / 360.0)
                    .max(200.0);
                let w = h * 360.0 / 960.0;
                ui.vertical_centered(|ui| {
                    let (r, _) =
                        ui.allocate_exact_size(Vec2::new(w + 12.0, h + 12.0), Sense::hover());
                    ui.painter().rect(
                        r,
                        CornerRadius::same(18),
                        BG,
                        Stroke::new(1.0, BORDER_HI),
                        StrokeKind::Inside,
                    );
                    let inner = r.shrink(6.0);
                    match &self.preview_tex {
                        Some(t) => {
                            egui::Image::new((t.id(), inner.size()))
                                .corner_radius(CornerRadius::same(12))
                                .paint_at(ui, inner);
                        }
                        None => {
                            ui.painter().text(
                                inner.center(),
                                Align2::CENTER_CENTER,
                                "Starting renderer…",
                                reg(12.0),
                                DIM,
                            );
                        }
                    }
                });
                ui.add_space(8.0);
                let mw = (ui.available_width() - 8.0) / 2.0;
                let live = st.state == LinkState::Live;
                let v = |s: String| if live { s } else { "—".into() };
                ui.horizontal(|ui| {
                    metric(
                        ui,
                        mw,
                        "Frame rate",
                        v(format!("{:.1} fps", st.fps)),
                        if st.fps >= 55.0 || !live { TEXT } else { AMBER },
                    );
                    metric(ui, mw, "Frame size", v(format!("{:.1} KB", st.kb)), TEXT);
                });
                ui.horizontal(|ui| {
                    metric(ui, mw, "Bandwidth", v(format!("{:.1} Mbps", st.mbps)), TEXT);
                    let lat = st.rnd_ms + st.jpg_ms + st.tx_ms;
                    metric(
                        ui,
                        mw,
                        "Pipeline",
                        v(format!("{lat:.1} ms")),
                        if lat < 16.6 || !live { TEXT } else { AMBER },
                    );
                });
            });
    }
}

impl eframe::App for App {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if self.pending_hide > 0 {
            self.pending_hide -= 1;
            ctx.send_viewport_cmd(ViewportCommand::Visible(false));
            if self.pending_hide == 0 {
                if let Some(h) = self.cloaked_hwnd.take() {
                    winutil::set_cloaked(h, false);
                }
            } else {
                ctx.request_repaint();
            }
        }
        while let Ok(cmd) = self.rx.try_recv() {
            match cmd {
                GuiCmd::Show => self.show(ctx),
                GuiCmd::Menu(id) => self.handle_menu(ctx, id),
            }
        }
        if ctx.input(|i| i.viewport().close_requested()) && !self.allow_close {
            ctx.send_viewport_cmd(ViewportCommand::CancelClose);
            self.hide(ctx);
        }
        let minimized = ctx.input(|i| i.viewport().minimized.unwrap_or(false));
        self.sh
            .gui_visible
            .store(self.visible && !minimized, Ordering::Relaxed);
        self.sync_tray();
    }

    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        egui::Panel::top("top")
            .frame(egui::Frame::new().fill(BG).inner_margin(Margin {
                left: 20,
                right: 20,
                top: 14,
                bottom: 12,
            }))
            .show(ui, |ui| {
                self.top_bar(ui);
                let r = ui.max_rect();
                ui.painter().hline(
                    r.x_range().expand(20.0),
                    r.bottom() + 12.0,
                    Stroke::new(1.0, BORDER),
                );
            });

        egui::Panel::right("preview")
            .exact_size(330.0)
            .resizable(false)
            .frame(egui::Frame::new().fill(BG).inner_margin(Margin {
                left: 0,
                right: 20,
                top: 16,
                bottom: 16,
            }))
            .show(ui, |ui| self.preview_panel(ui, &ctx));

        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(BG).inner_margin(Margin {
                left: 20,
                right: 12,
                top: 16,
                bottom: 4,
            }))
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = 12.0;
                        let total = ui.available_width() - 8.0;
                        let gap = 12.0;
                        if total > 760.0 {
                            let cw = (total - gap) / 2.0;
                            ui.horizontal_top(|ui| {
                                ui.spacing_mut().item_spacing.x = gap;
                                ui.vertical(|ui| {
                                    ui.set_width(cw);
                                    self.mode_card(ui);
                                    self.now_playing_card(ui, &ctx);
                                    self.music_card(ui);
                                });
                                ui.vertical(|ui| {
                                    ui.set_width(cw);
                                    self.media_card(ui);
                                    self.color_card(ui);
                                    self.system_card(ui);
                                });
                            });
                        } else {
                            ui.set_width(total);
                            self.mode_card(ui);
                            self.now_playing_card(ui, &ctx);
                            self.music_card(ui);
                            self.media_card(ui);
                            self.color_card(ui);
                            self.system_card(ui);
                        }
                        if let Some((msg, t)) = &self.toast
                            && t.elapsed() < Duration::from_secs(6)
                        {
                            ui.label(egui::RichText::new(msg).font(reg(12.0)).color(ACCENT));
                        }
                        ui.add_space(8.0);
                    });
            });

        // Keep the now-playing progress and status chips fresh; the engine also pokes us on
        // every new preview frame.
        ctx.request_repaint_after(Duration::from_millis(250));
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0, 0.0, 0.0, 1.0]
    }
}
