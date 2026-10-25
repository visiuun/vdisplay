//! Text rendering for the device frames: Windows system fonts via ab_glyph, a per-glyph
//! fallback chain (Segoe UI, Segoe UI Symbol, CJK) and a coverage-bitmap glyph cache.

use ab_glyph::{Font, FontVec, GlyphId, PxScale, ScaleFont, point};
use std::collections::HashMap;
use std::rc::Rc;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Face {
    Regular,
    Semibold,
    Bold,
    Light,
}

#[derive(Clone, Copy)]
pub enum Anchor {
    /// x = left edge, y = top (ascender line).
    LeftTop,
    /// x = right edge, y = top.
    RightTop,
    /// Centered on (x, y).
    Center,
}

struct Glyph {
    left: i32,
    top: i32,
    w: i32,
    h: i32,
    cov: Vec<u8>,
}

struct LoadedFont {
    font: FontVec,
    em: f32, // height_unscaled / units_per_em, converts an em size to ab_glyph's PxScale
}

const FONT_DIR: &str = r"C:\Windows\Fonts";

fn load(name: &str, index: u32) -> Option<LoadedFont> {
    let data = std::fs::read(format!(r"{FONT_DIR}\{name}")).ok()?;
    let font = FontVec::try_from_vec_and_index(data, index).ok()?;
    let em = font.height_unscaled() / font.units_per_em().unwrap_or(1000.0);
    Some(LoadedFont { font, em })
}

fn load_first(names: &[&str]) -> Option<LoadedFont> {
    names.iter().find_map(|n| load(n, 0))
}

pub struct Fonts {
    fonts: Vec<LoadedFont>,
    primary: [usize; 4],
    symbol: Option<usize>,
    cjk: [Option<usize>; 4],
    cjk_tried: [bool; 4],
    glyph_cache: HashMap<(u8, u16, u16, u8), Option<Rc<Glyph>>>,
    char_cache: HashMap<(Face, char), (u8, GlyphId)>,
}

fn face_idx(f: Face) -> usize {
    match f {
        Face::Regular => 0,
        Face::Semibold => 1,
        Face::Bold => 2,
        Face::Light => 3,
    }
}

impl Fonts {
    pub fn new() -> Self {
        let mut fonts = Vec::new();
        let push = |f: Option<LoadedFont>, fonts: &mut Vec<LoadedFont>| -> Option<usize> {
            f.map(|f| {
                fonts.push(f);
                fonts.len() - 1
            })
        };
        let regular = push(
            load_first(&["segoeui.ttf", "arial.ttf", "tahoma.ttf"]),
            &mut fonts,
        );
        let bold = push(
            load_first(&["segoeuib.ttf", "arialbd.ttf", "tahomabd.ttf"]),
            &mut fonts,
        );
        let semibold = push(load_first(&["seguisb.ttf"]), &mut fonts);
        let light = push(load_first(&["segoeuil.ttf", "segoeuisl.ttf"]), &mut fonts);
        let symbol = push(load_first(&["seguisym.ttf"]), &mut fonts);
        let regular = regular.or(bold).or(symbol);
        let bold = bold.or(regular);
        let semibold = semibold.or(bold);
        let Some(regular) = regular else {
            // Without any font we can still draw shapes; text calls become no-ops.
            return Self {
                fonts,
                primary: [usize::MAX; 4],
                symbol: None,
                cjk: [None; 4],
                cjk_tried: [true; 4],
                glyph_cache: HashMap::new(),
                char_cache: HashMap::new(),
            };
        };
        Self {
            fonts,
            primary: [
                regular,
                semibold.unwrap_or(regular),
                bold.unwrap_or(regular),
                light.unwrap_or(regular),
            ],
            symbol,
            cjk: [None; 4],
            cjk_tried: [false; 4],
            glyph_cache: HashMap::new(),
            char_cache: HashMap::new(),
        }
    }

    fn cjk_font(&mut self, face: Face) -> Option<usize> {
        let i = face_idx(face);
        if !self.cjk_tried[i] {
            self.cjk_tried[i] = true;
            let names: &[&str] = if matches!(face, Face::Regular | Face::Light) {
                &[
                    "YuGothM.ttc",
                    "meiryo.ttc",
                    "msyh.ttc",
                    "malgun.ttf",
                    "msgothic.ttc",
                ]
            } else {
                &[
                    "YuGothB.ttc",
                    "meiryob.ttc",
                    "msyhbd.ttc",
                    "malgunbd.ttf",
                    "YuGothM.ttc",
                    "meiryo.ttc",
                    "msyh.ttc",
                ]
            };
            if let Some(f) = load_first(names) {
                self.fonts.push(f);
                self.cjk[i] = Some(self.fonts.len() - 1);
            }
        }
        self.cjk[i]
    }

    fn resolve(&mut self, face: Face, c: char) -> Option<(u8, GlyphId)> {
        if let Some(r) = self.char_cache.get(&(face, c)) {
            return Some(*r);
        }
        let p = *self.primary.get(face_idx(face))?;
        if p == usize::MAX {
            return None;
        }
        let mut chain = vec![p];
        if let Some(s) = self.symbol {
            chain.push(s);
        }
        let mut found = None;
        for &fi in &chain {
            let g = self.fonts[fi].font.glyph_id(c);
            if g.0 != 0 {
                found = Some((fi as u8, g));
                break;
            }
        }
        if found.is_none()
            && let Some(fi) = self.cjk_font(face)
        {
            let g = self.fonts[fi].font.glyph_id(c);
            if g.0 != 0 {
                found = Some((fi as u8, g));
            }
        }
        // Fall back to the primary font's .notdef / space for unknown glyphs.
        let r = found.unwrap_or((
            p as u8,
            self.fonts[p]
                .font
                .glyph_id(if c.is_whitespace() { ' ' } else { c }),
        ));
        self.char_cache.insert((face, c), r);
        Some(r)
    }

    fn scale(&self, fi: usize, size: f32) -> PxScale {
        PxScale::from(size * self.fonts[fi].em)
    }

    /// (ascent, descent) in px for the primary font of `face` (descent is negative).
    pub fn metrics(&self, face: Face, size: f32) -> (f32, f32) {
        let p = self.primary[face_idx(face)];
        if p == usize::MAX {
            return (size, 0.0);
        }
        let f = self.fonts[p].font.as_scaled(self.scale(p, size));
        (f.ascent(), f.descent())
    }

    /// Lays out a single line: (font, glyph, x offset) and the total advance width.
    fn layout(
        &mut self,
        text: &str,
        face: Face,
        size: f32,
        out: &mut Vec<(u8, GlyphId, f32)>,
    ) -> f32 {
        out.clear();
        let mut x = 0.0f32;
        let mut prev: Option<(u8, GlyphId)> = None;
        for c in text.chars() {
            if c == '\n' || c == '\r' {
                continue;
            }
            let Some((fi, g)) = self.resolve(face, c) else {
                continue;
            };
            let sf = self.fonts[fi as usize]
                .font
                .as_scaled(self.scale(fi as usize, size));
            if let Some((pf, pg)) = prev
                && pf == fi
            {
                x += sf.kern(pg, g);
            }
            out.push((fi, g, x));
            x += sf.h_advance(g);
            prev = Some((fi, g));
        }
        x
    }

    pub fn width(&mut self, text: &str, face: Face, size: f32) -> f32 {
        let mut x = 0.0f32;
        let mut prev: Option<(u8, GlyphId)> = None;
        for c in text.chars() {
            let Some((fi, g)) = self.resolve(face, c) else {
                continue;
            };
            let sf = self.fonts[fi as usize]
                .font
                .as_scaled(self.scale(fi as usize, size));
            if let Some((pf, pg)) = prev
                && pf == fi
            {
                x += sf.kern(pg, g);
            }
            x += sf.h_advance(g);
            prev = Some((fi, g));
        }
        x
    }

    fn glyph(&mut self, fi: u8, g: GlyphId, size: f32, sub: u8) -> Option<Rc<Glyph>> {
        let key = (fi, g.0, (size * 4.0) as u16, sub);
        if let Some(c) = self.glyph_cache.get(&key) {
            return c.clone();
        }
        let f = &self.fonts[fi as usize];
        let glyph =
            g.with_scale_and_position(self.scale(fi as usize, size), point(sub as f32 / 4.0, 0.0));
        let res = f.font.outline_glyph(glyph).map(|og| {
            let b = og.px_bounds();
            let (w, h) = ((b.width() as i32).max(0), (b.height() as i32).max(0));
            let mut cov = vec![0u8; (w * h) as usize];
            og.draw(|x, y, c| {
                let i = (y as i32 * w + x as i32) as usize;
                if i < cov.len() {
                    cov[i] = (c.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
                }
            });
            Rc::new(Glyph {
                left: b.min.x as i32,
                top: b.min.y as i32,
                w,
                h,
                cov,
            })
        });
        if self.glyph_cache.len() > 6000 {
            self.glyph_cache.clear();
        }
        self.glyph_cache.insert(key, res.clone());
        res
    }

    /// Draws text into an opaque RGBA buffer. `clip` limits drawing to x in [clip.0, clip.1).
    #[allow(clippy::too_many_arguments)]
    pub fn draw(
        &mut self,
        buf: &mut [u8],
        bw: i32,
        bh: i32,
        text: &str,
        face: Face,
        size: f32,
        x: f32,
        y: f32,
        anchor: Anchor,
        color: [u8; 4],
        clip: Option<(f32, f32)>,
    ) -> f32 {
        let mut layout = Vec::with_capacity(text.len());
        let width = self.layout(text, face, size, &mut layout);
        let (asc, desc) = self.metrics(face, size);
        let (ox, baseline) = match anchor {
            Anchor::LeftTop => (x, y + asc),
            Anchor::RightTop => (x - width, y + asc),
            Anchor::Center => (x - width / 2.0, y + (asc + desc) / 2.0),
        };
        let by = baseline.round() as i32;
        let (cx0, cx1) = match clip {
            Some((a, b)) => (a.floor().max(0.0) as i32, b.ceil().min(bw as f32) as i32),
            None => (0, bw),
        };
        let ca = color[3] as u32;
        for (fi, g, gx) in layout {
            let px = ox + gx;
            let mut ix = px.floor() as i32;
            let mut sub = ((px - px.floor()) * 4.0).round() as u8;
            if sub >= 4 {
                sub = 0;
                ix += 1;
            }
            let Some(gl) = self.glyph(fi, g, size, sub) else {
                continue;
            };
            let x0 = ix + gl.left;
            let y0 = by + gl.top;
            for gy in 0..gl.h {
                let yy = y0 + gy;
                if yy < 0 || yy >= bh {
                    continue;
                }
                let row = (yy * bw) as usize * 4;
                for gxp in 0..gl.w {
                    let xx = x0 + gxp;
                    if xx < cx0 || xx >= cx1 {
                        continue;
                    }
                    let a = gl.cov[(gy * gl.w + gxp) as usize] as u32 * ca / 255;
                    if a == 0 {
                        continue;
                    }
                    let i = row + xx as usize * 4;
                    let d = &mut buf[i..i + 3];
                    for k in 0..3 {
                        let dv = d[k] as u32;
                        d[k] = ((dv * (255 - a) + color[k] as u32 * a + 127) / 255) as u8;
                    }
                }
            }
        }
        width
    }
}
