//! Loads custom images / animated GIF / animated WebP into ready-to-send 360×960 RGB frames.

use crate::render::{H, Renderer, W};
use crate::settings::MediaFit;
use image::codecs::gif::GifDecoder;
use image::codecs::webp::WebPDecoder;
use image::imageops::FilterType;
use image::{AnimationDecoder, RgbImage, RgbaImage};
use std::io::BufReader;
use std::path::Path;
use std::sync::Arc;

const MAX_FRAMES: usize = 900;

pub struct MediaFrames {
    pub name: String,
    pub frames: Vec<Arc<RgbImage>>,
    pub delays_ms: Vec<u32>,
}

impl MediaFrames {
    pub fn describe(&self) -> String {
        if self.frames.len() > 1 {
            let total: u32 = self.delays_ms.iter().sum();
            format!(
                "{} · {} frames · {:.1}s loop",
                self.name,
                self.frames.len(),
                total as f32 / 1000.0
            )
        } else {
            format!("{} · still image", self.name)
        }
    }
}

/// Composites onto black and fits into the panel according to `fit`.
fn fit_frame(src: &RgbaImage, fit: MediaFit, filter: FilterType) -> RgbImage {
    let (sw, sh) = (src.width().max(1) as f32, src.height().max(1) as f32);
    let flat = RgbImage::from_fn(src.width(), src.height(), |x, y| {
        let p = src.get_pixel(x, y).0;
        let a = p[3] as u32;
        image::Rgb([
            (p[0] as u32 * a / 255) as u8,
            (p[1] as u32 * a / 255) as u8,
            (p[2] as u32 * a / 255) as u8,
        ])
    });
    match fit {
        MediaFit::Stretch => image::imageops::resize(&flat, W, H, filter),
        MediaFit::Fill => {
            let s = (W as f32 / sw).max(H as f32 / sh);
            let (nw, nh) = (
                ((sw * s).ceil() as u32).max(W),
                ((sh * s).ceil() as u32).max(H),
            );
            let big = image::imageops::resize(&flat, nw, nh, filter);
            image::imageops::crop_imm(&big, (nw - W) / 2, (nh - H) / 2, W, H).to_image()
        }
        MediaFit::Fit => {
            let s = (W as f32 / sw).min(H as f32 / sh);
            let (nw, nh) = (
                ((sw * s).round() as u32).clamp(1, W),
                ((sh * s).round() as u32).clamp(1, H),
            );
            let small = image::imageops::resize(&flat, nw, nh, filter);
            let mut out = RgbImage::new(W, H);
            image::imageops::overlay(
                &mut out,
                &small,
                ((W - nw) / 2) as i64,
                ((H - nh) / 2) as i64,
            );
            out
        }
    }
}

/// Resizes frames on all cores.
fn fit_all(frames: Vec<RgbaImage>, fit: MediaFit) -> Vec<Arc<RgbImage>> {
    let threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .clamp(1, 8);
    let chunk = frames.len().div_ceil(threads).max(1);
    std::thread::scope(|s| {
        let handles: Vec<_> = frames
            .chunks(chunk)
            .map(|c| {
                s.spawn(move || {
                    c.iter()
                        .map(|f| Arc::new(fit_frame(f, fit, FilterType::Triangle)))
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|h| h.join().unwrap_or_default())
            .collect()
    })
}

fn delay_ms(d: image::Delay) -> u32 {
    let (n, dd) = d.numer_denom_ms();
    let ms = n.checked_div(dd).unwrap_or(100);
    // Browsers treat 0-10 ms GIF delays as 100 ms; do the same so such files don't race.
    if ms <= 10 { 100 } else { ms.min(10_000) }
}

fn animated(frames: Vec<image::Frame>, name: String, fit: MediaFit) -> MediaFrames {
    let delays = frames.iter().map(|f| delay_ms(f.delay())).collect();
    let frames = fit_all(frames.into_iter().map(|f| f.into_buffer()).collect(), fit);
    MediaFrames {
        name,
        frames,
        delays_ms: delays,
    }
}

pub fn load_path(path: &Path, fit: MediaFit) -> Result<MediaFrames, String> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let open = || {
        std::fs::File::open(path)
            .map(BufReader::new)
            .map_err(|e| e.to_string())
    };

    if ext == "gif" {
        let dec = GifDecoder::new(open()?).map_err(|e| e.to_string())?;
        let frames: Vec<_> = dec
            .into_frames()
            .take(MAX_FRAMES)
            .collect::<Result<_, _>>()
            .map_err(|e| e.to_string())?;
        if frames.len() > 1 {
            return Ok(animated(frames, name, fit));
        }
    }
    if ext == "webp" {
        let dec = WebPDecoder::new(open()?).map_err(|e| e.to_string())?;
        if dec.has_animation() {
            let frames: Vec<_> = dec
                .into_frames()
                .take(MAX_FRAMES)
                .collect::<Result<_, _>>()
                .map_err(|e| e.to_string())?;
            if frames.len() > 1 {
                return Ok(animated(frames, name, fit));
            }
        }
    }
    let img = image::open(path).map_err(|e| e.to_string())?.into_rgba8();
    let frame = fit_frame(&img, fit, FilterType::Lanczos3);
    Ok(MediaFrames {
        name,
        frames: vec![Arc::new(frame)],
        delays_ms: vec![1000],
    })
}

pub fn load_preset(name: &str) -> MediaFrames {
    let mut r = Renderer::new();
    let img = r.preset(name);
    let label = match name {
        "crimson" => "OLED Crimson",
        "black" => "Pure Black",
        _ => "Cyber Gradient",
    };
    MediaFrames {
        name: format!("Preset: {label}"),
        frames: vec![Arc::new(img)],
        delays_ms: vec![1000],
    }
}

/// Resolves a `media_path` setting (file path or `preset:<name>`).
pub fn load_source(src: &str, fit: MediaFit) -> Result<MediaFrames, String> {
    match src.strip_prefix("preset:") {
        Some(p) => Ok(load_preset(p)),
        None => load_path(Path::new(src), fit),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gif_and_fit() {
        let dir = std::env::temp_dir().join("vdisplay-loader-test");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("t.gif");
        {
            let f = std::fs::File::create(&path).unwrap();
            let mut enc = image::codecs::gif::GifEncoder::new(f);
            for (i, c) in [[255u8, 0, 0, 255], [0, 0, 255, 255]].iter().enumerate() {
                let img = RgbaImage::from_pixel(100, 50, image::Rgba(*c));
                let delay = image::Delay::from_numer_denom_ms(if i == 0 { 200 } else { 0 }, 1);
                enc.encode_frame(image::Frame::from_parts(img, 0, 0, delay))
                    .unwrap();
            }
        }
        let m = load_path(&path, MediaFit::Fit).unwrap();
        assert_eq!(m.frames.len(), 2);
        assert_eq!(m.delays_ms, vec![200, 100]); // 0 ms becomes 100 ms, like browsers
        let f = &m.frames[0];
        assert_eq!((f.width(), f.height()), (W, H));
        assert_eq!(f.get_pixel(180, 480).0, [255, 0, 0]); // centre is the image
        assert_eq!(f.get_pixel(180, 10).0, [0, 0, 0]); // letterboxed
        let fill = load_path(&path, MediaFit::Fill).unwrap();
        assert_eq!(fill.frames[1].get_pixel(5, 5).0, [0, 0, 255]); // cover crop fills edges
    }
}
