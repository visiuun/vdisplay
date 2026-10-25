//! State shared between the GUI, tray, stream engine and background workers.

use crate::hw::HwSampler;
use crate::loader::MediaFrames;
use crate::lyrics::LyricsStore;
use crate::media::MediaPoller;
use crate::render::LyricsUi;
use crate::settings::Settings;
use parking_lot::{Mutex, RwLock};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum LinkState {
    #[default]
    Idle,
    /// Streaming requested but the screen isn't plugged in / not found.
    NoDevice,
    Connecting,
    Live,
    /// Live, but suspended because a fullscreen app is in the foreground.
    Paused,
    Error,
}

#[derive(Clone, Default)]
pub struct EngineStatus {
    pub state: LinkState,
    pub port: Option<String>,
    pub message: String,
    pub fullscreen_app: Option<String>,
    pub fps: f32,
    pub kb: f32,
    pub mbps: f32,
    pub rnd_ms: f32,
    pub jpg_ms: f32,
    pub tx_ms: f32,
    pub frames: u64,
    pub lyrics: LyricsUi,
}

pub struct Preview {
    pub seq: u64,
    pub rgb: Vec<u8>,
}

pub struct Shared {
    pub settings: RwLock<Settings>,
    settings_dirty: AtomicBool,
    /// Bumped on every settings change so the engine can cut a long frame wait short.
    pub generation: AtomicU64,
    pub stream: AtomicBool,
    pub gui_visible: AtomicBool,
    pub quit: AtomicBool,
    pub rescan: AtomicBool,
    pub release_port: AtomicBool,
    pub status: Mutex<EngineStatus>,
    pub preview: Mutex<Preview>,
    pub media_file: RwLock<Arc<MediaFrames>>,
    pub media_seq: AtomicU64,
    pub media_message: Mutex<String>,
    pub media: Arc<MediaPoller>,
    pub hw: Arc<HwSampler>,
    pub lyrics: Arc<LyricsStore>,
    pub egui_ctx: Mutex<Option<eframe::egui::Context>>,
}

impl Shared {
    pub fn new(settings: Settings, media_file: MediaFrames) -> Arc<Self> {
        let tokens = crate::lyrics::resolve_tokens(&settings.extra);
        Arc::new(Self {
            stream: AtomicBool::new(settings.auto_stream),
            settings: RwLock::new(settings),
            settings_dirty: AtomicBool::new(false),
            generation: AtomicU64::new(0),
            gui_visible: AtomicBool::new(false),
            quit: AtomicBool::new(false),
            rescan: AtomicBool::new(false),
            release_port: AtomicBool::new(false),
            status: Mutex::new(EngineStatus::default()),
            preview: Mutex::new(Preview {
                seq: 0,
                rgb: Vec::new(),
            }),
            media_message: Mutex::new(media_file.describe()),
            media_file: RwLock::new(Arc::new(media_file)),
            media_seq: AtomicU64::new(1),
            media: MediaPoller::start(),
            hw: HwSampler::start(),
            lyrics: LyricsStore::new(tokens),
            egui_ctx: Mutex::new(None),
        })
    }

    /// Mutate settings; persisted shortly after by the saver thread (debounced).
    pub fn update(&self, f: impl FnOnce(&mut Settings)) {
        f(&mut self.settings.write());
        self.settings_dirty.store(true, Ordering::Release);
        self.poke();
    }

    /// Wakes the engine so a change shows up on the panel immediately.
    pub fn poke(&self) {
        self.generation.fetch_add(1, Ordering::Release);
    }

    pub fn settings(&self) -> Settings {
        self.settings.read().clone()
    }

    pub fn save_now(&self) {
        self.settings_dirty.store(false, Ordering::Release);
        let _ = self.settings.read().save();
    }

    pub fn start_saver(self: &Arc<Self>) {
        let me = self.clone();
        std::thread::Builder::new()
            .name("settings-saver".into())
            .spawn(move || {
                loop {
                    std::thread::sleep(std::time::Duration::from_millis(400));
                    if me.settings_dirty.swap(false, Ordering::AcqRel) {
                        let _ = me.settings.read().save();
                    }
                }
            })
            .ok();
    }

    pub fn set_media(&self, m: MediaFrames) {
        *self.media_message.lock() = m.describe();
        *self.media_file.write() = Arc::new(m);
        self.media_seq.fetch_add(1, Ordering::Release);
        self.poke();
    }

    pub fn repaint(&self) {
        if let Some(c) = self.egui_ctx.lock().as_ref() {
            c.request_repaint();
        }
    }
}
