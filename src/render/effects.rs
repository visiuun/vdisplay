//! Pixel effects: Kawarp fluid background, palette extraction, gradients, color calibration.

use super::canvas::Rgb;
use image::RgbImage;

pub const DEFAULT_BG: Rgb = (8, 8, 12);
pub const DEFAULT_ACCENT: Rgb = (56, 189, 248);

/// Dominant accent + deep OLED ambient tone taken strictly from the artwork.
pub fn extract_palette(img: &RgbImage) -> (Rgb, Rgb) {
    let small = image::imageops::resize(img, 32, 32, image::imageops::FilterType::Triangle);
    let mut best: Option<(f32, Rgb)> = None;
    for p in small.pixels() {
        let [r, g, b] = p.0;
        let mx = r.max(g).max(b) as f32;
        let mn = r.min(g).min(b) as f32;
        let sat = (mx - mn) / mx.max(1.0);
        let lum = (r as f32 * 299.0 + g as f32 * 587.0 + b as f32 * 114.0) / 1000.0;
        let score = if sat > 0.12 && lum > 35.0 && lum < 225.0 {
            sat * (0.6 + 0.4 * (lum / 255.0))
        } else if lum > 50.0 {
            0.05
        } else {
            continue;
        };
        if best.is_none_or(|(s, _)| score > s) {
            best = Some((score, (r, g, b)));
        }
    }
    let accent = best.map(|b| b.1).unwrap_or(DEFAULT_ACCENT);
    let bg = (
        ((accent.0 as f32 * 0.10) as u8).clamp(6, 18),
        ((accent.1 as f32 * 0.10) as u8).clamp(6, 18),
        ((accent.2 as f32 * 0.10) as u8).clamp(8, 22),
    );
    (bg, accent)
}

fn clamp8(v: f32) -> u8 {
    v.round().clamp(0.0, 255.0) as u8
}

/// The Kawarp base: artwork downsampled once per track with saturation/contrast/brightness
/// tuning.
pub fn kawarp_base(art: &RgbImage) -> RgbImage {
    let mut img = image::imageops::resize(art, 140, 140, image::imageops::FilterType::CatmullRom);
    // Color (saturation) 1.65
    for p in img.pixels_mut() {
        let [r, g, b] = p.0.map(|v| v as f32);
        let l = 0.299 * r + 0.587 * g + 0.114 * b;
        p.0 = [
            clamp8(l + (r - l) * 1.65),
            clamp8(l + (g - l) * 1.65),
            clamp8(l + (b - l) * 1.65),
        ];
    }
    // Contrast 1.28 around the mean luminance
    let n = (img.width() * img.height()) as f32;
    let mean = img
        .pixels()
        .map(|p| 0.299 * p[0] as f32 + 0.587 * p[1] as f32 + 0.114 * p[2] as f32)
        .sum::<f32>()
        / n;
    let mean = mean.round();
    for p in img.pixels_mut() {
        p.0 = p.0.map(|v| clamp8(mean + (v as f32 - mean) * 1.28));
    }
    // Brightness 0.68
    for p in img.pixels_mut() {
        p.0 = p.0.map(|v| clamp8(v as f32 * 0.68));
    }
    img
}

/// Bilinear sample with edge clamping, pixel-centre convention.
#[inline]
fn sample(src: &[u8], w: i32, h: i32, x: f32, y: f32) -> [f32; 3] {
    let x = x - 0.5;
    let y = y - 0.5;
    let x0 = x.floor();
    let y0 = y.floor();
    let fx = x - x0;
    let fy = y - y0;
    let cx = |v: i32| v.clamp(0, w - 1);
    let cy = |v: i32| v.clamp(0, h - 1);
    let (xa, xb, ya, yb) = (
        cx(x0 as i32),
        cx(x0 as i32 + 1),
        cy(y0 as i32),
        cy(y0 as i32 + 1),
    );
    let px = |xx: i32, yy: i32| {
        let i = ((yy * w + xx) * 3) as usize;
        [src[i] as f32, src[i + 1] as f32, src[i + 2] as f32]
    };
    let (a, b, c, d) = (px(xa, ya), px(xb, ya), px(xa, yb), px(xb, yb));
    let mut o = [0.0; 3];
    for k in 0..3 {
        let top = a[k] + (b[k] - a[k]) * fx;
        let bot = c[k] + (d[k] - c[k]) * fx;
        o[k] = top + (bot - top) * fy;
    }
    o
}

/// Quad transform: maps the output rectangle onto a source quad
/// (upper-left, lower-left, lower-right, upper-right).
fn quad_warp(src: &RgbImage, flip: bool, q: [f32; 8], ow: usize, oh: usize, out: &mut [f32]) {
    let (w, h) = (src.width() as i32, src.height() as i32);
    let s = src.as_raw();
    let (x0, y0, x1, y1, x2, y2, x3, y3) = (q[0], q[1], q[2], q[3], q[4], q[5], q[6], q[7]);
    let (fw, fh) = (ow as f32, oh as f32);
    for oy in 0..oh {
        let v = (oy as f32 + 0.5) / fh;
        for ox in 0..ow {
            let u = (ox as f32 + 0.5) / fw;
            let sx = x0 + (x3 - x0) * u + (x1 - x0) * v + (x0 - x1 + x2 - x3) * u * v;
            let sy = y0 + (y3 - y0) * u + (y1 - y0) * v + (y0 - y1 + y2 - y3) * u * v;
            let sx = if flip { w as f32 - sx } else { sx };
            let p = sample(s, w, h, sx, sy);
            let i = (oy * ow + ox) * 3;
            out[i..i + 3].copy_from_slice(&p);
        }
    }
}

/// Separable box blur with edge extension .
fn box_blur(buf: &mut [f32], w: usize, h: usize, r: usize, tmp: &mut Vec<f32>) {
    tmp.resize(buf.len(), 0.0);
    let norm = 1.0 / (2 * r + 1) as f32;
    for y in 0..h {
        for k in 0..3 {
            let at = |x: isize| buf[(y * w + x.clamp(0, w as isize - 1) as usize) * 3 + k];
            let mut acc: f32 = (-(r as isize)..=r as isize).map(at).sum();
            for x in 0..w {
                tmp[(y * w + x) * 3 + k] = acc * norm;
                acc += at(x as isize + r as isize + 1) - at(x as isize - r as isize);
            }
        }
    }
    for x in 0..w {
        for k in 0..3 {
            let at = |y: isize| tmp[(y.clamp(0, h as isize - 1) as usize * w + x) * 3 + k];
            let mut acc: f32 = (-(r as isize)..=r as isize).map(at).sum();
            for y in 0..h {
                buf[(y * w + x) * 3 + k] = acc * norm;
                acc += at(y as isize + r as isize + 1) - at(y as isize - r as isize);
            }
        }
    }
}

pub struct Kawarp {
    a: Vec<f32>,
    b: Vec<f32>,
    tmp: Vec<f32>,
}

const GW: usize = 64;
const GH: usize = 170;

impl Kawarp {
    pub fn new() -> Self {
        Self {
            a: vec![0.0; GW * GH * 3],
            b: vec![0.0; GW * GH * 3],
            tmp: Vec::new(),
        }
    }

    /// Domain-warped, Kawase-diffused liquid background written straight into an RGBA canvas.
    pub fn render(
        &mut self,
        base: &RgbImage,
        t: f64,
        out: &mut [u8],
        ow: usize,
        oh: usize,
        dim: f32,
    ) {
        let t = (t * 0.95) as f32;
        let (w, h) = (base.width() as f32, base.height() as f32);
        let (s, c) = (f32::sin, f32::cos);
        let q1 = [
            w * (0.12 + 0.22 * s(t * 1.10)),
            h * (0.08 + 0.16 * c(t * 0.85)),
            w * (0.16 + 0.24 * c(t * 0.95 + 1.2)),
            h * (0.92 - 0.15 * s(t * 0.75 + 0.4)),
            w * (0.88 - 0.22 * c(t * 1.15 + 1.8)),
            h * (0.90 - 0.18 * s(t * 0.90 + 1.0)),
            w * (0.84 - 0.24 * s(t * 0.80 + 2.2)),
            h * (0.10 + 0.16 * c(t * 1.05 + 0.6)),
        ];
        let q2 = [
            w * (0.86 - 0.24 * c(t * 1.05 + 1.5)),
            h * (0.14 + 0.18 * s(t * 0.80 + 1.1)),
            w * (0.82 - 0.20 * s(t * 0.90 + 2.4)),
            h * (0.86 - 0.16 * c(t * 1.10 + 0.7)),
            w * (0.16 + 0.22 * c(t * 1.20 + 2.8)),
            h * (0.88 - 0.18 * s(t * 0.85 + 1.9)),
            w * (0.14 + 0.20 * s(t * 0.75 + 0.8)),
            h * (0.12 + 0.16 * c(t * 0.95 + 2.5)),
        ];
        quad_warp(base, false, q1, GW, GH, &mut self.a);
        quad_warp(base, true, q2, GW, GH, &mut self.b);
        let wb = 0.50 + 0.22 * s(t * 0.70);
        for (x, y) in self.a.iter_mut().zip(self.b.iter()) {
            *x = *x * (1.0 - wb) + *y * wb;
        }
        box_blur(&mut self.a, GW, GH, 7, &mut self.tmp);
        box_blur(&mut self.a, GW, GH, 6, &mut self.tmp);
        upscale_into(&self.a, GW, GH, out, ow, oh, dim);
    }
}

/// Row brightness: `dim` overall, fading a further 35% towards the bottom edge so the
/// progress bar and pill always sit on near-black (OLED friendly, better contrast).
#[inline]
fn row_gain(y: usize, oh: usize, dim: f32) -> f32 {
    let t = y as f32 / oh as f32;
    let bottom = ((t - 0.62) / 0.38).clamp(0.0, 1.0);
    let top = ((0.08 - t) / 0.08).clamp(0.0, 1.0);
    dim * (1.0 - 0.35 * bottom * bottom) * (1.0 - 0.2 * top)
}

/// Bilinear upscale of a small float RGB buffer into an RGBA canvas, with a tiny ordered
/// dither so the smooth gradients don't band after JPEG compression.
fn upscale_into(src: &[f32], sw: usize, sh: usize, out: &mut [u8], ow: usize, oh: usize, dim: f32) {
    const DITHER: [f32; 16] = [
        0.0, 8.0, 2.0, 10.0, 12.0, 4.0, 14.0, 6.0, 3.0, 11.0, 1.0, 9.0, 15.0, 7.0, 13.0, 5.0,
    ];
    let sx = sw as f32 / ow as f32;
    let sy = sh as f32 / oh as f32;
    let taps: Vec<(usize, usize, f32)> = (0..ow)
        .map(|x| {
            let fx = ((x as f32 + 0.5) * sx - 0.5).max(0.0);
            let x0 = (fx.floor() as usize).min(sw - 1);
            let x1 = (x0 + 1).min(sw - 1);
            (x0 * 3, x1 * 3, fx - x0 as f32)
        })
        .collect();
    let mut row = vec![0.0f32; sw * 3];
    for y in 0..oh {
        let fy = ((y as f32 + 0.5) * sy - 0.5).max(0.0);
        let y0 = (fy.floor() as usize).min(sh - 1);
        let y1 = (y0 + 1).min(sh - 1);
        let wy = fy - y0 as f32;
        let gain = row_gain(y, oh, dim);
        // Vertical pass once per output row (gain folded in), then a cheap horizontal lerp.
        let (r0, r1) = (
            &src[y0 * sw * 3..(y0 + 1) * sw * 3],
            &src[y1 * sw * 3..(y1 + 1) * sw * 3],
        );
        for i in 0..sw * 3 {
            row[i] = (r0[i] + (r1[i] - r0[i]) * wy) * gain;
        }
        let drow = &DITHER[(y & 3) * 4..(y & 3) * 4 + 4];
        let orow = &mut out[y * ow * 4..(y + 1) * ow * 4];
        for (x, (o, &(a, b, wx))) in orow.chunks_exact_mut(4).zip(taps.iter()).enumerate() {
            let d = drow[x & 3] * (1.0 / 16.0) + 0.03; // +0.5 rounding −0.47 dither centre
            o[0] = (row[a] + (row[b] - row[a]) * wx + d) as u8;
            o[1] = (row[a + 1] + (row[b + 1] - row[a + 1]) * wx + d) as u8;
            o[2] = (row[a + 2] + (row[b + 2] - row[a + 2]) * wx + d) as u8;
            o[3] = 255;
        }
    }
}

/// Fallback when there's no artwork: blurred accent orb on deep black.
pub fn orb_background(accent: Rgb, out: &mut [u8], ow: usize, oh: usize) {
    let mut buf = vec![0.0f32; GW * GH * 3];
    let (cx, cy, r) = (GW as f32 * 0.5, GH as f32 * 0.4, GW as f32 * 0.45);
    for y in 0..GH {
        for x in 0..GW {
            let dx = x as f32 + 0.5 - cx;
            let dy = y as f32 + 0.5 - cy;
            let inside = dx * dx + dy * dy <= r * r;
            let c = if inside {
                [accent.0, accent.1, accent.2].map(|v| v as f32)
            } else {
                [8.0, 8.0, 14.0]
            };
            buf[(y * GW + x) * 3..(y * GW + x) * 3 + 3].copy_from_slice(&c);
        }
    }
    let mut tmp = Vec::new();
    box_blur(&mut buf, GW, GH, 12, &mut tmp);
    upscale_into(&buf, GW, GH, out, ow, oh, 0.85);
}

/// Minimal style: the ambient tone fading to pure black (fully black below ~92%).
pub fn gradient_background(bg: Rgb, out: &mut [u8], ow: usize, oh: usize) {
    for y in 0..oh {
        let f = (1.0 - (y as f32 / oh as f32 * 120.0) / 110.0).max(0.0);
        let px = [
            (bg.0 as f32 * f) as u8,
            (bg.1 as f32 * f) as u8,
            (bg.2 as f32 * f) as u8,
            255,
        ];
        for x in 0..ow {
            out[(y * ow + x) * 4..(y * ow + x) * 4 + 4].copy_from_slice(&px);
        }
    }
}

// Color calibration: one 3×3 matrix (hue rotation + saturation) and a brightness gain,
// applied in a single pass (~1 ms per frame).

pub struct Calibration {
    m: [[f32; 3]; 3],
    identity: bool,
}

impl Calibration {
    pub fn new(brightness: i32, saturation: i32, hue: i32) -> Self {
        if brightness == 100 && saturation == 100 && hue == 0 {
            return Self {
                m: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
                identity: true,
            };
        }
        let (lr, lg, lb) = (0.213f32, 0.715f32, 0.072f32);
        // Hue rotation matrix (luminance preserving, as used by CSS/SVG feColorMatrix).
        let a = (hue as f32).to_radians();
        let (cs, sn) = (a.cos(), a.sin());
        let hue_m = [
            [
                lr + cs * (1.0 - lr) + sn * (-lr),
                lg + cs * (-lg) + sn * (-lg),
                lb + cs * (-lb) + sn * (1.0 - lb),
            ],
            [
                lr + cs * (-lr) + sn * 0.143,
                lg + cs * (1.0 - lg) + sn * 0.140,
                lb + cs * (-lb) + sn * (-0.283),
            ],
            [
                lr + cs * (-lr) + sn * (-(1.0 - lr)),
                lg + cs * (-lg) + sn * lg,
                lb + cs * (1.0 - lb) + sn * lb,
            ],
        ];
        let s = (saturation as f32 / 100.0).max(0.0);
        let sat_m = [
            [lr + (1.0 - lr) * s, lg - lg * s, lb - lb * s],
            [lr - lr * s, lg + (1.0 - lg) * s, lb - lb * s],
            [lr - lr * s, lg - lg * s, lb + (1.0 - lb) * s],
        ];
        let gain = (brightness as f32 / 100.0).max(0.1);
        let mut m = [[0.0f32; 3]; 3];
        for i in 0..3 {
            for j in 0..3 {
                m[i][j] = (0..3).map(|k| sat_m[i][k] * hue_m[k][j]).sum::<f32>() * gain;
            }
        }
        Self { m, identity: false }
    }

    pub fn is_identity(&self) -> bool {
        self.identity
    }

    /// In-place on an opaque RGBA buffer.
    pub fn apply_rgba(&self, buf: &mut [u8]) {
        if self.identity {
            return;
        }
        let m = self.m;
        for p in buf.chunks_exact_mut(4) {
            let (r, g, b) = (p[0] as f32, p[1] as f32, p[2] as f32);
            p[0] = clamp8(m[0][0] * r + m[0][1] * g + m[0][2] * b);
            p[1] = clamp8(m[1][0] * r + m[1][1] * g + m[1][2] * b);
            p[2] = clamp8(m[2][0] * r + m[2][1] * g + m[2][2] * b);
        }
    }
}
