//! Vector brand icons for the media header, plus the Vdisplay app logo.

use super::canvas::{pixmap_from_rgba, rrect_path};
use crate::media::AppIcon;
use tiny_skia::{Color, FillRule, LineCap, Paint, PathBuilder, Pixmap, Stroke, Transform};

fn paint(r: u8, g: u8, b: u8) -> Paint<'static> {
    let mut p = Paint::default();
    p.set_color(Color::from_rgba8(r, g, b, 255));
    p.anti_alias = true;
    p
}

/// Arc: bounding box + angles in degrees clockwise from 3 o'clock.
fn arc(pm: &mut Pixmap, x0: f32, y0: f32, x1: f32, y1: f32, a0: f32, a1: f32, w: f32, p: &Paint) {
    let (cx, cy, rx, ry) = (
        (x0 + x1) / 2.0,
        (y0 + y1) / 2.0,
        (x1 - x0) / 2.0,
        (y1 - y0) / 2.0,
    );
    let mut pb = PathBuilder::new();
    let n = 32;
    for i in 0..=n {
        let a = (a0 + (a1 - a0) * i as f32 / n as f32).to_radians();
        let (x, y) = (cx + rx * a.cos(), cy + ry * a.sin());
        if i == 0 {
            pb.move_to(x, y)
        } else {
            pb.line_to(x, y)
        }
    }
    if let Some(path) = pb.finish() {
        let s = Stroke {
            width: w,
            line_cap: LineCap::Round,
            ..Default::default()
        };
        pm.stroke_path(&path, p, &s, Transform::identity(), None);
    }
}

fn rrect(pm: &mut Pixmap, x0: f32, y0: f32, x1: f32, y1: f32, r: f32, p: &Paint) {
    if let Some(path) = rrect_path(x0, y0, x1, y1, r) {
        pm.fill_path(&path, p, FillRule::Winding, Transform::identity(), None);
    }
}

fn spotify(size: u32) -> Pixmap {
    let mut pm = Pixmap::new(size, size).unwrap();
    let s = size as f32;
    if let Some(c) = PathBuilder::from_circle(s / 2.0, s / 2.0, s / 2.0) {
        pm.fill_path(
            &c,
            &paint(30, 215, 96),
            FillRule::Winding,
            Transform::identity(),
            None,
        );
    }
    let w = (s / 11.0).max(2.0);
    let k = paint(0, 0, 0);
    arc(
        &mut pm,
        s * 0.20,
        s * 0.20,
        s * 0.80,
        s * 0.78,
        215.0,
        325.0,
        w,
        &k,
    );
    arc(
        &mut pm,
        s * 0.25,
        s * 0.33,
        s * 0.75,
        s * 0.82,
        218.0,
        322.0,
        w * 0.85,
        &k,
    );
    arc(
        &mut pm,
        s * 0.30,
        s * 0.46,
        s * 0.70,
        s * 0.86,
        222.0,
        318.0,
        w * 0.75,
        &k,
    );
    pm
}

fn tidal(size: u32) -> Pixmap {
    let mut pm = Pixmap::new(size, size).unwrap();
    let s = size as f32;
    rrect(&mut pm, 0.0, 0.0, s, s, s * 0.29, &paint(0, 0, 0));
    let u = s / 34.0;
    let r = 3.6 * u;
    let white = paint(255, 255, 255);
    for (cx, cy) in [(9.8, 13.4), (17.0, 13.4), (24.2, 13.4), (17.0, 20.6)] {
        let (cx, cy) = (cx * u, cy * u);
        let mut pb = PathBuilder::new();
        pb.move_to(cx, cy - r);
        pb.line_to(cx + r, cy);
        pb.line_to(cx, cy + r);
        pb.line_to(cx - r, cy);
        pb.close();
        if let Some(p) = pb.finish() {
            pm.fill_path(&p, &white, FillRule::Winding, Transform::identity(), None);
        }
    }
    pm
}

fn note(pm: &mut Pixmap, s: f32, white: &Paint) {
    let u = s / 34.0;
    let ell = |pm: &mut Pixmap, x0: f32, y0: f32, x1: f32, y1: f32| {
        if let Some(r) = tiny_skia::Rect::from_ltrb(x0 * u, y0 * u, x1 * u, y1 * u)
            && let Some(p) = PathBuilder::from_oval(r)
        {
            pm.fill_path(&p, white, FillRule::Winding, Transform::identity(), None);
        }
    };
    ell(pm, 9.0, 19.5, 16.0, 25.5);
    ell(pm, 19.0, 16.5, 26.0, 22.5);
    let mut pb = PathBuilder::new();
    pb.move_to(14.0 * u, 22.0 * u);
    pb.line_to(14.0 * u, 10.5 * u);
    pb.line_to(26.0 * u, 7.5 * u);
    pb.line_to(26.0 * u, 19.0 * u);
    pb.line_to(24.0 * u, 19.0 * u);
    pb.line_to(24.0 * u, 11.0 * u);
    pb.line_to(16.0 * u, 13.0 * u);
    pb.line_to(16.0 * u, 22.0 * u);
    pb.close();
    if let Some(p) = pb.finish() {
        pm.fill_path(&p, white, FillRule::Winding, Transform::identity(), None);
    }
}

fn apple(size: u32) -> Pixmap {
    let mut pm = Pixmap::new(size, size).unwrap();
    let s = size as f32;
    rrect(&mut pm, 0.0, 0.0, s, s, s * 0.29, &paint(250, 45, 72));
    note(&mut pm, s, &paint(255, 255, 255));
    pm
}

fn generic(size: u32) -> Pixmap {
    let mut pm = Pixmap::new(size, size).unwrap();
    let s = size as f32;
    rrect(&mut pm, 0.0, 0.0, s, s, s * 0.29, &paint(24, 24, 30));
    note(&mut pm, s, &paint(220, 220, 228));
    pm
}

pub fn app_icon_pixmap(icon: &AppIcon, size: u32) -> Pixmap {
    match icon {
        AppIcon::Spotify => spotify(size),
        AppIcon::Tidal => tidal(size),
        AppIcon::AppleMusic => apple(size),
        AppIcon::Generic => generic(size),
        AppIcon::Extracted(img) => {
            let src = image::imageops::resize(
                img.as_ref(),
                size,
                size,
                image::imageops::FilterType::CatmullRom,
            );
            pixmap_from_rgba(&src).unwrap_or_else(|| generic(size))
        }
    }
}

/// The Vdisplay mark: a crimson "V" on an OLED-black rounded tile. Straight RGBA.
pub fn logo_rgba(size: u32) -> Vec<u8> {
    let mut pm = Pixmap::new(size, size).unwrap();
    let s = size as f32;
    rrect(
        &mut pm,
        s * 0.03,
        s * 0.03,
        s * 0.97,
        s * 0.97,
        s * 0.25,
        &paint(10, 10, 12),
    );
    if let Some(p) = rrect_path(
        s * 0.03 + 1.0,
        s * 0.03 + 1.0,
        s * 0.97 - 1.0,
        s * 0.97 - 1.0,
        s * 0.25,
    ) {
        let st = Stroke {
            width: (s / 24.0).max(1.0),
            ..Default::default()
        };
        pm.stroke_path(&p, &paint(255, 45, 70), &st, Transform::identity(), None);
    }
    let mut pb = PathBuilder::new();
    pb.move_to(s * 0.30, s * 0.32);
    pb.line_to(s * 0.50, s * 0.70);
    pb.line_to(s * 0.70, s * 0.32);
    if let Some(p) = pb.finish() {
        let st = Stroke {
            width: s * 0.09,
            line_cap: LineCap::Round,
            line_join: tiny_skia::LineJoin::Round,
            ..Default::default()
        };
        pm.stroke_path(&p, &paint(255, 45, 70), &st, Transform::identity(), None);
    }
    // Un-premultiply for consumers that want straight alpha.
    let mut out = pm.take();
    for px in out.chunks_exact_mut(4) {
        let a = px[3] as u32;
        if a > 0 && a < 255 {
            for c in &mut px[..3] {
                *c = ((*c as u32 * 255 + a / 2) / a).min(255) as u8;
            }
        }
    }
    out
}
