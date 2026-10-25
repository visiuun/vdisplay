//! Windows media session (SMTC) poller + online cover-art fallback.

use crate::{lyrics, net, winutil};
use image::RgbImage;
use parking_lot::Mutex;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use windows::Media::Control::{
    GlobalSystemMediaTransportControlsSession as Session,
    GlobalSystemMediaTransportControlsSessionManager as Manager,
    GlobalSystemMediaTransportControlsSessionPlaybackStatus as Status,
};
use windows::Storage::Streams::DataReader;

static NEXT_ART_ID: AtomicU64 = AtomicU64::new(1);

/// Decoded cover art. `id` is unique per decode, so caches never confuse two covers.
pub struct Art {
    pub id: u64,
    pub img: RgbImage,
}

impl Art {
    pub fn decode(bytes: &[u8]) -> Option<Arc<Art>> {
        let img = image::load_from_memory(bytes).ok()?.into_rgb8();
        Some(Self::from_rgb(img))
    }

    pub fn from_rgb(mut img: RgbImage) -> Arc<Art> {
        let max = img.width().max(img.height());
        if max > 640 {
            let s = 640.0 / max as f32;
            img = image::imageops::resize(
                &img,
                ((img.width() as f32 * s) as u32).max(1),
                ((img.height() as f32 * s) as u32).max(1),
                image::imageops::FilterType::Triangle,
            );
        }
        Arc::new(Art {
            id: NEXT_ART_ID.fetch_add(1, Ordering::Relaxed),
            img,
        })
    }
}

#[derive(Clone)]
pub enum AppIcon {
    Spotify,
    Tidal,
    AppleMusic,
    Generic,
    /// Real icon pulled from the player's executable (RGBA, square).
    Extracted(Arc<image::RgbaImage>),
}

#[derive(Clone)]
pub struct MediaInfo {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub source_app: String,
    pub art: Option<Arc<Art>>,
    pub icon: AppIcon,
    pub position_ms: f64,
    pub duration_ms: f64,
    pub is_playing: bool,
    pub polled_at: Instant,
}

impl MediaInfo {
    /// Playback position extrapolated to "now".
    pub fn position_now(&self) -> f64 {
        let mut p = self.position_ms;
        if self.is_playing {
            p += self.polled_at.elapsed().as_secs_f64() * 1000.0;
        }
        if self.duration_ms > 0.0 {
            p.clamp(0.0, self.duration_ms)
        } else {
            p.max(0.0)
        }
    }

    pub fn track_key(&self) -> String {
        format!("{}|{}", self.title, self.artist)
    }
}

// Online cover art (Deezer, then iTunes)

pub struct CoverStore {
    cache: Mutex<HashMap<String, Option<Arc<Art>>>>,
    fetching: Mutex<HashSet<String>>,
}

impl CoverStore {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            cache: Mutex::new(HashMap::new()),
            fetching: Mutex::new(HashSet::new()),
        })
    }

    /// `Some(result)` once a lookup has finished (result may be `None` = nothing found).
    fn get(self: &Arc<Self>, title: &str, artist: &str) -> Option<Option<Arc<Art>>> {
        if title.is_empty() {
            return Some(None);
        }
        let (t, a) = lyrics::clean_metadata(title, artist);
        let key = format!("{}|{}", t.to_lowercase(), a.to_lowercase());
        if let Some(v) = self.cache.lock().get(&key) {
            return Some(v.clone());
        }
        if self.fetching.lock().insert(key.clone()) {
            let me = self.clone();
            std::thread::Builder::new()
                .name("cover-fetch".into())
                .spawn(move || {
                    let art = fetch_cover(&t, &a);
                    let mut c = me.cache.lock();
                    if c.len() > 100 {
                        let drop_keys: Vec<_> = c.keys().take(25).cloned().collect();
                        for k in drop_keys {
                            c.remove(&k);
                        }
                    }
                    c.insert(key.clone(), art);
                    drop(c);
                    me.fetching.lock().remove(&key);
                })
                .ok();
        }
        None
    }
}

fn fetch_cover(title: &str, artist: &str) -> Option<Arc<Art>> {
    let q = if artist.is_empty() {
        title.to_string()
    } else {
        format!("artist:\"{artist}\" track:\"{title}\"")
    };
    if let Some(v) = net::get_json(
        &format!("https://api.deezer.com/search?q={}&limit=1", net::enc(&q)),
        &[],
    ) {
        let alb = v
            .get("data")
            .and_then(|d| d.get(0))
            .and_then(|d| d.get("album"));
        if let Some(alb) = alb {
            let url = ["cover_xl", "cover_big", "cover_medium"]
                .iter()
                .find_map(|k| {
                    alb.get(*k)
                        .and_then(|u| u.as_str())
                        .filter(|u| !u.is_empty())
                });
            if let Some(art) = url.and_then(net::get_bytes).and_then(|b| Art::decode(&b)) {
                return Some(art);
            }
        }
    }
    let term = format!("{title} {artist}");
    let v = net::get_json(
        &format!(
            "https://itunes.apple.com/search?term={}&entity=song&limit=1",
            net::enc(term.trim())
        ),
        &[],
    )?;
    let url = v
        .get("results")?
        .get(0)?
        .get("artworkUrl100")?
        .as_str()?
        .replace("100x100bb", "600x600bb");
    Art::decode(&net::get_bytes(&url)?)
}

// Demo track (`VDISPLAY_DEMO=1`): fixed fictional media for screenshots and testing.

pub const DEMO_LRC: &str = "[00:52.00]Streetlights hum a quiet tune
[00:56.00]We chase the static to the moon
[01:00.00]Every signal <01:01.00>fading <01:01.50>into blue (oh-oh)
[01:04.00]Hold the frequency for me
[01:08.00]Glass horizon, set us free
[01:12.00]Neon rivers running slow
[01:16.00]Tell me where the night will go
[01:20.00]We were made of afterglow";

pub fn demo_enabled() -> bool {
    std::env::var_os("VDISPLAY_DEMO").is_some_and(|v| v == "1")
}

pub fn demo_track(position_ms: f64) -> MediaInfo {
    static ART: std::sync::OnceLock<Arc<Art>> = std::sync::OnceLock::new();
    let art = ART.get_or_init(|| {
        Art::from_rgb(RgbImage::from_fn(600, 600, |x, y| {
            let (fx, fy) = (x as f32 / 600.0, y as f32 / 600.0);
            image::Rgb([
                (230.0 * (1.0 - fy) + 20.0) as u8,
                (60.0 + 120.0 * fx * fy) as u8,
                (200.0 * fx + 30.0) as u8,
            ])
        }))
    });
    MediaInfo {
        title: "Glass Horizon (Extended Mix)".into(),
        artist: "Nova Arcade".into(),
        album: "Afterglow Transit".into(),
        source_app: "Spotify.exe".into(),
        art: Some(art.clone()),
        icon: AppIcon::Spotify,
        position_ms,
        duration_ms: 243_000.0,
        is_playing: true,
        polled_at: Instant::now(),
    }
}

// SMTC poller

pub struct MediaPoller {
    latest: Mutex<Option<MediaInfo>>,
    pub covers: Arc<CoverStore>,
    pub enabled: AtomicBool,
}

struct TrackState {
    key: String,
    art_bytes: Option<Vec<u8>>,
    prev_art_bytes: Option<Vec<u8>>,
    art: Option<Arc<Art>>,
    verified: bool,
    changed_at: Instant,
}

impl MediaPoller {
    pub fn start() -> Arc<Self> {
        let me = Arc::new(Self {
            latest: Mutex::new(None),
            covers: CoverStore::new(),
            enabled: AtomicBool::new(true),
        });
        let m = me.clone();
        std::thread::Builder::new()
            .name("smtc-poller".into())
            .spawn(move || m.run())
            .ok();
        me
    }

    pub fn latest(&self) -> Option<MediaInfo> {
        self.latest.lock().clone()
    }

    fn run(self: Arc<Self>) {
        let mut manager: Option<Manager> = None;
        let mut st = TrackState {
            key: String::new(),
            art_bytes: None,
            prev_art_bytes: None,
            art: None,
            verified: false,
            changed_at: Instant::now(),
        };
        let mut icons: HashMap<String, AppIcon> = HashMap::new();
        if demo_enabled() {
            let t0 = Instant::now();
            loop {
                let pos = 50_000.0 + (t0.elapsed().as_secs_f64() * 1000.0) % 40_000.0;
                *self.latest.lock() = Some(demo_track(pos));
                std::thread::sleep(Duration::from_millis(250));
            }
        }
        loop {
            if !self.enabled.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(500));
                continue;
            }
            if manager.is_none() {
                manager = Manager::RequestAsync().and_then(|op| op.join()).ok();
            }
            let info = match &manager {
                Some(m) => self.poll(m, &mut st, &mut icons),
                None => Err(windows::core::Error::empty()),
            };
            match info {
                Ok(i) => *self.latest.lock() = i,
                Err(_) => {
                    // The manager can go stale (e.g. explorer restart); recreate it next time.
                    manager = None;
                    *self.latest.lock() = None;
                }
            }
            std::thread::sleep(Duration::from_millis(250));
        }
    }

    fn pick_session(m: &Manager) -> windows::core::Result<Option<Session>> {
        let preferred = ["tidal", "spotify", "applemusic", "music"];
        let mut best: Option<Session> = None;
        for s in m.GetSessions()? {
            let playing = s
                .GetPlaybackInfo()
                .and_then(|p| p.PlaybackStatus())
                .is_ok_and(|p| p == Status::Playing);
            if !playing {
                continue;
            }
            let id = s
                .SourceAppUserModelId()
                .map(|h| h.to_string().to_lowercase())
                .unwrap_or_default();
            if preferred.iter().any(|p| id.contains(p)) {
                return Ok(Some(s));
            }
            if best.is_none() {
                best = Some(s);
            }
        }
        if best.is_some() {
            return Ok(best);
        }
        Ok(m.GetCurrentSession().ok())
    }

    fn poll(
        &self,
        m: &Manager,
        st: &mut TrackState,
        icons: &mut HashMap<String, AppIcon>,
    ) -> windows::core::Result<Option<MediaInfo>> {
        let Some(session) = Self::pick_session(m)? else {
            return Ok(None);
        };
        let props = session.TryGetMediaPropertiesAsync()?.join()?;
        let title = props.Title().map(|s| s.to_string()).unwrap_or_default();
        if title.trim().is_empty() {
            return Ok(None);
        }
        let artist = props.Artist().map(|s| s.to_string()).unwrap_or_default();
        let album = props
            .AlbumTitle()
            .map(|s| s.to_string())
            .unwrap_or_default();
        let source_app = session
            .SourceAppUserModelId()
            .map(|s| s.to_string())
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "MEDIA".into());

        let key = format!("{title}_{artist}_{album}");
        if key != st.key {
            st.key = key;
            st.prev_art_bytes = st.art_bytes.take().or(st.prev_art_bytes.take());
            st.art = None;
            st.verified = false;
            st.changed_at = Instant::now();
        }

        if !st.verified
            && let Some(bytes) = read_thumbnail(&props)
        {
            // Players often report the *previous* track's cover for a moment after skipping.
            let stale = st.prev_art_bytes.as_deref() == Some(bytes.as_slice());
            if !stale || st.changed_at.elapsed() > Duration::from_secs(3) {
                if let Some(a) = Art::decode(&bytes) {
                    st.art = Some(a);
                    st.verified = true;
                }
                st.art_bytes = Some(bytes);
            }
        }
        if !st.verified {
            match self.covers.get(&title, &artist) {
                Some(Some(a)) => {
                    st.art = Some(a);
                    st.verified = true;
                }
                Some(None) if st.changed_at.elapsed() > Duration::from_secs(5) => {
                    st.verified = true
                }
                _ => {}
            }
        }

        let is_playing = session
            .GetPlaybackInfo()
            .and_then(|p| p.PlaybackStatus())
            .is_ok_and(|p| p == Status::Playing);
        let (mut position_ms, mut duration_ms) = (0.0, 0.0);
        if let Ok(tl) = session.GetTimelineProperties() {
            let pos = tl
                .Position()
                .map(|t| t.Duration as f64 / 10_000.0)
                .unwrap_or(0.0);
            let start = tl
                .StartTime()
                .map(|t| t.Duration as f64 / 10_000.0)
                .unwrap_or(0.0);
            duration_ms = tl
                .EndTime()
                .map(|t| t.Duration as f64 / 10_000.0)
                .unwrap_or(0.0)
                - start;
            position_ms = pos - start;
            if is_playing && let Ok(updated) = tl.LastUpdatedTime() {
                let elapsed = (now_filetime() - updated.UniversalTime) as f64 / 10_000.0;
                if elapsed > 0.0 && elapsed < 6.0 * 3600.0 * 1000.0 {
                    position_ms += elapsed;
                }
            }
            position_ms = if duration_ms > 0.0 {
                position_ms.clamp(0.0, duration_ms)
            } else {
                position_ms.max(0.0)
            };
        }

        let icon = icons
            .entry(source_app.clone())
            .or_insert_with(|| resolve_icon(&source_app))
            .clone();

        Ok(Some(MediaInfo {
            title,
            artist,
            album,
            source_app,
            art: st.art.clone(),
            icon,
            position_ms,
            duration_ms: duration_ms.max(0.0),
            is_playing,
            polled_at: Instant::now(),
        }))
    }
}

fn now_filetime() -> i64 {
    // 100 ns intervals since 1601-01-01 (WinRT DateTime epoch).
    let d = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    (d.as_nanos() / 100) as i64 + 116_444_736_000_000_000
}

fn read_thumbnail(
    props: &windows::Media::Control::GlobalSystemMediaTransportControlsSessionMediaProperties,
) -> Option<Vec<u8>> {
    let r = props.Thumbnail().ok()?;
    let stream = r.OpenReadAsync().ok()?.join().ok()?;
    let size = stream.Size().ok()?;
    if size == 0 || size > 16 * 1024 * 1024 {
        return None;
    }
    let input = stream.GetInputStreamAt(0).ok()?;
    let reader = DataReader::CreateDataReader(&input).ok()?;
    let loaded = reader.LoadAsync(size as u32).ok()?.join().ok()?;
    let mut buf = vec![0u8; loaded as usize];
    reader.ReadBytes(&mut buf).ok()?;
    let _ = reader.Close();
    let _ = stream.Close();
    (!buf.is_empty()).then_some(buf)
}

/// Brand icon for the well-known players, otherwise the real icon of the player's executable.
/// Only the session's own app is considered, so a browser tab never gets a Spotify logo.
fn resolve_icon(source_app: &str) -> AppIcon {
    let key = source_app.to_lowercase();
    if key.contains("spotify") {
        return AppIcon::Spotify;
    }
    if key.contains("tidal") {
        return AppIcon::Tidal;
    }
    if key.contains("apple") || key.contains("itunes") {
        return AppIcon::AppleMusic;
    }
    match winutil::find_process_exe(&key).and_then(|p| winutil::extract_exe_icon(&p, 64)) {
        Some(img) => AppIcon::Extracted(Arc::new(img)),
        None => AppIcon::Generic,
    }
}
