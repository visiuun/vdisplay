//! The stream engine: renders frames, encodes JPEG and pushes them to the panel with
//! deadline-based pacing. Runs on its own thread, independent of the GUI (which only reads
//! the published preview), so hiding the window never affects the display.

use crate::device::{self, Link, WriteError};
use crate::render::effects::Calibration;
use crate::render::{DebugHud, H, LyricsUi, MusicOpts, Renderer, W};
use crate::settings::{IdleMode, Mode, Settings};
use crate::shared::{LinkState, Shared};
use crate::winutil;
use jpeg_encoder::{ColorType, Encoder, SamplingFactor};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

const VENDOR_APPS: [&str; 2] = ["MARS GAMING.exe", "MythCool.exe"];
const PAUSE_GRACE: Duration = Duration::from_secs(20);

#[derive(Default)]
struct Stats {
    fps: f32,
    kb: f32,
    rnd: f32,
    jpg: f32,
    tx: f32,
    count: u32,
    window_start: Option<Instant>,
    frames: u64,
}

impl Stats {
    fn record(&mut self, rnd: f32, jpg: f32, tx: f32, kb: f32) {
        let ema = |a: &mut f32, v: f32| *a = if *a == 0.0 { v } else { *a * 0.85 + v * 0.15 };
        ema(&mut self.rnd, rnd);
        ema(&mut self.jpg, jpg);
        ema(&mut self.tx, tx);
        ema(&mut self.kb, kb);
        self.count += 1;
        self.frames += 1;
        let start = *self.window_start.get_or_insert_with(Instant::now);
        let dt = start.elapsed().as_secs_f32();
        if dt >= 0.5 {
            self.fps = self.count as f32 / dt;
            self.count = 0;
            self.window_start = Some(Instant::now());
        }
    }

    fn mbps(&self) -> f32 {
        self.kb * 1024.0 * 8.0 * self.fps / 1_000_000.0
    }
}

struct Frame {
    interval: Duration,
    quality: u8,
    tag: String,
}

pub struct Engine {
    sh: Arc<Shared>,
    r: Renderer,
    link: Option<Link>,
    first_frame: bool,
    init_burst: u32,
    timeouts: u32,
    jpeg: Vec<u8>,
    packet: Vec<u8>,
    rgb: Vec<u8>,
    stats: Stats,
    calib: (i32, i32, i32, Calibration),
    gif_idx: usize,
    gif_seq: u64,
    last_playing: Option<(String, Instant)>,
    fs_checked: Option<Instant>,
    fs_app: Option<String>,
    fs_sent: Option<Instant>,
    last_scan: Option<Instant>,
    vendor_killed: bool,
    last_preview: Option<Instant>,
}

impl Engine {
    pub fn spawn(sh: Arc<Shared>) -> std::thread::JoinHandle<()> {
        std::thread::Builder::new()
            .name("stream-engine".into())
            .spawn(move || {
                let mut e = Engine {
                    sh,
                    r: Renderer::new(),
                    link: None,
                    first_frame: true,
                    init_burst: 0,
                    timeouts: 0,
                    jpeg: Vec::with_capacity(128 * 1024),
                    packet: Vec::with_capacity(128 * 1024),
                    rgb: Vec::with_capacity((W * H * 3) as usize),
                    stats: Stats::default(),
                    calib: (100, 100, 0, Calibration::new(100, 100, 0)),
                    gif_idx: 0,
                    gif_seq: 0,
                    last_playing: None,
                    fs_checked: None,
                    fs_app: None,
                    fs_sent: None,
                    last_scan: None,
                    vendor_killed: false,
                    last_preview: None,
                };
                e.run();
            })
            .expect("spawn engine")
    }

    fn set_status(&self, f: impl FnOnce(&mut crate::shared::EngineStatus)) {
        f(&mut self.sh.status.lock());
    }

    /// Sleeps until `deadline`, waking early if settings change or we're shutting down.
    fn wait_until(&self, deadline: Instant) {
        let generation = self.sh.generation.load(Ordering::Acquire);
        loop {
            let now = Instant::now();
            if now >= deadline
                || self.sh.quit.load(Ordering::Relaxed)
                || self.sh.generation.load(Ordering::Acquire) != generation
            {
                return;
            }
            std::thread::sleep((deadline - now).min(Duration::from_millis(50)));
        }
    }

    fn scan_port(&mut self, force: bool) -> Option<String> {
        let due = self
            .last_scan
            .is_none_or(|t| t.elapsed() > Duration::from_secs(2));
        if !(force || due) {
            return self.sh.status.lock().port.clone();
        }
        self.last_scan = Some(Instant::now());
        let p = device::find_port();
        self.set_status(|s| s.port = p.clone());
        p
    }

    fn run(&mut self) {
        let mut next = Instant::now();
        while !self.sh.quit.load(Ordering::Relaxed) {
            let force_scan = self.sh.rescan.swap(false, Ordering::AcqRel);
            if self.sh.release_port.swap(false, Ordering::AcqRel) {
                self.link = None;
                let n = winutil::kill_processes(&VENDOR_APPS);
                if n > 0 {
                    std::thread::sleep(Duration::from_millis(500));
                }
                self.set_status(|s| s.message = format!("Closed {n} vendor process(es)"));
                self.scan_port(true);
            }

            let s = self.sh.settings();
            let streaming = self.sh.stream.load(Ordering::Relaxed);
            let visible = self.sh.gui_visible.load(Ordering::Relaxed);
            // SMTC polling is only needed while something can show the result.
            self.sh
                .media
                .enabled
                .store(streaming || visible, Ordering::Relaxed);

            if !streaming {
                if self.link.take().is_some() {
                    self.stats = Stats::default();
                }
                self.vendor_killed = false;
                self.scan_port(force_scan);
                self.set_status(|st| {
                    st.state = LinkState::Idle;
                    st.fps = 0.0;
                    st.fullscreen_app = None;
                });
                if visible {
                    // Preview-only rendering (nothing is sent), capped at 30 FPS.
                    let f = self.compose(&s);
                    self.finish_pixels(&s, &f, false);
                    self.publish_preview(true);
                    next = (next + f.interval.max(Duration::from_millis(33))).max(Instant::now());
                    self.wait_until(next);
                } else {
                    self.sh.hw.enabled.store(false, Ordering::Relaxed);
                    self.wait_until(Instant::now() + Duration::from_millis(250));
                }
                continue;
            }

            if self.link.is_none() && !self.connect(&s, force_scan) {
                continue;
            }

            if self.init_burst > 0 {
                self.init_burst -= 1;
                let port = self
                    .link
                    .as_ref()
                    .map(|l| l.name.clone())
                    .unwrap_or_default();
                self.r.startup_test(&port, s.music_fps);
                self.finish_pixels(
                    &s,
                    &Frame {
                        interval: Duration::ZERO,
                        quality: 75,
                        tag: String::new(),
                    },
                    true,
                );
                let _ = self.send(true, 75);
                self.publish_preview(false);
                std::thread::sleep(Duration::from_millis(40));
                next = Instant::now();
                continue;
            }

            let link = self.link.as_mut().unwrap();
            link.drain_input();
            if link.queued() > 32_768 {
                // USB transfer buffer backed up: skip a frame rather than add latency.
                std::thread::sleep(Duration::from_millis(2));
                continue;
            }

            if s.pause_on_fullscreen {
                if self
                    .fs_checked
                    .is_none_or(|t| t.elapsed() > Duration::from_millis(250))
                {
                    self.fs_checked = Some(Instant::now());
                    self.fs_app = winutil::fullscreen_app();
                }
            } else {
                self.fs_app = None;
            }
            if let Some(app) = self.fs_app.clone() {
                self.sh.hw.enabled.store(false, Ordering::Relaxed);
                self.set_status(|st| {
                    st.state = LinkState::Paused;
                    st.fullscreen_app = Some(app.clone());
                    st.fps = 0.0;
                });
                if self
                    .fs_sent
                    .is_none_or(|t| t.elapsed() > Duration::from_millis(1500))
                {
                    self.fs_sent = Some(Instant::now());
                    self.r.fullscreen_paused(&app);
                    self.finish_pixels(
                        &s,
                        &Frame {
                            interval: Duration::ZERO,
                            quality: 50,
                            tag: String::new(),
                        },
                        true,
                    );
                    if self.send(self.first_frame, 50).is_ok() {
                        self.first_frame = false;
                    }
                    self.publish_preview(true);
                }
                self.wait_until(Instant::now() + Duration::from_millis(100));
                next = Instant::now();
                continue;
            }
            self.fs_sent = None;

            let t0 = Instant::now();
            let frame = self.compose(&s);
            let rnd = t0.elapsed().as_secs_f32() * 1000.0;
            self.finish_pixels(&s, &frame, true);
            let first = self.first_frame;
            match self.send(first, frame.quality) {
                Ok((jpg, tx, kb)) => {
                    self.first_frame = false;
                    self.timeouts = 0;
                    self.stats.record(rnd, jpg, tx, kb);
                    let st = &self.stats;
                    let (fps, kb, mbps, r, j, t, n) =
                        (st.fps, st.kb, st.mbps(), st.rnd, st.jpg, st.tx, st.frames);
                    let lyr = self.r.report.lyrics;
                    let port = self.link.as_ref().map(|l| l.name.clone());
                    self.set_status(|st| {
                        st.state = LinkState::Live;
                        st.port = port;
                        st.fullscreen_app = None;
                        st.message.clear();
                        st.fps = fps;
                        st.kb = kb;
                        st.mbps = mbps;
                        st.rnd_ms = r;
                        st.jpg_ms = j;
                        st.tx_ms = t;
                        st.frames = n;
                        st.lyrics = lyr;
                    });
                }
                Err(WriteError::Timeout) => {
                    self.timeouts += 1;
                    if let Some(l) = self.link.as_mut() {
                        l.reset_output();
                    }
                    // A partial frame may be on the wire; flag the next one as a resync frame.
                    self.first_frame = true;
                    if self.timeouts >= 3 {
                        self.drop_link("Display stopped responding (write timeout)");
                    } else {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    continue;
                }
                Err(WriteError::Fatal(e)) => {
                    self.drop_link(&format!("Connection lost: {e}"));
                    continue;
                }
            }
            self.publish_preview(false);

            next += frame.interval;
            let now = Instant::now();
            if next < now {
                next = now;
            }
            self.wait_until(next);
        }
        // Shutdown: closing the port lets the panel fall back cleanly.
        self.link = None;
    }

    fn drop_link(&mut self, msg: &str) {
        self.link = None;
        self.timeouts = 0;
        self.set_status(|s| {
            s.state = LinkState::Error;
            s.message = msg.to_string();
            s.fps = 0.0;
        });
        std::thread::sleep(Duration::from_millis(800));
        self.scan_port(true);
    }

    fn connect(&mut self, s: &Settings, force_scan: bool) -> bool {
        if !self.vendor_killed {
            // The vendor software holds the COM port open; close it once per streaming session.
            self.vendor_killed = true;
            if winutil::kill_processes(&VENDOR_APPS) > 0 {
                std::thread::sleep(Duration::from_millis(500));
            }
            self.last_scan = None;
        }
        let Some(port) = self.scan_port(force_scan) else {
            self.set_status(|st| {
                st.state = LinkState::NoDevice;
                st.fps = 0.0;
                st.message = "Waiting for the screen (VID 33C3 / PID F101)…".into();
            });
            if self.sh.gui_visible.load(Ordering::Relaxed) {
                let f = self.compose(s);
                self.finish_pixels(s, &f, false);
                self.publish_preview(true);
            }
            self.wait_until(Instant::now() + Duration::from_millis(1000));
            return false;
        };
        self.set_status(|st| {
            st.state = LinkState::Connecting;
            st.message = format!("Opening {port}…");
        });
        match Link::open(&port) {
            Ok(l) => {
                self.link = Some(l);
                self.first_frame = true;
                self.init_burst = 5;
                self.timeouts = 0;
                self.stats = Stats::default();
                true
            }
            Err(e) => {
                self.set_status(|st| {
                    st.state = LinkState::Error;
                    st.message = format!("Can't open {port}: {e}");
                });
                self.wait_until(Instant::now() + Duration::from_millis(800));
                false
            }
        }
    }

    fn music_quality(fps: u32) -> u8 {
        match fps {
            60 => 76,
            30 => 78,
            _ => 82,
        }
    }

    /// Renders the current screen into the canvas.
    fn compose(&mut self, s: &Settings) -> Frame {
        let opts = MusicOpts {
            bg_style: s.bg_style,
            show_lyrics: s.show_lyrics,
            lyrics_offset_ms: s.lyrics_offset_sec * 1000.0,
        };
        let music_iv = Duration::from_secs_f64(1.0 / s.music_fps.max(1) as f64);
        let hw = |e: &mut Self| {
            e.sh.hw.enabled.store(true, Ordering::Relaxed);
            let st = e.sh.hw.stats();
            e.r.hardware(&st);
            e.r.report = Default::default();
            Frame {
                interval: Duration::from_secs(1),
                quality: 82,
                tag: "HARDWARE".into(),
            }
        };
        match s.current_mode {
            Mode::Hardware => hw(self),
            Mode::MediaFile => {
                self.sh.hw.enabled.store(false, Ordering::Relaxed);
                self.media_frame(s)
            }
            Mode::Media => {
                let info = self.sh.media.latest();
                if let Some(i) = info.as_ref().filter(|i| i.is_playing) {
                    self.last_playing = Some((i.track_key(), Instant::now()));
                }
                // Briefly paused tracks stay on screen (with a PAUSED badge) instead of
                // flipping straight to the idle screen.
                let active = info.as_ref().filter(|i| {
                    i.is_playing
                        || self
                            .last_playing
                            .as_ref()
                            .is_some_and(|(k, t)| *k == i.track_key() && t.elapsed() < PAUSE_GRACE)
                });
                match active {
                    Some(i) => {
                        self.sh.hw.enabled.store(false, Ordering::Relaxed);
                        let pos = i.position_now();
                        let lyrics = self.sh.lyrics.clone();
                        self.r.music(Some(i), pos, &opts, &lyrics);
                        let tag = match self.r.report.lyrics {
                            _ if !s.show_lyrics => "MEDIA".to_string(),
                            LyricsUi::WordSynced(_) | LyricsUi::Synced(_) => {
                                let base = if self.r.report.word_sync {
                                    "WORD SYNC"
                                } else {
                                    "SYNCED"
                                };
                                if s.lyrics_offset_sec != 0.0 {
                                    format!("{base} ({:+}s)", s.lyrics_offset_sec)
                                } else {
                                    base.into()
                                }
                            }
                            _ => "MEDIA".into(),
                        };
                        let interval = if i.is_playing {
                            music_iv
                        } else {
                            music_iv.max(Duration::from_millis(66))
                        };
                        Frame {
                            interval,
                            quality: Self::music_quality(s.music_fps),
                            tag,
                        }
                    }
                    None => match s.idle_mode {
                        IdleMode::Hardware => hw(self),
                        IdleMode::MediaFile => {
                            self.sh.hw.enabled.store(false, Ordering::Relaxed);
                            self.media_frame(s)
                        }
                        IdleMode::Standby => {
                            self.sh.hw.enabled.store(false, Ordering::Relaxed);
                            self.r.standby();
                            self.r.report = Default::default();
                            Frame {
                                interval: Duration::from_secs(1),
                                quality: 82,
                                tag: "STANDBY".into(),
                            }
                        }
                    },
                }
            }
        }
    }

    fn media_frame(&mut self, s: &Settings) -> Frame {
        self.r.report = Default::default();
        let seq = self.sh.media_seq.load(Ordering::Acquire);
        if seq != self.gif_seq {
            self.gif_seq = seq;
            self.gif_idx = 0;
        }
        let media = self.sh.media_file.read().clone();
        if media.frames.is_empty() {
            self.r.standby();
            return Frame {
                interval: Duration::from_secs(1),
                quality: 82,
                tag: "MEDIA FILE".into(),
            };
        }
        let i = self.gif_idx % media.frames.len();
        self.r.canvas.put_rgb(media.frames[i].as_raw());
        self.gif_idx = (i + 1) % media.frames.len();
        let interval = if media.frames.len() == 1 {
            Duration::from_secs(1) // still image: keep-alive refresh only
        } else {
            match s.gif_fps {
                Some(f) => Duration::from_secs_f64(1.0 / f as f64),
                None => Duration::from_millis(media.delays_ms[i].max(16) as u64),
            }
        };
        let fps = (1.0 / interval.as_secs_f64()).round() as u32;
        Frame {
            interval,
            quality: Self::music_quality(fps),
            tag: if media.frames.len() > 1 {
                "GIF".into()
            } else {
                "IMAGE".into()
            },
        }
    }

    /// Calibration + optional HUD, then export to RGB.
    fn finish_pixels(&mut self, s: &Settings, f: &Frame, hud: bool) {
        if (self.calib.0, self.calib.1, self.calib.2) != (s.brightness, s.saturation, s.hue) {
            self.calib = (
                s.brightness,
                s.saturation,
                s.hue,
                Calibration::new(s.brightness, s.saturation, s.hue),
            );
        }
        if !self.calib.3.is_identity() {
            self.calib.3.apply_rgba(self.r.canvas.pm.data_mut());
        }
        if hud && s.show_debug_overlay && !f.tag.is_empty() {
            let st = &self.stats;
            let d = DebugHud {
                fps: st.fps,
                kb: st.kb,
                mbps: st.mbps(),
                rnd_ms: st.rnd,
                jpg_ms: st.jpg,
                tx_ms: st.tx,
                tag: f.tag.clone(),
            };
            self.r.debug_overlay(&d);
        }
        self.r.canvas.to_rgb(&mut self.rgb);
    }

    /// Encodes `self.rgb` and writes it. Returns (jpeg ms, tx ms, KB).
    fn send(&mut self, first: bool, quality: u8) -> Result<(f32, f32, f32), WriteError> {
        let t0 = Instant::now();
        self.jpeg.clear();
        let mut enc = Encoder::new(&mut self.jpeg, quality);
        enc.set_sampling_factor(SamplingFactor::F_2_2);
        enc.encode(&self.rgb, W as u16, H as u16, ColorType::Rgb)
            .map_err(|e| WriteError::Fatal(e.to_string()))?;
        let jpg = t0.elapsed().as_secs_f32() * 1000.0;
        device::build_frame(&self.jpeg, first, &mut self.packet);
        let t1 = Instant::now();
        let Some(link) = self.link.as_mut() else {
            return Err(WriteError::Fatal("not connected".into()));
        };
        link.write(&self.packet)?;
        Ok((
            jpg,
            t1.elapsed().as_secs_f32() * 1000.0,
            self.jpeg.len() as f32 / 1024.0,
        ))
    }

    fn publish_preview(&mut self, idle: bool) {
        if !self.sh.gui_visible.load(Ordering::Relaxed) {
            return;
        }
        if self
            .last_preview
            .is_some_and(|t| t.elapsed() < Duration::from_millis(33))
        {
            return;
        }
        self.last_preview = Some(Instant::now());
        {
            let mut p = self.sh.preview.lock();
            std::mem::swap(&mut p.rgb, &mut self.rgb);
            p.seq += 1;
        }
        if idle {
            self.sh.status.lock().lyrics = self.r.report.lyrics;
        }
        self.sh.repaint();
    }
}
