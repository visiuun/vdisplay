//! Frame renderer for the 360×960 portrait OLED panel.

pub mod canvas;
pub mod effects;
pub mod icons;
pub mod text;

use crate::hw::HwStats;
use crate::lyrics::{self, Line, Lyrics, LyricsState, LyricsStore, Syllable};
use crate::media::{AppIcon, Art, MediaInfo};
use crate::sensors::SensorState;
use crate::settings::BgStyle;
use canvas::{Canvas, Rgb, c, ca, hex, pixmap_from_rgb};
use effects::{Kawarp, extract_palette};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use text::{Anchor, Face, Fonts};
use tiny_skia::Pixmap;

pub const W: u32 = 360;
pub const H: u32 = 960;
const WF: f32 = W as f32;

const WHITE: [u8; 4] = [255, 255, 255, 255];
const RED: Rgb = (255, 45, 70);
const GREEN: Rgb = (34, 197, 94);
const BLUE: Rgb = (56, 189, 248);

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum LyricsUi {
    #[default]
    Off,
    Loading,
    Missing,
    Synced(&'static str),
    WordSynced(&'static str),
}

#[derive(Clone, Copy, Default)]
pub struct RenderReport {
    pub lyrics: LyricsUi,
    pub word_sync: bool,
}

pub struct MusicOpts {
    pub bg_style: BgStyle,
    pub show_lyrics: bool,
    pub lyrics_offset_ms: f64,
}

pub struct DebugHud {
    pub fps: f32,
    pub kb: f32,
    pub mbps: f32,
    pub rnd_ms: f32,
    pub jpg_ms: f32,
    pub tx_ms: f32,
    pub tag: String,
}

#[derive(Clone)]
struct Seg {
    text: String,
    start: i64,
    dur: i64,
    end: i64,
}

impl From<&Syllable> for Seg {
    fn from(s: &Syllable) -> Self {
        Seg {
            text: s.text.clone(),
            start: s.start,
            dur: s.dur,
            end: s.end,
        }
    }
}

pub struct Renderer {
    pub fonts: Fonts,
    pub canvas: Canvas,
    kawarp: Kawarp,
    kawarp_base: Option<(u64, image::RgbImage)>,
    palette: Option<(u64, (Rgb, Rgb))>,
    art_cache: HashMap<(u64, u32), (Pixmap, i32)>,
    icon_cache: HashMap<String, Pixmap>,
    bg_cache: Option<(u8, Rgb, Vec<u8>)>,
    /// (app, paused since, last drawn) for the "paused for" timer.
    paused: Option<(String, Instant, Instant)>,
    epoch: Instant,
    pub report: RenderReport,
}

fn clock(fmt: &str) -> String {
    chrono::Local::now().format(fmt).to_string()
}

fn fmt_time(ms: f64) -> String {
    if ms <= 0.0 {
        return "00:00".into();
    }
    let s = (ms / 1000.0) as i64;
    format!("{:02}:{:02}", s / 60, s % 60)
}

impl Renderer {
    pub fn new() -> Self {
        Self {
            fonts: Fonts::new(),
            canvas: Canvas::new(W, H),
            kawarp: Kawarp::new(),
            kawarp_base: None,
            palette: None,
            art_cache: HashMap::new(),
            icon_cache: HashMap::new(),
            bg_cache: None,
            paused: None,
            epoch: Instant::now(),
            report: RenderReport::default(),
        }
    }

    fn t(&self) -> f64 {
        self.epoch.elapsed().as_secs_f64()
    }

    // Shared building blocks

    fn text(
        &mut self,
        s: &str,
        face: Face,
        size: f32,
        x: f32,
        y: f32,
        anchor: Anchor,
        col: [u8; 4],
    ) -> f32 {
        self.canvas
            .text(&mut self.fonts, s, face, size, x, y, anchor, col)
    }

    fn width(&mut self, s: &str, face: Face, size: f32) -> f32 {
        self.fonts.width(s, face, size)
    }

    /// Truncates with an ellipsis so the text fits `max_w`.
    fn ellipsize(&mut self, s: &str, face: Face, size: f32, max_w: f32) -> String {
        if self.width(s, face, size) <= max_w {
            return s.to_string();
        }
        let mut chars: Vec<char> = s.chars().collect();
        while chars.len() > 1 {
            chars.pop();
            let cand: String = chars.iter().collect::<String>().trim_end().to_string() + "…";
            if self.width(&cand, face, size) <= max_w {
                return cand;
            }
        }
        "…".into()
    }

    fn palette(&mut self, art: Option<&Arc<Art>>) -> (Rgb, Rgb) {
        let Some(a) = art else {
            return (effects::DEFAULT_BG, effects::DEFAULT_ACCENT);
        };
        if let Some((id, p)) = self.palette
            && id == a.id
        {
            return p;
        }
        let p = extract_palette(&a.img);
        self.palette = Some((a.id, p));
        p
    }

    /// Rounded, bordered (and optionally glowing) cover sprite, built once per track.
    fn draw_art(
        &mut self,
        art: &Arc<Art>,
        size: u32,
        x: f32,
        y: f32,
        radius: f32,
        border: ([u8; 4], f32),
        glow: Option<Rgb>,
    ) {
        let key = (art.id, size);
        if !self.art_cache.contains_key(&key) {
            self.art_cache.retain(|k, _| k.0 == art.id);
            let img = image::imageops::resize(
                &art.img,
                size,
                size,
                image::imageops::FilterType::Lanczos3,
            );
            let Some(pm) = pixmap_from_rgb(&img) else {
                return;
            };
            self.art_cache
                .insert(key, canvas::art_sprite(&pm, radius, Some(border), glow));
        }
        let (pm, pad) = &self.art_cache[&key];
        self.canvas.blit(pm, x as i32 - pad, y as i32 - pad);
    }

    fn background(
        &mut self,
        art: Option<&Arc<Art>>,
        bg: Rgb,
        accent: Rgb,
        style: BgStyle,
        dim: f32,
    ) {
        let (w, h) = (W as usize, H as usize);
        match (style, art) {
            (BgStyle::Radiant, Some(a)) => {
                if self.kawarp_base.as_ref().is_none_or(|(id, _)| *id != a.id) {
                    self.kawarp_base = Some((a.id, effects::kawarp_base(&a.img)));
                }
                let t = self.t();
                let base = &self.kawarp_base.as_ref().unwrap().1;
                self.kawarp
                    .render(base, t, self.canvas.pm.data_mut(), w, h, dim);
            }
            _ => {
                let kind = if style == BgStyle::Radiant { 1u8 } else { 2u8 };
                let col = if kind == 1 { accent } else { bg };
                if self
                    .bg_cache
                    .as_ref()
                    .is_none_or(|(k, c, _)| *k != kind || *c != col)
                {
                    let mut buf = vec![0u8; w * h * 4];
                    if kind == 1 {
                        effects::orb_background(accent, &mut buf, w, h);
                    } else {
                        effects::gradient_background(bg, &mut buf, w, h);
                    }
                    self.bg_cache = Some((kind, col, buf));
                }
                let buf = &self.bg_cache.as_ref().unwrap().2;
                self.canvas.pm.data_mut().copy_from_slice(buf);
            }
        }
    }

    fn header_icon(&mut self, info: &MediaInfo) {
        let key = match &info.icon {
            AppIcon::Spotify => "spotify".to_string(),
            AppIcon::Tidal => "tidal".to_string(),
            AppIcon::AppleMusic => "apple".to_string(),
            AppIcon::Generic => "generic".to_string(),
            AppIcon::Extracted(_) => format!("exe:{}", info.source_app),
        };
        if !self.icon_cache.contains_key(&key) {
            if self.icon_cache.len() > 16 {
                self.icon_cache.clear();
            }
            self.icon_cache
                .insert(key.clone(), icons::app_icon_pixmap(&info.icon, 68));
        }
        let pm = self.icon_cache.get(&key).unwrap();
        self.canvas.image_rounded(pm, 24.0, 20.0, 34.0, 34.0, 10.0);
        self.canvas.rrect(
            24.0,
            20.0,
            57.0,
            53.0,
            10.0,
            None,
            Some(c((42, 42, 54))),
            1.0,
        );
    }

    fn progress_bar(
        &mut self,
        y: f32,
        h: f32,
        pos: f64,
        dur: f64,
        accent: Rgb,
        time_size: f32,
        time_dy: f32,
    ) {
        let (x1, x2) = (28.0, WF - 28.0);
        let r = h / 2.0;
        self.canvas.fill_rrect(x1, y, x2, y + h, r, c((35, 35, 44)));
        let pct = if dur > 0.0 {
            (pos / dur).clamp(0.0, 1.0)
        } else {
            0.0
        } as f32;
        let end = x1 + pct * (x2 - x1);
        if end > x1 + 1.0 {
            self.canvas.fill_rrect(x1, y, end, y + h, r, c(accent));
            self.canvas.circle(end, y + h / 2.0, r + 3.0, WHITE);
        }
        self.text(
            &fmt_time(pos),
            Face::Semibold,
            time_size,
            x1,
            y + time_dy,
            Anchor::LeftTop,
            hex(0xA1A1AA),
        );
        self.text(
            &fmt_time(dur),
            Face::Semibold,
            time_size,
            x2,
            y + time_dy,
            Anchor::RightTop,
            hex(0xA1A1AA),
        );
    }

    fn status_pill(&mut self, cy: f32, playing: bool, accent: Rgb) {
        let (label, col) = if playing {
            ("●  PLAYING", accent)
        } else {
            ("❚❚  PAUSED", (140, 140, 150))
        };
        let w = self.width(label, Face::Bold, 12.0) + 40.0;
        let x0 = WF / 2.0 - w / 2.0;
        self.canvas.rrect(
            x0,
            cy - 15.0,
            x0 + w,
            cy + 15.0,
            15.0,
            Some(c((12, 12, 16))),
            Some(ca(col, 200)),
            1.0,
        );
        self.text(
            label,
            Face::Bold,
            12.0,
            WF / 2.0,
            cy,
            Anchor::Center,
            c(col),
        );
    }

    // Music screens

    pub fn music(
        &mut self,
        info: Option<&MediaInfo>,
        pos_ms: f64,
        opts: &MusicOpts,
        lyrics_store: &Arc<LyricsStore>,
    ) {
        self.report = RenderReport::default();
        let Some(info) = info else {
            self.standby();
            return;
        };
        if opts.show_lyrics {
            match lyrics_store.get(&info.title, &info.artist, info.duration_ms) {
                LyricsState::Ready(l) => {
                    self.lyrics_screen(info, &l, pos_ms, opts);
                    return;
                }
                LyricsState::Loading => self.report.lyrics = LyricsUi::Loading,
                LyricsState::Missing => self.report.lyrics = LyricsUi::Missing,
            }
        }
        self.now_playing(info, pos_ms, opts);
    }

    fn now_playing(&mut self, info: &MediaInfo, pos_ms: f64, opts: &MusicOpts) {
        let art = info.art.clone();
        let (bg, accent) = self.palette(art.as_ref());
        self.background(art.as_ref(), bg, accent, opts.bg_style, 0.82);
        self.header_icon(info);
        self.text(
            &clock("%H:%M"),
            Face::Bold,
            24.0,
            WF - 24.0,
            24.0,
            Anchor::RightTop,
            WHITE,
        );

        let (size, x, y) = (312u32, (W - 312) as f32 / 2.0, 140.0);
        if let Some(a) = &art {
            self.draw_art(a, size, x, y, 24.0, (ca(accent, 170), 1.5), Some(accent));
        } else {
            self.canvas.rrect(
                x,
                y,
                x + size as f32,
                y + size as f32,
                24.0,
                Some(c((14, 14, 18))),
                Some(c((40, 40, 48))),
                1.0,
            );
            self.text(
                "♪",
                Face::Bold,
                64.0,
                WF / 2.0,
                y + 140.0,
                Anchor::Center,
                c((70, 70, 80)),
            );
            self.text(
                "NO ARTWORK",
                Face::Semibold,
                14.0,
                WF / 2.0,
                y + 200.0,
                Anchor::Center,
                c((90, 90, 100)),
            );
        }

        let title_lines = self.wrap(&info.title, Face::Bold, 30.0, WF - 48.0, 2);
        let mut ty = 515.0;
        for l in &title_lines {
            self.text(l, Face::Bold, 30.0, WF / 2.0, ty, Anchor::Center, WHITE);
            ty += 38.0;
        }
        let artist = self.ellipsize(&info.artist, Face::Regular, 22.0, WF - 48.0);
        self.text(
            &artist,
            Face::Regular,
            22.0,
            WF / 2.0,
            ty + 10.0,
            Anchor::Center,
            hex(0xE4E4E7),
        );
        if !info.album.is_empty() {
            let album = self.ellipsize(&info.album, Face::Regular, 15.0, WF - 48.0);
            self.text(
                &album,
                Face::Regular,
                15.0,
                WF / 2.0,
                ty + 44.0,
                Anchor::Center,
                hex(0xA1A1AA),
            );
        }

        self.progress_bar(745.0, 8.0, pos_ms, info.duration_ms, accent, 14.0, 18.0);
        self.status_pill(851.0, info.is_playing, accent);
    }

    /// Word wrap (or per-character wrap for CJK / overlong words), capped at `max_lines`.
    fn wrap(
        &mut self,
        text: &str,
        face: Face,
        size: f32,
        max_w: f32,
        max_lines: usize,
    ) -> Vec<String> {
        let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
        if text.is_empty() {
            return vec![];
        }
        let mut lines = Vec::new();
        if lyrics::needs_char_wrap(&text)
            || (!text.contains(' ') && self.width(&text, face, size) > max_w)
        {
            let mut cur = String::new();
            for ch in text.chars() {
                let mut cand = cur.clone();
                cand.push(ch);
                if self.width(&cand, face, size) <= max_w || cur.is_empty() {
                    cur = cand;
                } else {
                    lines.push(std::mem::take(&mut cur).trim().to_string());
                    cur.push(ch);
                }
            }
            if !cur.is_empty() {
                lines.push(cur.trim().to_string());
            }
        } else {
            let mut cur = String::new();
            for w in text.split(' ') {
                let cand = if cur.is_empty() {
                    w.to_string()
                } else {
                    format!("{cur} {w}")
                };
                if self.width(&cand, face, size) <= max_w || cur.is_empty() {
                    cur = cand;
                } else {
                    lines.push(std::mem::replace(&mut cur, w.to_string()));
                }
            }
            if !cur.is_empty() {
                lines.push(cur);
            }
        }
        if lines.len() > max_lines {
            lines.truncate(max_lines);
            let last = lines.pop().unwrap() + "…";
            let fitted = self.ellipsize(&last, face, size, max_w);
            lines.push(fitted);
        }
        lines
    }

    /// Splits multi-word syllables into words (timing shared by character count) so they can wrap.
    fn split_multiword(segs: Vec<Seg>) -> Vec<Seg> {
        let mut out = Vec::with_capacity(segs.len());
        for s in segs {
            if s.text.is_empty() {
                continue;
            }
            if s.text.split_whitespace().count() <= 1 || lyrics::needs_char_wrap(&s.text) {
                out.push(s);
                continue;
            }
            let parts: Vec<&str> = s.text.split(' ').collect();
            let tot = parts
                .iter()
                .map(|p| p.chars().count())
                .sum::<usize>()
                .max(1) as f64;
            let mut t = s.start;
            let last = parts.len() - 1;
            for (i, p) in parts.iter().enumerate() {
                if p.is_empty() && i > 0 {
                    continue;
                }
                let text = if i == last {
                    p.to_string()
                } else {
                    format!("{p} ")
                };
                let d = ((s.dur as f64 * (p.chars().count().max(1) as f64 / tot)) as i64).max(10);
                out.push(Seg {
                    text,
                    start: t,
                    dur: d,
                    end: t + d,
                });
                t += d;
            }
        }
        out
    }

    fn segment(line: &Line, syls: &[&Syllable], force_clean: bool) -> Vec<Seg> {
        let mut out = Vec::new();
        if !syls.is_empty() {
            let segs = syls
                .iter()
                .map(|s| Seg {
                    text: if force_clean {
                        s.clean.clone()
                    } else {
                        s.text.clone()
                    },
                    start: s.start,
                    dur: s.dur,
                    end: s.end,
                })
                .collect();
            return Self::split_multiword(segs);
        } else {
            let txt = if force_clean {
                line.clean_text.as_str()
            } else {
                line.text.as_str()
            };
            if txt.is_empty() {
                return out;
            }
            let line_dur = line.dur.max(600);
            if lyrics::needs_char_wrap(txt) {
                let chars: Vec<char> = txt.chars().collect();
                let cd = (line_dur / chars.len().max(1) as i64).max(50);
                let mut t = line.start;
                for ch in chars {
                    out.push(Seg {
                        text: ch.to_string(),
                        start: t,
                        dur: cd,
                        end: t + cd,
                    });
                    t += cd;
                }
            } else {
                let mut words: Vec<String> =
                    txt.split_whitespace().map(|w| format!("{w} ")).collect();
                if let Some(l) = words.last_mut() {
                    *l = l.trim_end().to_string();
                }
                let tot = words
                    .iter()
                    .map(|w| w.chars().count())
                    .sum::<usize>()
                    .max(1) as f64;
                let mut t = line.start;
                for w in words {
                    let d = ((line_dur as f64 * (w.chars().count() as f64 / tot)) as i64).max(80);
                    out.push(Seg {
                        text: w,
                        start: t,
                        dur: d,
                        end: t + d,
                    });
                    t += d;
                }
            }
        }
        out
    }

    /// Packs segments into visual lines, breaking only between words (a syllable group like
    /// "e"+"nough" stays together) unless a single word is wider than the line.
    fn visual_lines(
        &mut self,
        items: Vec<Seg>,
        face: Face,
        size: f32,
        max_w: f32,
    ) -> Vec<Vec<(Seg, f32)>> {
        let mut words: Vec<Vec<(Seg, f32)>> = Vec::new();
        let mut cur_word: Vec<(Seg, f32)> = Vec::new();
        for s in items {
            let w = self.width(&s.text, face, size);
            let ends_word =
                s.text.ends_with(char::is_whitespace) || lyrics::needs_char_wrap(&s.text);
            cur_word.push((s, w));
            if ends_word {
                words.push(std::mem::take(&mut cur_word));
            }
        }
        if !cur_word.is_empty() {
            words.push(cur_word);
        }

        let mut lines: Vec<Vec<(Seg, f32)>> = Vec::new();
        let mut cur: Vec<(Seg, f32)> = Vec::new();
        let mut cur_w = 0.0;
        for word in words {
            let ww: f32 = word.iter().map(|(_, w)| *w).sum();
            // Trailing space of the word doesn't need to fit at the end of a line.
            let ww_trim = ww
                - word
                    .last()
                    .map(|(s, w)| {
                        if s.text.ends_with(' ') {
                            w - self.width(s.text.trim_end(), face, size)
                        } else {
                            0.0
                        }
                    })
                    .unwrap_or(0.0);
            if cur_w + ww_trim > max_w && !cur.is_empty() {
                lines.push(std::mem::take(&mut cur));
                cur_w = 0.0;
            }
            if ww_trim > max_w {
                // Overlong single word: fall back to breaking between its syllables.
                for (s, w) in word {
                    if cur_w + w > max_w && !cur.is_empty() {
                        lines.push(std::mem::take(&mut cur));
                        cur_w = 0.0;
                    }
                    cur_w += w;
                    cur.push((s, w));
                }
            } else {
                cur_w += ww;
                cur.extend(word);
            }
        }
        if !cur.is_empty() {
            lines.push(cur);
        }
        lines
    }

    fn lyrics_screen(&mut self, info: &MediaInfo, ly: &Lyrics, position_ms: f64, opts: &MusicOpts) {
        let art = info.art.clone();
        let (bg, accent) = self.palette(art.as_ref());
        self.background(art.as_ref(), bg, accent, opts.bg_style, 0.62);

        let lines = &ly.lines;
        let pos = (position_ms + opts.lyrics_offset_ms) as i64;
        let idx = ly.starts.partition_point(|&s| s <= pos) as isize - 1;
        let active_idx = idx.clamp(0, lines.len() as isize - 1) as usize;
        let active = &lines[active_idx];
        let has_word_sync = active.has_syls && !active.syls.is_empty();
        self.report.word_sync = has_word_sync;
        self.report.lyrics = if ly.word_synced {
            LyricsUi::WordSynced(ly.source)
        } else {
            LyricsUi::Synced(ly.source)
        };

        // Lead vs ad-lib pairing.
        let mut lead_idx = active_idx;
        let mut adlib_idx: Option<usize> = None;
        if active.bg {
            let prev = (active_idx.saturating_sub(2)..active_idx)
                .rev()
                .find(|&i| !lines[i].bg);
            if let Some(p) = prev
                && (active.start - lines[p].start).abs() < 4500
            {
                lead_idx = p;
                adlib_idx = Some(active_idx);
            }
        } else if let Some(n) = lines.get(active_idx + 1)
            && n.bg
            && n.start <= pos + 2500
            && n.end >= pos - 1500
        {
            adlib_idx = Some(active_idx + 1);
        }
        let lead = &lines[lead_idx];
        let adlib_line = adlib_idx.map(|i| &lines[i]);

        let mut lead_syls: Vec<&Syllable> = Vec::new();
        let mut adlib_syls: Vec<Seg> = Vec::new();
        if lead.has_syls {
            let mut in_paren = false;
            for s in &lead.syls {
                if s.text.contains('(') || s.text.contains('[') {
                    in_paren = true;
                }
                let is_adlib = s.bg || (lead.has_text_adlib && in_paren);
                if s.text.contains(')') || s.text.contains(']') {
                    in_paren = false;
                }
                if is_adlib && adlib_line.is_none() {
                    adlib_syls.push(s.into());
                } else {
                    lead_syls.push(s);
                }
            }
        }
        if lead_syls.is_empty() && !lead.syls.is_empty() {
            lead_syls = lead.syls.iter().collect();
            adlib_syls.clear();
        }
        if let Some(a) = adlib_line.filter(|a| a.has_syls) {
            adlib_syls = a.syls.iter().map(Seg::from).collect();
        } else if adlib_syls.is_empty()
            && lead.has_text_adlib
            && let Some(t) = &lead.adlib_text
        {
            adlib_syls.push(Seg {
                text: t.clone(),
                start: lead.start,
                dur: lead.dur,
                end: lead.end,
            });
        }

        // Header
        self.header_icon(info);
        let is_chorus = lead.part.to_lowercase().contains("chorus");
        let (label, badge_col) = if is_chorus {
            (
                if has_word_sync {
                    "CHORUS • WORD SYNC".to_string()
                } else {
                    "CHORUS".into()
                },
                RED,
            )
        } else if lead.right {
            (
                if has_word_sync {
                    "SINGER 2 • WORD SYNC".to_string()
                } else {
                    "SINGER 2 • DUET".into()
                },
                BLUE,
            )
        } else if !lead.part.is_empty() {
            let p = lead.part.to_uppercase();
            (
                if has_word_sync {
                    format!("{p} • WORD SYNC")
                } else {
                    p
                },
                accent,
            )
        } else {
            (
                if has_word_sync {
                    "WORD SYNC".to_string()
                } else {
                    "SYNCED LYRICS".into()
                },
                accent,
            )
        };
        let label = self.ellipsize(&label, Face::Bold, 10.0, 150.0);
        let bw = self.width(&label, Face::Bold, 10.0) + 36.0;
        self.canvas.rrect(
            66.0,
            24.0,
            66.0 + bw,
            50.0,
            13.0,
            Some(ca((14, 14, 18), 235)),
            Some(ca(badge_col, 220)),
            1.0,
        );
        self.canvas.circle(79.0, 37.0, 3.0, c(badge_col));
        self.text(
            &label,
            Face::Bold,
            10.0,
            66.0 + bw / 2.0 + 5.0,
            37.0,
            Anchor::Center,
            WHITE,
        );
        self.text(
            &clock("%H:%M"),
            Face::Bold,
            22.0,
            WF - 24.0,
            24.0,
            Anchor::RightTop,
            WHITE,
        );

        // Compact track card
        let (cy, chh) = (62.0, 84.0);
        self.canvas.rrect(
            20.0,
            cy,
            WF - 20.0,
            cy + chh,
            16.0,
            Some(ca((10, 10, 14), 200)),
            Some(ca((60, 60, 76), 120)),
            1.0,
        );
        let (thumb, tx0, ty0) = (64u32, 30.0, cy + 10.0);
        if let Some(a) = &art {
            self.draw_art(a, thumb, tx0, ty0, 12.0, (c((60, 60, 75)), 1.0), None);
        } else {
            self.canvas.rrect(
                tx0,
                ty0,
                tx0 + thumb as f32,
                ty0 + thumb as f32,
                12.0,
                Some(c((24, 24, 30))),
                Some(c((50, 50, 60))),
                1.0,
            );
            self.text(
                "♪",
                Face::Bold,
                22.0,
                tx0 + 32.0,
                ty0 + 32.0,
                Anchor::Center,
                c((85, 85, 85)),
            );
        }
        let tx = tx0 + thumb as f32 + 12.0;
        let max_w = WF - tx - 28.0;
        let t = self.ellipsize(&info.title, Face::Bold, 16.0, max_w);
        self.text(&t, Face::Bold, 16.0, tx, cy + 13.0, Anchor::LeftTop, WHITE);
        let a = self.ellipsize(&info.artist, Face::Regular, 13.0, max_w);
        self.text(
            &a,
            Face::Regular,
            13.0,
            tx,
            cy + 36.0,
            Anchor::LeftTop,
            hex(0xD4D4D8),
        );
        let sync = if has_word_sync {
            "Word synced"
        } else {
            "Synced"
        };
        let album = if info.album.is_empty() {
            "Now Playing"
        } else {
            info.album.as_str()
        };
        let section = if lead.part.is_empty() {
            String::new()
        } else {
            format!(" • {}", lead.part)
        };
        let tail = format!("{section} • {sync}");
        let tail_w = self.width(&tail, Face::Regular, 11.0);
        let album = self.ellipsize(album, Face::Regular, 11.0, (max_w - tail_w).max(30.0));
        self.text(
            &format!("{album}{tail}"),
            Face::Regular,
            11.0,
            tx,
            cy + 57.0,
            Anchor::LeftTop,
            c(accent),
        );

        // Lyric stage
        const ACTIVE: f32 = 34.0;
        const ADLIB: f32 = 20.0;
        const OTHER: f32 = 22.0;
        const ACTIVE_H: f32 = 44.0;
        const ADLIB_H: f32 = 28.0;
        const OTHER_H: f32 = 32.0;
        const PAD: f32 = 26.0;
        const TOP: f32 = 164.0;
        const BOTTOM: f32 = 825.0;
        let max_lw = WF - PAD * 2.0;

        let force_clean = !adlib_syls.is_empty() || lead.has_text_adlib;
        let lead_words = Self::segment(lead, &lead_syls, force_clean);
        let active_lines = self.visual_lines(lead_words, Face::Bold, ACTIVE, max_lw);

        let mut adlib_lines = Vec::new();
        let mut adlib_right = lead.right;
        if !adlib_syls.is_empty() || adlib_line.is_some_and(|a| !a.text.is_empty()) {
            let ad_obj = adlib_line.unwrap_or(lead);
            adlib_right = ad_obj.right;
            let mut words = if !adlib_syls.is_empty() {
                Self::split_multiword(adlib_syls)
            } else {
                Self::segment(ad_obj, &[], false)
            };
            if let Some(f) = words.first_mut()
                && !f.text.starts_with('(')
            {
                f.text = format!("({}", f.text);
            }
            if let Some(l) = words.last_mut()
                && !l.text.ends_with(')')
            {
                l.text = format!("{})", l.text.trim_end());
            }
            adlib_lines = self.visual_lines(words, Face::Bold, ADLIB, max_lw);
        }

        let first_start = lines[0].start;
        let mut instrumental = false;
        if pos < first_start {
            instrumental = true;
        } else if pos > lead.end + 2500 {
            instrumental = match lines.get(active_idx + 1) {
                Some(n) => pos < n.start,
                None => true,
            };
        }

        let lead_h = active_lines.len() as f32 * ACTIVE_H;
        let adlib_h = if adlib_lines.is_empty() {
            0.0
        } else {
            adlib_lines.len() as f32 * ADLIB_H + 4.0
        };
        let total_h = if instrumental { 36.0 } else { lead_h + adlib_h };
        let active_y = (360.0 - total_h / 2.0).clamp(240.0, 410.0).floor();

        let mut skip: Vec<usize> = if instrumental {
            vec![]
        } else {
            vec![active_idx]
        };
        if let Some(a) = adlib_idx {
            skip.push(a);
        }

        // Past lines (clean, no ad-libs), laid out upwards.
        let past_start: isize = if instrumental && pos >= first_start {
            active_idx as isize
        } else {
            active_idx as isize - 1
        };
        let mut past_items = Vec::new();
        let mut p = past_start;
        while p >= 0 && p > past_start - 5 {
            let pi = p as usize;
            p -= 1;
            if skip.contains(&pi) {
                continue;
            }
            let l = &lines[pi];
            if l.bg || l.pure_adlib || l.clean_text.trim().is_empty() {
                continue;
            }
            let wrapped = self.wrap(&l.clean_text, Face::Bold, OTHER, max_lw, 4);
            if !wrapped.is_empty() {
                past_items.push((pi, wrapped, l.right));
            }
        }
        let mut bottom = active_y - 20.0;
        for (rank, (_, wrapped, right)) in past_items.iter().enumerate() {
            let block_h = wrapped.len() as f32 * OTHER_H;
            let top = bottom - block_h;
            if top < TOP {
                break;
            }
            let dim = if rank == 0 {
                [255, 255, 255, 115]
            } else {
                [255, 255, 255, 56]
            };
            let mut dy = top;
            for wl in wrapped {
                if *right {
                    self.text(wl, Face::Bold, OTHER, WF - PAD, dy, Anchor::RightTop, dim);
                } else {
                    self.text(wl, Face::Bold, OTHER, PAD, dy, Anchor::LeftTop, dim);
                }
                dy += OTHER_H;
            }
            bottom = top - 14.0;
        }

        let mut cur_y;
        if instrumental {
            let t = self.t() * 3.8;
            let dot_cy = active_y + 16.0;
            let gap = 22.0;
            let right = if pos < first_start {
                lines[0].right
            } else if let Some(n) = lines.get(active_idx + 1).filter(|n| pos < n.start) {
                n.right
            } else {
                lead.right
            };
            let x0 = if right {
                WF - PAD - 8.0 - 2.0 * gap
            } else {
                PAD + 8.0
            };
            for d in 0..3 {
                let pulse = (t - d as f64 * 0.75).sin().max(0.0) as f32;
                let dy = dot_cy - pulse * 9.0;
                let r = 5.0 + pulse * 3.0;
                let v = (120.0 + pulse * 135.0) as u8;
                self.canvas.circle(x0 + d as f32 * gap, dy, r, c((v, v, v)));
            }
            cur_y = active_y + 36.0;
        } else {
            cur_y = active_y;
            let lead_dur = lead.dur.max(500) as f64;
            let total_w: f32 = active_lines.iter().flatten().map(|(_, w)| *w).sum();
            let ratio = ((pos - lead.start) as f64 / lead_dur).clamp(0.0, 1.0) as f32;
            let target_px = ratio * total_w;
            let mut acc = 0.0f32;
            for vl in &active_lines {
                let line_w: f32 = vl.iter().map(|(_, w)| *w).sum();
                let line_x = if lead.right { WF - PAD - line_w } else { PAD };
                let mut x = line_x;
                for (s, sw) in vl {
                    if *sw <= 0.0 {
                        continue;
                    }
                    self.text(
                        &s.text,
                        Face::Bold,
                        ACTIVE,
                        x.round(),
                        cur_y,
                        Anchor::LeftTop,
                        WHITE,
                    );
                    if has_word_sync {
                        let uy = cur_y + ACTIVE_H - 4.0;
                        if s.start <= pos && pos <= s.end {
                            let r = ((pos - s.start) as f32 / s.dur.max(1) as f32).clamp(0.0, 1.0);
                            let fw = (sw * r).floor();
                            if fw > 0.0 {
                                self.canvas.fill_rrect(x, uy, x + fw, uy + 3.0, 1.5, WHITE);
                            }
                        } else if pos > s.end {
                            self.canvas.fill_rrect(
                                x,
                                uy,
                                x + sw - 3.0,
                                uy + 3.0,
                                1.5,
                                c((210, 210, 220)),
                            );
                        }
                    }
                    x += sw;
                }
                if !has_word_sync {
                    let pw = if target_px >= acc + line_w {
                        line_w
                    } else if target_px > acc {
                        target_px - acc
                    } else {
                        0.0
                    };
                    if pw > 0.0 {
                        let uy = cur_y + ACTIVE_H - 4.0;
                        self.canvas.fill_rrect(
                            line_x,
                            uy,
                            line_x + pw.min(line_w),
                            uy + 3.0,
                            1.5,
                            WHITE,
                        );
                    }
                    acc += line_w;
                }
                cur_y += ACTIVE_H;
            }

            if !adlib_lines.is_empty() {
                cur_y += 4.0;
                for al in &adlib_lines {
                    let line_w: f32 = al.iter().map(|(_, w)| *w).sum();
                    let mut x = if adlib_right { WF - PAD - line_w } else { PAD };
                    for (s, sw) in al {
                        if *sw <= 0.0 {
                            continue;
                        }
                        let uy = cur_y + ADLIB_H - 3.0;
                        let muted = [255, 255, 255, 120];
                        if pos < s.start {
                            self.text(
                                &s.text,
                                Face::Bold,
                                ADLIB,
                                x.round(),
                                cur_y,
                                Anchor::LeftTop,
                                muted,
                            );
                        } else if pos >= s.end {
                            self.text(
                                &s.text,
                                Face::Bold,
                                ADLIB,
                                x.round(),
                                cur_y,
                                Anchor::LeftTop,
                                WHITE,
                            );
                            self.canvas.fill_rrect(
                                x,
                                uy,
                                x + sw - 2.0,
                                uy + 2.0,
                                1.0,
                                c((200, 200, 210)),
                            );
                        } else {
                            // Sub-word wipe: muted text with a white copy clipped to the progress.
                            self.text(
                                &s.text,
                                Face::Bold,
                                ADLIB,
                                x.round(),
                                cur_y,
                                Anchor::LeftTop,
                                muted,
                            );
                            let r = ((pos - s.start) as f32 / s.dur.max(1) as f32).clamp(0.0, 1.0);
                            let fw = (sw * r).floor();
                            if fw > 0.0 {
                                let xr = x.round();
                                self.canvas.text_clipped(
                                    &mut self.fonts,
                                    &s.text,
                                    Face::Bold,
                                    ADLIB,
                                    xr,
                                    cur_y,
                                    WHITE,
                                    (xr, xr + fw),
                                );
                                self.canvas.fill_rrect(x, uy, x + fw, uy + 2.0, 1.0, WHITE);
                            }
                        }
                        x += sw;
                    }
                    cur_y += ADLIB_H;
                }
            }
        }

        // Upcoming lines (clean, no ad-libs).
        cur_y += 24.0;
        let start_u = if pos < first_start { 0 } else { active_idx + 1 };
        #[allow(clippy::needless_range_loop)]
        for ui in start_u..lines.len().min(start_u + 8) {
            if cur_y > BOTTOM {
                break;
            }
            if skip.contains(&ui) {
                continue;
            }
            let l = &lines[ui];
            if l.bg || l.pure_adlib || l.clean_text.trim().is_empty() {
                continue;
            }
            let dim = match ui - start_u {
                0 => [255, 255, 255, 150],
                1 => [255, 255, 255, 105],
                2 => [255, 255, 255, 70],
                _ => [255, 255, 255, 40],
            };
            let wrapped = self.wrap(&l.clean_text, Face::Bold, OTHER, max_lw, 4);
            for wl in &wrapped {
                if cur_y + OTHER_H > BOTTOM {
                    break;
                }
                if l.right {
                    self.text(
                        wl,
                        Face::Bold,
                        OTHER,
                        WF - PAD,
                        cur_y,
                        Anchor::RightTop,
                        dim,
                    );
                } else {
                    self.text(wl, Face::Bold, OTHER, PAD, cur_y, Anchor::LeftTop, dim);
                }
                cur_y += OTHER_H;
            }
            cur_y += 14.0;
        }

        self.progress_bar(
            850.0,
            6.0,
            position_ms,
            info.duration_ms,
            accent,
            13.0,
            15.0,
        );
        self.status_pill(904.0, info.is_playing, accent);
    }

    // Other screens

    /// Nothing playing: a dim, slowly drifting clock (OLED burn-in friendly).
    pub fn standby(&mut self) {
        self.canvas.clear((0, 0, 0));
        let now = chrono::Local::now();
        let drift = ((now.timestamp() / 60) % 12) as f32 * 6.0 - 33.0;
        let cy = 430.0 + drift;
        self.text(
            &now.format("%H:%M").to_string(),
            Face::Semibold,
            88.0,
            WF / 2.0,
            cy,
            Anchor::Center,
            c((92, 92, 102)),
        );
        self.text(
            &now.format("%A, %d %B").to_string().to_uppercase(),
            Face::Semibold,
            14.0,
            WF / 2.0,
            cy + 62.0,
            Anchor::Center,
            c((70, 70, 80)),
        );
        self.canvas.fill_rrect(
            WF / 2.0 - 18.0,
            cy + 92.0,
            WF / 2.0 + 18.0,
            cy + 94.0,
            1.0,
            ca(RED, 120),
        );
        self.text(
            "NO MEDIA PLAYING",
            Face::Bold,
            12.0,
            WF / 2.0,
            cy + 122.0,
            Anchor::Center,
            c((52, 52, 60)),
        );
    }

    /// System telemetry: pure black, typographic, colour only where it means something.
    pub fn hardware(&mut self, s: &HwStats) {
        const L: f32 = 28.0;
        const R: f32 = WF - 28.0;
        let label_col = c((112, 112, 122));
        let dim = c((84, 84, 94));
        let hair = c((28, 28, 33));
        self.canvas.clear((0, 0, 0));
        self.header();

        let fmt_temp = |t: Option<f32>| t.map(|t| format!("{t:.0}°"));
        let ghz = |mhz: Option<f32>| {
            mhz.filter(|m| *m > 0.0)
                .map(|m| format!("{:.2} GHz", m / 1000.0))
        };
        let watts = |w: Option<f32>| w.filter(|w| *w > 0.0).map(|w| format!("{w:.0} W"));

        // CPU
        let details: Vec<String> = [ghz(s.cpu_clock_mhz), watts(s.cpu_power_w)]
            .into_iter()
            .flatten()
            .collect();
        self.metric_block(
            130.0,
            "CPU",
            &s.cpu_name,
            s.cpu_pct,
            fmt_temp(s.cpu_temp),
            s.cpu_temp,
            "TEMP",
            &details,
        );

        // GPU
        match &s.gpu {
            Some(g) => {
                let mut d = Vec::new();
                if g.vram_total_gb > 0.0 {
                    d.push(format!("{:.1} / {:.1} GB", g.vram_used_gb, g.vram_total_gb));
                }
                d.extend([ghz(g.clock_mhz), watts(g.power_w)].into_iter().flatten());
                self.metric_block(
                    288.0,
                    "GPU",
                    &g.name,
                    g.util,
                    fmt_temp(g.temp),
                    g.temp,
                    "TEMP",
                    &d,
                );
            }
            None => self.metric_block(288.0, "GPU", "not detected", 0.0, None, None, "TEMP", &[]),
        }

        // Memory (+ system drive on the right)
        let used = format!("{:.1} / {:.1} GB", s.ram_used_gb, s.ram_total_gb);
        self.metric_block(
            446.0,
            "MEMORY",
            &used,
            s.ram_pct,
            Some(format!("{:.0}%", s.disk_pct)),
            None,
            "DRIVE C:",
            &[],
        );

        // Fans
        let mut y = 572.0;
        self.canvas.hline(L, R, y, hair);
        y += 20.0;
        self.text(
            "FANS",
            Face::Semibold,
            12.0,
            L,
            y,
            Anchor::LeftTop,
            label_col,
        );
        if let Some(src) = Some(s.sensor_source).filter(|s| !s.is_empty()) {
            self.text(src, Face::Regular, 11.0, R, y + 1.0, Anchor::RightTop, dim);
        }
        y += 28.0;
        let fans: Vec<_> = s.fans.iter().take(5).collect();
        if fans.is_empty() || s.sensor_state != SensorState::Live {
            let msg = match s.sensor_state {
                SensorState::HwinfoNeedsSetup => {
                    "Set up HWiNFO in Vdisplay → System to show fans and CPU temperature"
                }
                SensorState::HwinfoStarting => "Starting HWiNFO sensors…",
                SensorState::HwinfoNoSharedMemory => "Turn on HWiNFO sensors in Vdisplay → System",
                SensorState::NoProvider => {
                    "Install HWiNFO in Vdisplay → System to show fans and CPU temperature"
                }
                _ => "No fan sensors reported",
            };
            for line in self.wrap(msg, Face::Regular, 13.0, R - L, 2) {
                self.text(&line, Face::Regular, 13.0, L, y, Anchor::LeftTop, dim);
                y += 20.0;
            }
            y += 8.0;
        }
        for f in fans {
            let name = self.ellipsize(&f.name, Face::Regular, 15.0, 170.0);
            self.text(
                &name,
                Face::Regular,
                15.0,
                L,
                y,
                Anchor::LeftTop,
                hex(0xD4D4D8),
            );
            if f.rpm < 1.0 {
                self.text("idle", Face::Regular, 15.0, R, y, Anchor::RightTop, dim);
            } else {
                let unit_w = self.text(
                    " rpm",
                    Face::Regular,
                    12.0,
                    R,
                    y + 3.0,
                    Anchor::RightTop,
                    label_col,
                );
                self.text(
                    &thousands(f.rpm),
                    Face::Semibold,
                    15.0,
                    R - unit_w,
                    y,
                    Anchor::RightTop,
                    WHITE,
                );
            }
            y += 28.0;
        }

        // Network
        let y = 778.0;
        self.canvas.hline(L, R, y, hair);
        self.text(
            "NETWORK",
            Face::Semibold,
            12.0,
            L,
            y + 20.0,
            Anchor::LeftTop,
            label_col,
        );
        let up = rate(s.net_up_kbs);
        let uw = self.text(
            &up,
            Face::Semibold,
            14.0,
            R,
            y + 18.0,
            Anchor::RightTop,
            WHITE,
        );
        let aw = self.text(
            "↑ ",
            Face::Regular,
            14.0,
            R - uw,
            y + 18.0,
            Anchor::RightTop,
            label_col,
        );
        let x = R - uw - aw - 22.0;
        let dw = self.text(
            &rate(s.net_down_kbs),
            Face::Semibold,
            14.0,
            x,
            y + 18.0,
            Anchor::RightTop,
            WHITE,
        );
        self.text(
            "↓ ",
            Face::Regular,
            14.0,
            x - dw,
            y + 18.0,
            Anchor::RightTop,
            label_col,
        );

        // Busiest apps
        let y = 834.0;
        self.text(
            "TOP APPS",
            Face::Semibold,
            12.0,
            L,
            y,
            Anchor::LeftTop,
            label_col,
        );
        let mut ry = y + 26.0;
        for (name, cpu, _) in s.top.iter().take(3) {
            let n = self.ellipsize(name, Face::Regular, 13.0, 220.0);
            self.text(
                &n,
                Face::Regular,
                13.0,
                L,
                ry,
                Anchor::LeftTop,
                hex(0xA1A1AA),
            );
            self.text(
                &format!("{cpu:.1}%"),
                Face::Semibold,
                13.0,
                R,
                ry,
                Anchor::RightTop,
                hex(0xD4D4D8),
            );
            ry += 22.0;
        }
    }

    /// Shared header for the black typographic screens: big light clock, date on the right.
    fn header(&mut self) {
        const L: f32 = 28.0;
        const R: f32 = WF - 28.0;
        let now = chrono::Local::now();
        self.text(
            &now.format("%H:%M").to_string(),
            Face::Light,
            56.0,
            L - 3.0,
            26.0,
            Anchor::LeftTop,
            WHITE,
        );
        self.text(
            &now.format("%A").to_string().to_uppercase(),
            Face::Semibold,
            11.0,
            R,
            44.0,
            Anchor::RightTop,
            c((112, 112, 122)),
        );
        self.text(
            &now.format("%d %B").to_string().to_uppercase(),
            Face::Semibold,
            11.0,
            R,
            61.0,
            Anchor::RightTop,
            hex(0xC8C8CE),
        );
        self.canvas.hline(L, R, 112.0, c((28, 28, 33)));
    }

    /// One telemetry block: label + subtitle, big light percentage with a hairline bar, an
    /// optional secondary value on the right (temperature-coloured if `temp` is given).
    #[allow(clippy::too_many_arguments)]
    fn metric_block(
        &mut self,
        y: f32,
        label: &str,
        sub: &str,
        pct: f32,
        right: Option<String>,
        temp: Option<f32>,
        right_caption: &str,
        details: &[String],
    ) {
        const L: f32 = 28.0;
        const R: f32 = WF - 28.0;
        let label_col = c((112, 112, 122));
        let lw = self.text(
            label,
            Face::Semibold,
            12.0,
            L,
            y,
            Anchor::LeftTop,
            label_col,
        );
        let sub = self.ellipsize(sub, Face::Regular, 12.0, R - L - lw - 10.0);
        self.text(
            &sub,
            Face::Regular,
            12.0,
            L + lw + 8.0,
            y,
            Anchor::LeftTop,
            c((84, 84, 94)),
        );

        // Big percentage, baseline-aligned with its "%" and the right-hand value.
        let baseline = y + 76.0;
        let (asc_big, _) = self.fonts.metrics(Face::Light, 60.0);
        let num = format!("{:.0}", pct.clamp(0.0, 100.0));
        let nw = self.text(
            &num,
            Face::Light,
            60.0,
            L - 3.0,
            baseline - asc_big,
            Anchor::LeftTop,
            WHITE,
        );
        let (asc_pc, _) = self.fonts.metrics(Face::Light, 26.0);
        self.text(
            "%",
            Face::Light,
            26.0,
            L - 3.0 + nw + 3.0,
            baseline - asc_pc,
            Anchor::LeftTop,
            c((140, 140, 150)),
        );

        let (asc_r, _) = self.fonts.metrics(Face::Light, 40.0);
        let (rv, rcol) = match (right, temp) {
            (Some(v), Some(t)) if t >= 88.0 => (v, c((255, 76, 88))),
            (Some(v), Some(t)) if t >= 75.0 => (v, c((245, 182, 66))),
            (Some(v), _) => (v, WHITE),
            (None, _) => (String::new(), WHITE),
        };
        if rv.is_empty() {
            let (asc_na, _) = self.fonts.metrics(Face::Semibold, 13.0);
            self.text(
                "N/A",
                Face::Semibold,
                13.0,
                R,
                baseline - asc_na,
                Anchor::RightTop,
                c((70, 70, 80)),
            );
        } else {
            self.text(
                &rv,
                Face::Light,
                40.0,
                R,
                baseline - asc_r,
                Anchor::RightTop,
                rcol,
            );
        }
        self.text(
            right_caption,
            Face::Semibold,
            10.0,
            R,
            baseline + 8.0,
            Anchor::RightTop,
            label_col,
        );

        // Hairline bar.
        let by = baseline + 28.0;
        let bar_r = R - 84.0;
        self.canvas
            .fill_rrect(L, by, bar_r, by + 2.0, 1.0, c((30, 30, 36)));
        let fw = (pct / 100.0).clamp(0.0, 1.0) * (bar_r - L);
        if fw > 0.5 {
            let col = if pct >= 90.0 {
                c((245, 182, 66))
            } else {
                c((236, 236, 240))
            };
            self.canvas.fill_rrect(L, by, L + fw, by + 2.0, 1.0, col);
        }
        if !details.is_empty() {
            let d = details.join("   ·   ");
            let d = self.ellipsize(&d, Face::Regular, 12.0, R - L);
            self.text(
                &d,
                Face::Regular,
                12.0,
                L,
                by + 14.0,
                Anchor::LeftTop,
                c((140, 140, 150)),
            );
        }
    }

    /// Wakes / clears the panel after connecting.
    pub fn startup_test(&mut self, port: &str, fps: u32) {
        self.canvas.clear((0, 0, 0));
        let (r, g, b) = ((255, 30, 56), GREEN, BLUE);
        self.canvas.rrect(
            10.0,
            10.0,
            WF - 10.0,
            H as f32 - 10.0,
            16.0,
            None,
            Some(c(r)),
            2.0,
        );
        self.text(
            "HARDWARE TEST",
            Face::Bold,
            13.0,
            24.0,
            24.0,
            Anchor::LeftTop,
            c(r),
        );
        self.text(
            &clock("%H:%M:%S"),
            Face::Bold,
            18.0,
            WF - 24.0,
            22.0,
            Anchor::RightTop,
            WHITE,
        );
        self.canvas.hline(24.0, WF - 24.0, 52.0, c((35, 35, 45)));
        let cy = 380.0;
        self.canvas.rrect(
            WF / 2.0 - 70.0,
            cy - 80.0,
            WF / 2.0 + 70.0,
            cy - 40.0,
            14.0,
            Some(c((20, 10, 14))),
            Some(c(r)),
            1.0,
        );
        self.text(
            "●  LINK ACTIVE",
            Face::Bold,
            11.0,
            WF / 2.0,
            cy - 60.0,
            Anchor::Center,
            c(r),
        );
        self.text(
            "VDISPLAY",
            Face::Bold,
            32.0,
            WF / 2.0,
            cy - 10.0,
            Anchor::Center,
            WHITE,
        );
        self.text(
            "MC-3TCORELCD ENGINE",
            Face::Bold,
            13.0,
            WF / 2.0,
            cy + 24.0,
            Anchor::Center,
            c(b),
        );
        let cy2 = cy + 70.0;
        self.canvas.rrect(
            24.0,
            cy2,
            WF - 24.0,
            cy2 + 190.0,
            12.0,
            Some(c((12, 12, 16))),
            Some(c((35, 35, 45))),
            1.0,
        );
        self.text(
            "SELF-TEST DIAGNOSTICS",
            Face::Bold,
            11.0,
            36.0,
            cy2 + 16.0,
            Anchor::LeftTop,
            hex(0xA1A1AA),
        );
        let items = [
            ("DEVICE PORT", format!("[{port}] CONNECTED"), c(g)),
            ("PANEL RES", format!("{W} x {H} (PORTRAIT)"), WHITE),
            ("DECODER", "BASELINE 4:2:0 JPEG".to_string(), c(b)),
            ("STREAM TARGET", format!("{fps} FPS FLUID STREAM"), c(g)),
        ];
        let mut iy = cy2 + 46.0;
        for (l, v, col) in items {
            self.text(
                l,
                Face::Regular,
                11.0,
                36.0,
                iy,
                Anchor::LeftTop,
                hex(0x71717A),
            );
            self.text(&v, Face::Bold, 11.0, WF - 36.0, iy, Anchor::RightTop, col);
            iy += 32.0;
        }
        self.text(
            "INITIALIZING LIVE DISPLAY…",
            Face::Bold,
            12.0,
            WF / 2.0,
            880.0,
            Anchor::Center,
            hex(0xA1A1AA),
        );
        self.text(
            "UNFREEZE & SYNC OK",
            Face::Bold,
            11.0,
            WF / 2.0,
            905.0,
            Anchor::Center,
            c(g),
        );
    }

    /// Fullscreen app has focus: same typographic language as the telemetry screen.
    pub fn fullscreen_paused(&mut self, app: &str) {
        const L: f32 = 28.0;
        const R: f32 = WF - 28.0;
        let label_col = c((112, 112, 122));
        let dim = c((84, 84, 94));
        let hair = c((28, 28, 33));
        self.canvas.clear((0, 0, 0));
        self.header();

        // Pause timer (reset when the app changes or the screen wasn't shown for a while).
        let now = Instant::now();
        let since = match &mut self.paused {
            Some((a, since, seen))
                if a == app && seen.elapsed() < std::time::Duration::from_secs(5) =>
            {
                *seen = now;
                *since
            }
            p => {
                *p = Some((app.to_string(), now, now));
                now
            }
        };

        // Status
        let y = 130.0;
        self.text(
            "DISPLAY",
            Face::Semibold,
            12.0,
            L,
            y,
            Anchor::LeftTop,
            label_col,
        );
        let sw = self.text(
            "STANDBY",
            Face::Semibold,
            11.0,
            R,
            y + 1.0,
            Anchor::RightTop,
            c(RED),
        );
        self.canvas.circle(R - sw - 10.0, y + 8.0, 3.0, c(RED));
        let baseline = y + 76.0;
        let (asc, _) = self.fonts.metrics(Face::Light, 60.0);
        self.text(
            "Paused",
            Face::Light,
            60.0,
            L - 3.0,
            baseline - asc,
            Anchor::LeftTop,
            WHITE,
        );
        let mut ty = baseline + 22.0;
        for line in self.wrap(
            "A fullscreen app has focus, so the screen stops rendering to leave every frame to it.",
            Face::Regular,
            14.0,
            R - L,
            3,
        ) {
            self.text(
                &line,
                Face::Regular,
                14.0,
                L,
                ty,
                Anchor::LeftTop,
                c((140, 140, 150)),
            );
            ty += 21.0;
        }

        // Active app + time paused
        let y = 330.0;
        self.canvas.hline(L, R, y, hair);
        let y = y + 20.0;
        self.text(
            "FULLSCREEN APP",
            Face::Semibold,
            12.0,
            L,
            y,
            Anchor::LeftTop,
            label_col,
        );
        let baseline = y + 62.0;
        let pretty = app
            .strip_suffix(".exe")
            .or_else(|| app.strip_suffix(".EXE"))
            .unwrap_or(app);
        let (asc_r, _) = self.fonts.metrics(Face::Light, 40.0);
        let secs = since.elapsed().as_secs();
        let dur = match secs {
            0..60 => format!("{secs}s"),
            60..3600 => format!("{}m", secs / 60),
            _ => format!("{}h {:02}m", secs / 3600, secs / 60 % 60),
        };
        let dw = self.text(
            &dur,
            Face::Light,
            40.0,
            R,
            baseline - asc_r,
            Anchor::RightTop,
            WHITE,
        );
        self.text(
            "PAUSED FOR",
            Face::Semibold,
            10.0,
            R,
            baseline + 8.0,
            Anchor::RightTop,
            label_col,
        );
        let mut size = 40.0;
        while size > 24.0 && self.width(pretty, Face::Light, size) > R - L - dw - 20.0 {
            size -= 2.0;
        }
        let name = self.ellipsize(pretty, Face::Light, size, R - L - dw - 20.0);
        let (asc_n, _) = self.fonts.metrics(Face::Light, size);
        self.text(
            &name,
            Face::Light,
            size,
            L - 2.0,
            baseline - asc_n,
            Anchor::LeftTop,
            WHITE,
        );
        let exe = self.ellipsize(app, Face::Regular, 12.0, R - L - dw - 20.0);
        self.text(
            &exe,
            Face::Regular,
            12.0,
            L,
            baseline + 10.0,
            Anchor::LeftTop,
            dim,
        );

        // What the stream is doing meanwhile
        let y = 470.0;
        self.canvas.hline(L, R, y, hair);
        let mut ry = y + 20.0;
        self.text(
            "STREAM",
            Face::Semibold,
            12.0,
            L,
            ry,
            Anchor::LeftTop,
            label_col,
        );
        ry += 28.0;
        for (k, v) in [
            ("Rendering", "suspended"),
            ("Panel refresh", "every 1.5 s"),
            ("Resumes", "automatically"),
        ] {
            self.text(
                k,
                Face::Regular,
                15.0,
                L,
                ry,
                Anchor::LeftTop,
                hex(0xD4D4D8),
            );
            self.text(v, Face::Semibold, 15.0, R, ry, Anchor::RightTop, WHITE);
            ry += 28.0;
        }

        // Quiet pause glyph in the empty lower half.
        let (gx, gy) = (WF / 2.0, 760.0);
        let glyph = c((30, 30, 36));
        self.canvas
            .fill_rrect(gx - 26.0, gy - 44.0, gx - 10.0, gy + 44.0, 5.0, glyph);
        self.canvas
            .fill_rrect(gx + 10.0, gy - 44.0, gx + 26.0, gy + 44.0, 5.0, glyph);
        self.text(
            "ALT-TAB OR EXIT TO RESUME",
            Face::Semibold,
            10.0,
            WF / 2.0,
            888.0,
            Anchor::Center,
            dim,
        );
    }

    pub fn debug_overlay(&mut self, d: &DebugHud) {
        let y = 920.0;
        self.canvas.rrect(
            18.0,
            y,
            342.0,
            y + 32.0,
            7.0,
            Some(ca((8, 8, 12), 235)),
            Some(c((255, 30, 56))),
            1.0,
        );
        let fps_col = if d.fps >= 55.0 {
            hex(0x22C55E)
        } else if d.fps >= 28.0 {
            hex(0xEAB308)
        } else {
            hex(0xEF4444)
        };
        let f = Face::Bold;
        self.text(
            &format!("{:.1} FPS", d.fps),
            f,
            9.0,
            26.0,
            y + 4.0,
            Anchor::LeftTop,
            fps_col,
        );
        self.text(
            &format!("{:.1} Mbps", d.mbps),
            f,
            9.0,
            92.0,
            y + 4.0,
            Anchor::LeftTop,
            hex(0x38BDF8),
        );
        self.text(
            &format!("{:.1} KB", d.kb),
            f,
            9.0,
            165.0,
            y + 4.0,
            Anchor::LeftTop,
            hex(0xA1A1AA),
        );
        self.text(
            &d.tag,
            f,
            9.0,
            334.0,
            y + 4.0,
            Anchor::RightTop,
            hex(0xA855F7),
        );
        self.text(
            &format!("RND: {:.1}ms", d.rnd_ms),
            f,
            9.0,
            26.0,
            y + 18.0,
            Anchor::LeftTop,
            hex(0xD4D4D8),
        );
        self.text(
            &format!("JPG: {:.1}ms", d.jpg_ms),
            f,
            9.0,
            115.0,
            y + 18.0,
            Anchor::LeftTop,
            hex(0xD4D4D8),
        );
        self.text(
            &format!("TX: {:.1}ms", d.tx_ms),
            f,
            9.0,
            202.0,
            y + 18.0,
            Anchor::LeftTop,
            hex(0xD4D4D8),
        );
        let tot = d.rnd_ms + d.jpg_ms + d.tx_ms;
        let lc = if tot < 16.6 {
            hex(0x22C55E)
        } else {
            hex(0xEAB308)
        };
        self.text(
            &format!("LAT: {tot:.1}ms"),
            f,
            9.0,
            334.0,
            y + 18.0,
            Anchor::RightTop,
            lc,
        );
    }

    /// Built-in solid / gradient patterns for the custom-media slot.
    pub fn preset(&mut self, name: &str) -> image::RgbImage {
        match name {
            "crimson" => {
                self.canvas.clear((255, 30, 56));
                self.text(
                    "OLED CRIMSON",
                    Face::Bold,
                    30.0,
                    WF / 2.0,
                    450.0,
                    Anchor::Center,
                    WHITE,
                );
                self.text(
                    "360 × 960",
                    Face::Semibold,
                    16.0,
                    WF / 2.0,
                    492.0,
                    Anchor::Center,
                    c((60, 0, 10)),
                );
            }
            "black" => {
                self.canvas.clear((0, 0, 0));
                self.text(
                    "TRUE OLED",
                    Face::Bold,
                    22.0,
                    WF / 2.0,
                    470.0,
                    Anchor::Center,
                    c((40, 40, 46)),
                );
            }
            _ => {
                let data = self.canvas.pm.data_mut();
                for y in 0..H as usize {
                    let r = (255.0 * (1.0 - y as f32 / H as f32)) as u8;
                    for x in 0..W as usize {
                        let i = (y * W as usize + x) * 4;
                        data[i..i + 4].copy_from_slice(&[r, 10, 20, 255]);
                    }
                }
                self.text(
                    "MARS GAMING",
                    Face::Bold,
                    30.0,
                    WF / 2.0,
                    450.0,
                    Anchor::Center,
                    WHITE,
                );
                self.text(
                    "CYBER RED",
                    Face::Bold,
                    16.0,
                    WF / 2.0,
                    492.0,
                    Anchor::Center,
                    c((255, 30, 56)),
                );
            }
        }
        let mut rgb = Vec::new();
        self.canvas.to_rgb(&mut rgb);
        image::RgbImage::from_raw(W, H, rgb).unwrap()
    }
}

fn rate(kbs: f32) -> String {
    if kbs >= 1024.0 {
        format!("{:.1} MB/s", kbs / 1024.0)
    } else {
        format!("{kbs:.0} KB/s")
    }
}

fn thousands(v: f32) -> String {
    let n = v.round() as i64;
    if n >= 1000 {
        format!("{},{:03}", n / 1000, n % 1000)
    } else {
        n.to_string()
    }
}
