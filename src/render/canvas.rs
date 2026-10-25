//! Anti-aliased drawing surface for the 360×960 device frames.
//!
//! Boxes are inclusive (`[x0, y0, x1, y1]`).

use super::text::{Anchor, Face, Fonts};
use tiny_skia::{
    Color, FillRule, FilterQuality, Paint, Path, PathBuilder, Pattern, Pixmap, Rect, SpreadMode,
    Stroke, Transform,
};

pub type Rgb = (u8, u8, u8);

pub fn c(rgb: Rgb) -> [u8; 4] {
    [rgb.0, rgb.1, rgb.2, 255]
}

pub fn ca(rgb: Rgb, a: u8) -> [u8; 4] {
    [rgb.0, rgb.1, rgb.2, a]
}

pub fn hex(h: u32) -> [u8; 4] {
    [(h >> 16) as u8, (h >> 8) as u8, h as u8, 255]
}

fn color(c: [u8; 4]) -> Color {
    Color::from_rgba8(c[0], c[1], c[2], c[3])
}

pub fn rrect_path(x0: f32, y0: f32, x1: f32, y1: f32, r: f32) -> Option<Path> {
    if x1 <= x0 || y1 <= y0 {
        return None;
    }
    let r = r.min((x1 - x0) / 2.0).min((y1 - y0) / 2.0).max(0.0);
    if r <= 0.01 {
        return Some(PathBuilder::from_rect(Rect::from_ltrb(x0, y0, x1, y1)?));
    }
    const K: f32 = 0.552_284_8;
    let k = r * K;
    let mut pb = PathBuilder::new();
    pb.move_to(x0 + r, y0);
    pb.line_to(x1 - r, y0);
    pb.cubic_to(x1 - r + k, y0, x1, y0 + r - k, x1, y0 + r);
    pb.line_to(x1, y1 - r);
    pb.cubic_to(x1, y1 - r + k, x1 - r + k, y1, x1 - r, y1);
    pb.line_to(x0 + r, y1);
    pb.cubic_to(x0 + r - k, y1, x0, y1 - r + k, x0, y1 - r);
    pb.line_to(x0, y0 + r);
    pb.cubic_to(x0, y0 + r - k, x0 + r - k, y0, x0 + r, y0);
    pb.close();
    pb.finish()
}

pub struct Canvas {
    pub pm: Pixmap,
}

impl Canvas {
    pub fn new(w: u32, h: u32) -> Self {
        Self {
            pm: Pixmap::new(w, h).expect("canvas size"),
        }
    }

    pub fn w(&self) -> i32 {
        self.pm.width() as i32
    }

    pub fn h(&self) -> i32 {
        self.pm.height() as i32
    }

    pub fn clear(&mut self, rgb: Rgb) {
        self.pm.fill(Color::from_rgba8(rgb.0, rgb.1, rgb.2, 255));
    }

    /// Copies a full-frame tightly packed RGB buffer in.
    pub fn put_rgb(&mut self, rgb: &[u8]) {
        for (d, s) in self
            .pm
            .data_mut()
            .chunks_exact_mut(4)
            .zip(rgb.chunks_exact(3))
        {
            d[0] = s[0];
            d[1] = s[1];
            d[2] = s[2];
            d[3] = 255;
        }
    }

    /// Exports as tightly packed RGB (the canvas is always opaque, so premultiplied == straight).
    pub fn to_rgb(&self, out: &mut Vec<u8>) {
        out.clear();
        out.reserve(self.pm.data().len() / 4 * 3);
        for p in self.pm.data().chunks_exact(4) {
            out.extend_from_slice(&p[..3]);
        }
    }

    fn paint(c: [u8; 4]) -> Paint<'static> {
        let mut p = Paint::default();
        p.set_color(color(c));
        p.anti_alias = true;
        p
    }

    /// Rounded rectangle over the inclusive box with optional fill and outline.
    #[allow(clippy::too_many_arguments)]
    pub fn rrect(
        &mut self,
        x0: f32,
        y0: f32,
        x1: f32,
        y1: f32,
        r: f32,
        fill: Option<[u8; 4]>,
        outline: Option<[u8; 4]>,
        width: f32,
    ) {
        let (x1, y1) = (x1 + 1.0, y1 + 1.0);
        if let Some(f) = fill
            && let Some(p) = rrect_path(x0, y0, x1, y1, r)
        {
            self.pm.fill_path(
                &p,
                &Self::paint(f),
                FillRule::Winding,
                Transform::identity(),
                None,
            );
        }
        if let Some(o) = outline {
            let h = width / 2.0;
            if let Some(p) = rrect_path(x0 + h, y0 + h, x1 - h, y1 - h, (r - h).max(0.0)) {
                let s = Stroke {
                    width,
                    ..Default::default()
                };
                self.pm
                    .stroke_path(&p, &Self::paint(o), &s, Transform::identity(), None);
            }
        }
    }

    pub fn fill_rrect(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, r: f32, fill: [u8; 4]) {
        self.rrect(x0, y0, x1, y1, r, Some(fill), None, 0.0);
    }

    pub fn circle(&mut self, cx: f32, cy: f32, r: f32, fill: [u8; 4]) {
        if let Some(p) = PathBuilder::from_circle(cx, cy, r) {
            self.pm.fill_path(
                &p,
                &Self::paint(fill),
                FillRule::Winding,
                Transform::identity(),
                None,
            );
        }
    }

    pub fn hline(&mut self, x0: f32, x1: f32, y: f32, col: [u8; 4]) {
        if let Some(r) = Rect::from_xywh(x0, y, x1 - x0, 1.0) {
            self.pm
                .fill_rect(r, &Self::paint(col), Transform::identity(), None);
        }
    }

    /// Draws `img` scaled into the box with rounded corners.
    pub fn image_rounded(&mut self, img: &Pixmap, x: f32, y: f32, w: f32, h: f32, r: f32) {
        let Some(path) = rrect_path(x, y, x + w, y + h, r) else {
            return;
        };
        let sx = w / img.width() as f32;
        let sy = h / img.height() as f32;
        let quality = if (sx - 1.0).abs() < 0.001 && (sy - 1.0).abs() < 0.001 {
            FilterQuality::Nearest
        } else {
            FilterQuality::Bicubic
        };
        let shader = Pattern::new(
            img.as_ref(),
            SpreadMode::Pad,
            quality,
            1.0,
            Transform::from_row(sx, 0.0, 0.0, sy, x, y),
        );
        let paint = Paint {
            shader,
            anti_alias: true,
            ..Default::default()
        };
        self.pm.fill_path(
            &path,
            &paint,
            FillRule::Winding,
            Transform::identity(),
            None,
        );
    }

    /// Fast premultiplied source-over blit (no scaling), clipped to the canvas.
    pub fn blit(&mut self, src: &Pixmap, x: i32, y: i32) {
        let (cw, ch) = (self.w(), self.h());
        let (sw, sh) = (src.width() as i32, src.height() as i32);
        let sd = src.data();
        let dd = self.pm.data_mut();
        for sy in 0..sh {
            let dy = y + sy;
            if dy < 0 || dy >= ch {
                continue;
            }
            let x0 = (-x).max(0);
            let x1 = sw.min(cw - x);
            if x1 <= x0 {
                continue;
            }
            let srow = &sd[(sy * sw * 4) as usize..((sy + 1) * sw * 4) as usize];
            let drow = &mut dd[(dy * cw * 4) as usize..((dy + 1) * cw * 4) as usize];
            for sx in x0..x1 {
                let si = (sx * 4) as usize;
                let a = srow[si + 3] as u32;
                if a == 0 {
                    continue;
                }
                let di = ((x + sx) * 4) as usize;
                if a == 255 {
                    drow[di..di + 3].copy_from_slice(&srow[si..si + 3]);
                } else {
                    let ia = 255 - a;
                    for k in 0..3 {
                        drow[di + k] = (srow[si + k] as u32
                            + (drow[di + k] as u32 * ia + 127) / 255)
                            .min(255) as u8;
                    }
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn text(
        &mut self,
        f: &mut Fonts,
        s: &str,
        face: Face,
        size: f32,
        x: f32,
        y: f32,
        anchor: Anchor,
        col: [u8; 4],
    ) -> f32 {
        let (w, h) = (self.w(), self.h());
        f.draw(
            self.pm.data_mut(),
            w,
            h,
            s,
            face,
            size,
            x,
            y,
            anchor,
            col,
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn text_clipped(
        &mut self,
        f: &mut Fonts,
        s: &str,
        face: Face,
        size: f32,
        x: f32,
        y: f32,
        col: [u8; 4],
        clip: (f32, f32),
    ) {
        let (w, h) = (self.w(), self.h());
        f.draw(
            self.pm.data_mut(),
            w,
            h,
            s,
            face,
            size,
            x,
            y,
            Anchor::LeftTop,
            col,
            Some(clip),
        );
    }
}

/// Converts an RGB image into a premultiplied pixmap.
pub fn pixmap_from_rgb(img: &image::RgbImage) -> Option<Pixmap> {
    let mut pm = Pixmap::new(img.width(), img.height())?;
    for (d, s) in pm
        .data_mut()
        .chunks_exact_mut(4)
        .zip(img.as_raw().chunks_exact(3))
    {
        d[..3].copy_from_slice(s);
        d[3] = 255;
    }
    Some(pm)
}

pub fn pixmap_from_rgba(img: &image::RgbaImage) -> Option<Pixmap> {
    let mut pm = Pixmap::new(img.width(), img.height())?;
    for (d, s) in pm
        .data_mut()
        .chunks_exact_mut(4)
        .zip(img.as_raw().chunks_exact(4))
    {
        let a = s[3] as u32;
        d[0] = (s[0] as u32 * a / 255) as u8;
        d[1] = (s[1] as u32 * a / 255) as u8;
        d[2] = (s[2] as u32 * a / 255) as u8;
        d[3] = s[3];
    }
    Some(pm)
}

/// Pre-composites rounded artwork (optional soft glow + border) into a transparent sprite.
/// Returns the sprite and the padding around the artwork.
pub fn art_sprite(
    art: &Pixmap,
    radius: f32,
    border: Option<([u8; 4], f32)>,
    glow: Option<Rgb>,
) -> (Pixmap, i32) {
    let size = art.width() as f32;
    let pad = if glow.is_some() { 22 } else { 1 };
    let dim = art.width() + pad as u32 * 2;
    let mut pm = Pixmap::new(dim, dim + if glow.is_some() { 8 } else { 0 }).expect("sprite");
    let p = pad as f32;
    if let Some(g) = glow {
        for i in 0..7u8 {
            let e = 18.0 - i as f32 * 2.5;
            if let Some(path) = rrect_path(
                p - e,
                p - e + 7.0,
                p + size + e,
                p + size + e + 7.0,
                radius + e,
            ) {
                let mut paint = Paint::default();
                paint.set_color(Color::from_rgba8(g.0, g.1, g.2, 5 + i * 3));
                paint.anti_alias = true;
                pm.fill_path(
                    &path,
                    &paint,
                    FillRule::Winding,
                    Transform::identity(),
                    None,
                );
            }
        }
        // Keep the area under the cover opaque black so the glow doesn't tint the art edges.
        if let Some(path) = rrect_path(p, p, p + size, p + size, radius) {
            let mut paint = Paint::default();
            paint.set_color(Color::BLACK);
            paint.anti_alias = true;
            pm.fill_path(
                &path,
                &paint,
                FillRule::Winding,
                Transform::identity(),
                None,
            );
        }
    }
    if let Some(path) = rrect_path(p, p, p + size, p + size, radius) {
        let shader = Pattern::new(
            art.as_ref(),
            SpreadMode::Pad,
            FilterQuality::Nearest,
            1.0,
            Transform::from_translate(p, p),
        );
        let paint = Paint {
            shader,
            anti_alias: true,
            ..Default::default()
        };
        pm.fill_path(
            &path,
            &paint,
            FillRule::Winding,
            Transform::identity(),
            None,
        );
    }
    if let Some((col, w)) = border {
        let h = w / 2.0;
        if let Some(path) = rrect_path(p + h, p + h, p + size - h, p + size - h, radius - h) {
            let mut paint = Paint::default();
            paint.set_color(Color::from_rgba8(col[0], col[1], col[2], col[3]));
            paint.anti_alias = true;
            pm.stroke_path(
                &path,
                &paint,
                &Stroke {
                    width: w,
                    ..Default::default()
                },
                Transform::identity(),
                None,
            );
        }
    }
    (pm, pad)
}
