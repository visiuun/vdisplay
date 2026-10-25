//! Persistent settings stored in `%APPDATA%\Vdisplay\settings.json`.
//!
//! Unknown keys are preserved on save, so older and newer versions can share the file.

use serde_json::{Map, Value};
use std::path::PathBuf;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Media,
    Hardware,
    MediaFile,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BgStyle {
    Radiant,
    Gradient,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum IdleMode {
    Hardware,
    MediaFile,
    Standby,
}

/// `None` = honour the per-frame delays stored in the file.
pub type GifFps = Option<u32>;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MediaFit {
    Fill,
    Fit,
    Stretch,
}

#[derive(Clone, Debug)]
pub struct Settings {
    pub bg_style: BgStyle,
    pub music_fps: u32,
    pub gif_fps: GifFps,
    pub idle_mode: IdleMode,
    pub start_minimized: bool,
    pub auto_stream: bool,
    pub show_lyrics: bool,
    pub lyrics_offset_sec: f64,
    pub show_debug_overlay: bool,
    pub pause_on_fullscreen: bool,
    pub current_mode: Mode,
    pub brightness: i32,
    pub saturation: i32,
    pub hue: i32,
    /// File path, or `preset:<name>` for the built-in patterns.
    pub media_path: String,
    pub media_fit: MediaFit,
    /// Keys we don't know about (e.g. Radiant Lyrics tokens) are kept verbatim.
    pub extra: Map<String, Value>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            bg_style: BgStyle::Radiant,
            music_fps: 60,
            gif_fps: None,
            idle_mode: IdleMode::Hardware,
            start_minimized: false,
            auto_stream: true,
            show_lyrics: true,
            lyrics_offset_sec: 0.0,
            show_debug_overlay: false,
            pause_on_fullscreen: true,
            current_mode: Mode::Media,
            brightness: 100,
            saturation: 100,
            hue: 0,
            media_path: "preset:gradient".into(),
            media_fit: MediaFit::Fill,
            extra: Map::new(),
        }
    }
}

pub fn app_dir() -> PathBuf {
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("USERPROFILE").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("Vdisplay")
}

fn config_file() -> PathBuf {
    app_dir().join("settings.json")
}

fn as_bool(v: &Value) -> Option<bool> {
    match v {
        Value::Bool(b) => Some(*b),
        Value::Number(n) => n.as_f64().map(|f| f != 0.0),
        Value::String(s) => match s.trim().to_ascii_lowercase().as_str() {
            "true" | "1" | "yes" | "on" => Some(true),
            "false" | "0" | "no" | "off" => Some(false),
            _ => None,
        },
        _ => None,
    }
}

fn as_f64(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().trim_end_matches('s').trim().parse().ok(),
        _ => None,
    }
}

fn as_str(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

impl Settings {
    pub fn load() -> Self {
        let mut s = Settings::default();
        let Ok(text) = std::fs::read_to_string(config_file()) else {
            return s;
        };
        let Ok(Value::Object(mut map)) = serde_json::from_str::<Value>(&text) else {
            return s;
        };

        let mut take = |k: &str| map.remove(k);

        if let Some(v) = take("bg_style").and_then(|v| as_str(&v)) {
            s.bg_style = if v == "radiant" {
                BgStyle::Radiant
            } else {
                BgStyle::Gradient
            };
        }
        if let Some(v) = take("music_fps").and_then(|v| as_f64(&v)) {
            s.music_fps = match v as u32 {
                0..=22 => 15,
                23..=45 => 30,
                _ => 60,
            };
        }
        if let Some(v) = take("gif_fps").and_then(|v| as_str(&v)) {
            s.gif_fps = match v.as_str() {
                "15" => Some(15),
                "30" => Some(30),
                "60" => Some(60),
                _ => None,
            };
        }
        if let Some(v) = take("idle_mode").and_then(|v| as_str(&v)) {
            s.idle_mode = match v.as_str() {
                "MEDIA_FILE" => IdleMode::MediaFile,
                "STANDBY" => IdleMode::Standby,
                _ => IdleMode::Hardware,
            };
        }
        if let Some(v) = take("current_mode").and_then(|v| as_str(&v)) {
            s.current_mode = match v.as_str() {
                "HARDWARE" => Mode::Hardware,
                "MEDIA_FILE" => Mode::MediaFile,
                _ => Mode::Media,
            };
        }
        if let Some(v) = take("start_minimized").and_then(|v| as_bool(&v)) {
            s.start_minimized = v;
        }
        // Older files have no such key: they took over the screen at launch only when
        // starting minimized, so keep that behaviour for them.
        s.auto_stream = match take("auto_stream").and_then(|v| as_bool(&v)) {
            Some(v) => v,
            None => s.start_minimized,
        };
        if let Some(v) = take("show_lyrics").and_then(|v| as_bool(&v)) {
            s.show_lyrics = v;
        }
        if let Some(v) = take("show_debug_overlay").and_then(|v| as_bool(&v)) {
            s.show_debug_overlay = v;
        }
        if let Some(v) = take("pause_on_fullscreen").and_then(|v| as_bool(&v)) {
            s.pause_on_fullscreen = v;
        }
        if let Some(v) = take("lyrics_offset_sec").and_then(|v| as_f64(&v)) {
            s.lyrics_offset_sec = v.clamp(-10.0, 10.0);
        }
        if let Some(v) = take("brightness").and_then(|v| as_f64(&v)) {
            s.brightness = (v as i32).clamp(10, 200);
        }
        if let Some(v) = take("saturation").and_then(|v| as_f64(&v)) {
            s.saturation = (v as i32).clamp(0, 200);
        }
        if let Some(v) = take("hue").and_then(|v| as_f64(&v)) {
            s.hue = (v as i32).clamp(-180, 180);
        }
        if let Some(v) = take("media_path").and_then(|v| as_str(&v))
            && !v.is_empty()
        {
            s.media_path = v;
        }
        if let Some(v) = take("media_fit").and_then(|v| as_str(&v)) {
            s.media_fit = match v.as_str() {
                "fit" => MediaFit::Fit,
                "stretch" => MediaFit::Stretch,
                _ => MediaFit::Fill,
            };
        }
        s.extra = map;
        s
    }

    fn to_json(&self) -> Value {
        let mut m = self.extra.clone();
        let mut put = |k: &str, v: Value| {
            m.insert(k.to_string(), v);
        };
        put(
            "bg_style",
            Value::from(match self.bg_style {
                BgStyle::Radiant => "radiant",
                BgStyle::Gradient => "current",
            }),
        );
        put("music_fps", Value::from(self.music_fps.to_string()));
        put(
            "gif_fps",
            Value::from(
                self.gif_fps
                    .map(|f| f.to_string())
                    .unwrap_or_else(|| "auto".into()),
            ),
        );
        put(
            "idle_mode",
            Value::from(match self.idle_mode {
                IdleMode::Hardware => "HARDWARE",
                IdleMode::MediaFile => "MEDIA_FILE",
                IdleMode::Standby => "STANDBY",
            }),
        );
        put(
            "current_mode",
            Value::from(match self.current_mode {
                Mode::Media => "MEDIA",
                Mode::Hardware => "HARDWARE",
                Mode::MediaFile => "MEDIA_FILE",
            }),
        );
        put("start_minimized", Value::from(self.start_minimized));
        put("auto_stream", Value::from(self.auto_stream));
        put("show_lyrics", Value::from(self.show_lyrics));
        put(
            "lyrics_offset_sec",
            Value::from((self.lyrics_offset_sec * 1000.0).round() / 1000.0),
        );
        put("show_debug_overlay", Value::from(self.show_debug_overlay));
        put("pause_on_fullscreen", Value::from(self.pause_on_fullscreen));
        put("brightness", Value::from(self.brightness));
        put("saturation", Value::from(self.saturation));
        put("hue", Value::from(self.hue));
        put("media_path", Value::from(self.media_path.clone()));
        put(
            "media_fit",
            Value::from(match self.media_fit {
                MediaFit::Fill => "fill",
                MediaFit::Fit => "fit",
                MediaFit::Stretch => "stretch",
            }),
        );
        Value::Object(m)
    }

    /// Atomic save: write to a temp file, then rename over the old one so a crash
    /// mid-write can never leave a truncated settings file behind.
    pub fn save(&self) -> std::io::Result<()> {
        let dir = app_dir();
        std::fs::create_dir_all(&dir)?;
        let tmp = dir.join("settings.json.tmp");
        let text = serde_json::to_string_pretty(&self.to_json()).unwrap_or_default();
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, config_file())
    }
}
