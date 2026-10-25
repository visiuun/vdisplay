//! Synchronized lyrics: Radiant Lyrics (per-word TTML JSON) with LRCLIB as fallback.

use crate::net;
use parking_lot::Mutex;
use regex::Regex;
use serde_json::{Map, Value};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};

#[derive(Clone, Debug)]
pub struct Syllable {
    pub text: String,
    pub start: i64,
    pub dur: i64,
    pub end: i64,
    pub bg: bool,
    /// `text` with parenthesised ad-libs removed (pre-computed).
    pub clean: String,
}

#[derive(Clone, Debug)]
pub struct Line {
    pub text: String,
    pub start: i64,
    pub dur: i64,
    pub end: i64,
    pub syls: Vec<Syllable>,
    pub has_syls: bool,
    pub bg: bool,
    pub part: String,
    pub singer: String,
    // Pre-computed once so the 60 FPS renderer never runs a regex.
    pub clean_text: String,
    pub has_text_adlib: bool,
    pub adlib_text: Option<String>,
    pub pure_adlib: bool,
    pub right: bool,
}

#[derive(Debug)]
pub struct Lyrics {
    pub lines: Vec<Line>,
    pub starts: Vec<i64>,
    pub source: &'static str,
    pub word_synced: bool,
}

impl Lyrics {
    fn new(mut lines: Vec<Line>, source: &'static str) -> Option<Arc<Self>> {
        if lines.is_empty() {
            return None;
        }
        lines.sort_by_key(|l| l.start);
        for l in &mut lines {
            l.clean_text = strip_parens(&l.text);
            l.has_text_adlib = RE_HAS_PAREN.is_match(&l.text);
            l.adlib_text = RE_ADLIB
                .captures(&l.text)
                .map(|c| format!("({})", c[1].trim()));
            let t = l.text.trim();
            l.pure_adlib = t.starts_with('(') && t.ends_with(')');
            l.right = is_right_singer(&l.singer);
            if l.syls.iter().any(|s| mixed_brackets(&s.text)) {
                l.syls = l.syls.iter().flat_map(split_brackets).collect();
            }
            for s in &mut l.syls {
                s.clean = strip_parens(&s.text);
            }
        }
        let starts = lines.iter().map(|l| l.start).collect();
        let word_synced = lines.iter().any(|l| l.has_syls);
        Some(Arc::new(Self {
            lines,
            starts,
            source,
            word_synced,
        }))
    }
}

/// True when a timed chunk holds both normal words and a bracketed ad-lib.
fn mixed_brackets(t: &str) -> bool {
    let t = t.trim();
    (t.contains('(') || t.contains('['))
        && !((t.starts_with('(') && t.ends_with(')')) || (t.starts_with('[') && t.ends_with(']')))
}

/// Splits a chunk like "into blue (oh-oh)" at its brackets so the lead words and the ad-lib
/// can be laid out separately. Timing is shared out by character count.
fn split_brackets(s: &Syllable) -> Vec<Syllable> {
    if !mixed_brackets(&s.text) {
        return vec![s.clone()];
    }
    let mut pieces = Vec::new();
    let mut cur = String::new();
    for ch in s.text.chars() {
        if matches!(ch, '(' | '[') && !cur.trim().is_empty() {
            pieces.push(std::mem::take(&mut cur));
        }
        cur.push(ch);
        if matches!(ch, ')' | ']') {
            pieces.push(std::mem::take(&mut cur));
        }
    }
    if !cur.is_empty() {
        match pieces.last_mut() {
            Some(last) if cur.trim().is_empty() => last.push_str(&cur),
            _ => pieces.push(cur),
        }
    }
    let total = pieces
        .iter()
        .map(|p| p.trim().chars().count())
        .sum::<usize>()
        .max(1) as i64;
    let mut t = s.start;
    let n = pieces.len();
    pieces
        .into_iter()
        .enumerate()
        .map(|(i, text)| {
            let share = s.dur * text.trim().chars().count() as i64 / total;
            let end = if i + 1 == n { s.end } else { t + share };
            let piece = Syllable {
                text,
                start: t,
                dur: end - t,
                end,
                bg: s.bg,
                clean: String::new(),
            };
            t = end;
            piece
        })
        .collect()
}

pub fn is_right_singer(s: &str) -> bool {
    matches!(
        s.to_ascii_lowercase().as_str(),
        "v2" | "singer2" | "right" | "guest" | "female"
    )
}

/// Scripts that are written without spaces and must wrap per character.
pub fn is_char_wrap_char(c: char) -> bool {
    let c = c as u32;
    (0x0E00..=0x0EFF).contains(&c)        // Thai / Lao
        || (0x2E80..=0x9FFF).contains(&c) // CJK radicals, kana, ideographs
        || (0xF900..=0xFAFF).contains(&c)
        || (0xFF00..=0xFFEF).contains(&c)
        || (0x20000..=0x2FFFF).contains(&c)
}

pub fn needs_char_wrap(s: &str) -> bool {
    s.chars().any(is_char_wrap_char)
}

static RE_PAREN_STRIP: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\s*[\(\[][^)\]]*[\)\]]").unwrap());
static RE_HAS_PAREN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[\(\[][^)\]]+[\)\]]").unwrap());
static RE_ADLIB: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[\(\[]([^)\]^]+)[\)\]]").unwrap());
static RE_WS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s+").unwrap());

pub fn strip_parens(s: &str) -> String {
    RE_PAREN_STRIP.replace_all(s, "").into_owned()
}

// Metadata cleanup

static RE_TOPIC: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\s*-\s*topic$").unwrap());
static RE_VEVO: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\s*vevo$").unwrap());
static RE_BRACKETS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\[.*?\]|\(.*?\)").unwrap());
static RE_JUNK: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(feat|ft|prod|official audio|lyrics|official video|music video|hd|4k)\b.*")
        .unwrap()
});
static RE_ZW: LazyLock<Regex> =
    LazyLock::new(|| Regex::new("[\u{200b}-\u{200d}\u{feff}]").unwrap());

/// Isolates the real track title and primary artist from stream artefacts
/// ("Artist - Topic", "Artist - Title [Official Video]", ...).
pub fn clean_metadata(raw_title: &str, raw_artist: &str) -> (String, String) {
    let mut title = raw_title.trim().to_string();
    let mut author = raw_artist.trim().to_string();
    author = RE_TOPIC.replace(&author, "").trim().to_string();
    author = RE_VEVO.replace(&author, "").trim().to_string();

    if let Some((a, t)) = title.split_once(" - ") {
        let (a, t) = (a.trim().to_string(), t.trim().to_string());
        if !a.is_empty() && !t.is_empty() {
            let generic = [
                "various artists",
                "unknown artist",
                "youtube",
                "soundcloud",
                "media",
                "spotify",
                "tidal",
                "apple music",
            ];
            if author.is_empty() || generic.contains(&author.to_lowercase().as_str()) {
                author = a;
            }
            title = t;
        }
    }
    let t2 = RE_BRACKETS.replace_all(&title, "");
    let t3 = RE_JUNK.replace(&t2, "");
    let title = RE_ZW.replace_all(&t3, "").trim().to_string();
    let author = RE_ZW.replace_all(&author, "").trim().to_string();
    // Never return an empty title just because the whole thing was in brackets.
    if title.is_empty() {
        return (raw_title.trim().to_string(), author);
    }
    (title, author)
}

// Parsing

fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

fn vstr(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// First key whose value is present and not null.
fn first_present<'a>(o: &'a Map<String, Value>, keys: &[&str]) -> Option<&'a Value> {
    keys.iter().filter_map(|k| o.get(*k)).find(|v| !v.is_null())
}

/// First key whose value is truthy (non-empty, non-zero).
fn first_truthy<'a>(o: &'a Map<String, Value>, keys: &[&str]) -> Option<&'a Value> {
    keys.iter().filter_map(|k| o.get(*k)).find(|v| truthy(v))
}

pub fn parse_time_ms(v: &Value) -> i64 {
    match v {
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                i // integers are already milliseconds
            } else {
                let f = n.as_f64().unwrap_or(0.0);
                if f < 100000.0 {
                    (f * 1000.0).round() as i64
                } else {
                    f.round() as i64
                }
            }
        }
        Value::String(s) => parse_time_str(s),
        _ => 0,
    }
}

fn parse_time_str(s: &str) -> i64 {
    let s = s.trim();
    if s.is_empty() {
        return 0;
    }
    let lower = s.to_ascii_lowercase();
    if let Some(n) = lower.strip_suffix("ms") {
        return n.trim().parse::<f64>().map(|f| f as i64).unwrap_or(0);
    }
    if let Some(n) = lower.strip_suffix('s') {
        return n
            .trim()
            .parse::<f64>()
            .map(|f| (f * 1000.0).round() as i64)
            .unwrap_or(0);
    }
    if s.contains(':') {
        let p: Vec<f64> = match s.split(':').map(|x| x.trim().parse::<f64>()).collect() {
            Ok(v) => v,
            Err(_) => return 0,
        };
        return match p.len() {
            2 => ((p[0] * 60.0 + p[1]) * 1000.0).round() as i64,
            3 => ((p[0] * 3600.0 + p[1] * 60.0 + p[2]) * 1000.0).round() as i64,
            _ => 0,
        };
    }
    match s.parse::<f64>() {
        Ok(v) if s.contains('.') || v < 1000.0 => (v * 1000.0).round() as i64,
        Ok(v) => v.round() as i64,
        Err(_) => 0,
    }
}

fn is_bg_singer(s: &str) -> bool {
    matches!(
        s.to_ascii_lowercase().as_str(),
        "background" | "bg" | "backing" | "adlib"
    )
}

fn blank_line(text: String, start: i64, dur: i64, syls: Vec<Syllable>, has_syls: bool) -> Line {
    Line {
        text,
        start,
        dur,
        end: start + dur,
        syls,
        has_syls,
        bg: false,
        part: String::new(),
        singer: "v1".into(),
        clean_text: String::new(),
        has_text_adlib: false,
        adlib_text: None,
        pure_adlib: false,
        right: false,
    }
}

pub fn parse_ttml_json(data: &Value) -> Option<Arc<Lyrics>> {
    let empty = Vec::new();
    let mut parts_map: HashMap<String, String> = HashMap::new();
    let items: &Vec<Value> = match data {
        Value::Object(o) => {
            if let Some(sp) = o
                .get("metadata")
                .and_then(|m| m.get("songParts"))
                .and_then(|v| v.as_array())
            {
                for p in sp {
                    if let (Some(idx), Some(name)) =
                        (p.get("divIndex"), p.get("name").and_then(|n| n.as_str()))
                        && !idx.is_null()
                        && !name.is_empty()
                    {
                        parts_map.insert(idx.to_string(), name.to_string());
                    }
                }
            }
            ["data", "lines", "lyrics", "ttml"]
                .iter()
                .find_map(|k| o.get(*k).and_then(|v| v.as_array()))
                .unwrap_or(&empty)
        }
        Value::Array(a) => a,
        _ => &empty,
    };

    let null_obj = Map::new();
    let mut lines = Vec::new();
    for item in items {
        let Some(item) = item.as_object() else {
            continue;
        };
        let elem = item
            .get("element")
            .and_then(|e| e.as_object())
            .unwrap_or(&null_obj);
        let singer = elem
            .get("singer")
            .filter(|v| truthy(v))
            .or_else(|| item.get("singer").filter(|v| truthy(v)))
            .map(vstr)
            .unwrap_or_else(|| "v1".into())
            .trim()
            .to_string();
        let mut part = first_truthy(elem, &["part"])
            .or_else(|| first_truthy(item, &["part"]))
            .map(vstr)
            .unwrap_or_default()
            .trim()
            .to_string();
        if part.is_empty()
            && let Some(idx) = elem.get("songPartIndex").filter(|v| !v.is_null())
            && let Some(n) = parts_map.get(&idx.to_string())
        {
            part = n.clone();
        }
        let line_bg = item.get("isBackground").is_some_and(truthy)
            || elem.get("isBackground").is_some_and(truthy)
            || is_bg_singer(&singer);

        let mut line_text = first_truthy(item, &["text", "words_text", "line"])
            .map(vstr)
            .unwrap_or_default();
        let start = first_present(item, &["startTime", "time", "begin", "start"])
            .map(parse_time_ms)
            .unwrap_or(0);
        let dur = if let Some(d) = first_present(item, &["duration", "dur"]) {
            parse_time_ms(d)
        } else if let Some(e) = first_present(item, &["endTime", "end"]) {
            (parse_time_ms(e) - start).max(500)
        } else {
            3000
        };

        let mut syl_list = ["syllabus", "syllables", "words", "tokens", "spans"]
            .iter()
            .find_map(|k| {
                item.get(*k)
                    .and_then(|v| v.as_array())
                    .filter(|a| !a.is_empty())
            });
        if syl_list.is_none()
            && let Some(lead) = item.get("lead").and_then(|v| v.as_object())
        {
            syl_list = ["syllabus", "syllables", "words"].iter().find_map(|k| {
                lead.get(*k)
                    .and_then(|v| v.as_array())
                    .filter(|a| !a.is_empty())
            });
        }

        let mut syls = Vec::new();
        for s in syl_list.into_iter().flatten() {
            let Some(s) = s.as_object() else { continue };
            let text = first_truthy(s, &["text", "word", "token", "val"])
                .map(vstr)
                .unwrap_or_default();
            if text.is_empty() {
                continue;
            }
            let st = first_present(s, &["time", "startTime", "begin", "start"])
                .map(parse_time_ms)
                .unwrap_or(0);
            let sd = if let Some(d) = first_present(s, &["duration", "dur"]) {
                parse_time_ms(d)
            } else if let Some(e) = first_present(s, &["endTime", "end"]) {
                (parse_time_ms(e) - st).max(50)
            } else {
                200
            };
            let s_singer_raw = s.get("singer").map(vstr).unwrap_or_default();
            let bg = s.get("isBackground").is_some_and(truthy)
                || s.get("background").is_some_and(truthy)
                || is_bg_singer(&s_singer_raw)
                || line_bg;
            syls.push(Syllable {
                text,
                start: st,
                dur: sd,
                end: st + sd,
                bg,
                clean: String::new(),
            });
        }

        if line_text.is_empty() && !syls.is_empty() {
            line_text = syls.iter().map(|s| s.text.as_str()).collect();
        }
        let line_text = RE_WS.replace_all(&line_text, " ").trim().to_string();
        if line_text.is_empty() && syls.is_empty() {
            continue;
        }
        let has_syls = !syls.is_empty();
        if !has_syls {
            syls.push(Syllable {
                text: line_text.clone(),
                start,
                dur,
                end: start + dur,
                bg: line_bg,
                clean: String::new(),
            });
        }
        let mut l = blank_line(line_text, start, dur, syls, has_syls);
        l.bg = line_bg;
        l.part = part;
        l.singer = singer;
        lines.push(l);
    }
    Lyrics::new(lines, "Radiant Lyrics")
}

static RE_LRC_TS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\[(\d+):(\d+(?:\.\d+)?)\]").unwrap());
static RE_LRC_WORD: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"<(\d+):(\d+(?:\.\d+)?)>([^<]*)").unwrap());

fn lrc_ms(m: &str, s: &str) -> i64 {
    let m: f64 = m.parse().unwrap_or(0.0);
    let s: f64 = s.parse().unwrap_or(0.0);
    ((m * 60.0 + s) * 1000.0) as i64
}

pub fn parse_lrc(text: &str) -> Option<Arc<Lyrics>> {
    // (timestamp, text). Empty texts are kept as boundaries so instrumental gaps are detected.
    let mut entries: Vec<(i64, String)> = Vec::new();
    for raw in text.lines() {
        let mut rest = raw.trim();
        let mut stamps = Vec::new();
        while let Some(c) = RE_LRC_TS.captures(rest) {
            stamps.push(lrc_ms(&c[1], &c[2]));
            rest = &rest[c.get(0).unwrap().end()..];
        }
        let txt = rest.trim().to_string();
        for ts in stamps {
            entries.push((ts, txt.clone()));
        }
    }
    entries.sort_by_key(|e| e.0);

    let mut lines = Vec::new();
    for (i, (ts, txt)) in entries.iter().enumerate() {
        if txt.is_empty() {
            continue;
        }
        let next = entries.get(i + 1).map(|e| e.0).unwrap_or(ts + 4000);
        let dur = (next - ts).max(500);
        let words: Vec<_> = RE_LRC_WORD.captures_iter(txt).collect();
        if !words.is_empty() {
            let mut syls = Vec::new();
            // Text before the first word tag belongs to the line start.
            let lead = &txt[..words[0].get(0).unwrap().start()];
            if !lead.trim().is_empty() {
                let first = lrc_ms(&words[0][1], &words[0][2]);
                let d = (first - ts).max(50);
                syls.push(Syllable {
                    text: lead.to_string(),
                    start: *ts,
                    dur: d,
                    end: ts + d,
                    bg: false,
                    clean: String::new(),
                });
            }
            for (wi, w) in words.iter().enumerate() {
                let wts = lrc_ms(&w[1], &w[2]);
                let wd = match words.get(wi + 1) {
                    Some(n) => (lrc_ms(&n[1], &n[2]) - wts).max(50),
                    None => (ts + dur - wts).max(50),
                };
                syls.push(Syllable {
                    text: w[3].to_string(),
                    start: wts,
                    dur: wd,
                    end: wts + wd,
                    bg: false,
                    clean: String::new(),
                });
            }
            let clean: String = syls
                .iter()
                .map(|s| s.text.as_str())
                .collect::<String>()
                .trim()
                .to_string();
            let t = if clean.is_empty() { txt.clone() } else { clean };
            lines.push(blank_line(t, *ts, dur, syls, true));
        } else {
            let syl = Syllable {
                text: txt.clone(),
                start: *ts,
                dur,
                end: ts + dur,
                bg: false,
                clean: String::new(),
            };
            lines.push(blank_line(txt.clone(), *ts, dur, vec![syl], false));
        }
    }
    Lyrics::new(lines, "LRCLIB")
}

// Credentials

pub const TOKEN_ID_KEY: &str = "radiant_lyrics_token_id";
pub const TOKEN_KEY: &str = "radiant_lyrics_token";

/// Radiant Lyrics credentials (each user brings their own): the `RADIANT_LYRICS_TOKEN_ID` /
/// `RADIANT_LYRICS_TOKEN` environment variables, otherwise the ones saved in settings.
/// Empty when none are configured; only LRCLIB is used then.
pub fn resolve_tokens(extra: &Map<String, Value>) -> (String, String) {
    let env = |k: &str| {
        std::env::var(k)
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    };
    if let (Some(i), Some(s)) = (env("RADIANT_LYRICS_TOKEN_ID"), env("RADIANT_LYRICS_TOKEN")) {
        return (i, s);
    }
    let get = |k: &str| {
        extra
            .get(k)
            .map(vstr)
            .map(|s| s.trim().to_string())
            .unwrap_or_default()
    };
    let (i, s) = (get(TOKEN_ID_KEY), get(TOKEN_KEY));
    if i.is_empty() || s.is_empty() {
        (String::new(), String::new())
    } else {
        (i, s)
    }
}

// Fetching + cache

#[derive(Clone)]
pub enum LyricsState {
    Loading,
    Missing,
    Ready(Arc<Lyrics>),
}

struct Entry {
    lyrics: Option<Arc<Lyrics>>,
    at: Instant,
}

pub struct LyricsStore {
    cache: Mutex<HashMap<String, Entry>>,
    fetching: Mutex<HashSet<String>>,
    tokens: Mutex<(String, String)>,
}

const CACHE_TTL: Duration = Duration::from_secs(3600);
const NEGATIVE_TTL: Duration = Duration::from_secs(300);

impl LyricsStore {
    pub fn new(tokens: (String, String)) -> Arc<Self> {
        Arc::new(Self {
            cache: Mutex::new(HashMap::new()),
            fetching: Mutex::new(HashSet::new()),
            tokens: Mutex::new(tokens),
        })
    }

    pub fn has_tokens(&self) -> bool {
        !self.tokens.lock().0.is_empty()
    }

    /// Swaps the credentials; cached results are dropped so the new source is used right away.
    pub fn set_tokens(&self, tokens: (String, String)) {
        let mut t = self.tokens.lock();
        if *t != tokens {
            *t = tokens;
            drop(t);
            self.cache.lock().clear();
        }
    }

    /// Non-blocking: returns the cached state and kicks off a background fetch if needed.
    pub fn get(self: &Arc<Self>, title: &str, artist: &str, duration_ms: f64) -> LyricsState {
        if title.is_empty() {
            return LyricsState::Missing;
        }
        let (t, a) = clean_metadata(title, artist);
        let key = format!("{}|{}", t.to_lowercase(), a.to_lowercase());
        if let Some(e) = self.cache.lock().get(&key) {
            let ttl = if e.lyrics.is_some() {
                CACHE_TTL
            } else {
                NEGATIVE_TTL
            };
            if e.at.elapsed() < ttl {
                return match &e.lyrics {
                    Some(l) => LyricsState::Ready(l.clone()),
                    None => LyricsState::Missing,
                };
            }
        }
        if !self.fetching.lock().insert(key.clone()) {
            return LyricsState::Loading;
        }
        let me = self.clone();
        std::thread::Builder::new()
            .name("lyrics-fetch".into())
            .spawn(move || {
                let res = me.fetch(&t, &a, duration_ms);
                let mut c = me.cache.lock();
                if c.len() > 150 {
                    let mut keys: Vec<_> = c.iter().map(|(k, e)| (e.at, k.clone())).collect();
                    keys.sort();
                    for (_, k) in keys.into_iter().take(35) {
                        c.remove(&k);
                    }
                }
                c.insert(
                    key.clone(),
                    Entry {
                        lyrics: res,
                        at: Instant::now(),
                    },
                );
                drop(c);
                me.fetching.lock().remove(&key);
            })
            .ok();
        LyricsState::Loading
    }

    /// Seeds the cache directly (render test and demo mode).
    pub fn seed(&self, title: &str, artist: &str, l: Arc<Lyrics>) {
        let (t, a) = clean_metadata(title, artist);
        let key = format!("{}|{}", t.to_lowercase(), a.to_lowercase());
        self.cache.lock().insert(
            key,
            Entry {
                lyrics: Some(l),
                at: Instant::now(),
            },
        );
    }

    fn fetch(&self, title: &str, artist: &str, duration_ms: f64) -> Option<Arc<Lyrics>> {
        // LRCLIB runs in parallel so a Radiant miss doesn't cost a second round-trip.
        let (t2, a2) = (title.to_string(), artist.to_string());
        let lrclib = std::thread::spawn(move || fetch_lrclib(&t2, &a2, duration_ms));
        let (tid, tok) = self.tokens.lock().clone();
        if tid.is_empty() || tok.is_empty() {
            return lrclib.join().ok().flatten();
        }

        let isrc = resolve_isrc(title, artist);
        let mut url = format!(
            "https://api.radiant-lyrics.org/rl-api?title={}&artist={}",
            net::enc(title),
            net::enc(artist)
        );
        if let Some(i) = &isrc {
            url.push_str(&format!("&isrc={}", net::enc(i)));
        }
        url.push_str("&romanize=false&platform=rl");
        let headers = [
            ("P-Access-Token-Id", tid.as_str()),
            ("P-Access-Token", tok.as_str()),
            ("User-Agent", net::APP_UA),
        ];
        if let Some(v) = net::get_json(&url, &headers)
            && let Some(l) = parse_ttml_json(&v)
        {
            return Some(l);
        }
        lrclib.join().ok().flatten()
    }
}

fn resolve_isrc(title: &str, artist: &str) -> Option<String> {
    let q = if artist.is_empty() {
        title.to_string()
    } else {
        format!("artist:\"{artist}\" track:\"{title}\"")
    };
    let v = net::get_json(
        &format!("https://api.deezer.com/search?q={}&limit=1", net::enc(&q)),
        &[],
    )?;
    let id = v.get("data")?.get(0)?.get("id")?.as_i64()?;
    let t = net::get_json(&format!("https://api.deezer.com/track/{id}"), &[])?;
    let isrc = t.get("isrc")?.as_str()?.trim().to_uppercase();
    (isrc.len() >= 10).then_some(isrc)
}

fn fetch_lrclib(title: &str, artist: &str, duration_ms: f64) -> Option<Arc<Lyrics>> {
    let ua = [("User-Agent", net::APP_UA)];
    let mut url = format!(
        "https://lrclib.net/api/get?track_name={}&artist_name={}",
        net::enc(title),
        net::enc(artist)
    );
    if duration_ms > 0.0 {
        url.push_str(&format!("&duration={}", (duration_ms / 1000.0) as i64));
    }
    if let Some(v) = net::get_json(&url, &ua)
        && let Some(l) = v
            .get("syncedLyrics")
            .and_then(|s| s.as_str())
            .and_then(parse_lrc)
    {
        return Some(l);
    }
    // Exact match failed (often a duration mismatch): search and pick the closest synced result.
    let url = format!(
        "https://lrclib.net/api/search?track_name={}&artist_name={}",
        net::enc(title),
        net::enc(artist)
    );
    let v = net::get_json(&url, &ua)?;
    let target = duration_ms / 1000.0;
    v.as_array()?
        .iter()
        .filter(|r| {
            r.get("syncedLyrics")
                .and_then(|s| s.as_str())
                .is_some_and(|s| !s.is_empty())
        })
        .filter(|r| {
            let d = r.get("duration").and_then(|d| d.as_f64()).unwrap_or(0.0);
            target <= 0.0 || (d - target).abs() <= 6.0
        })
        .min_by(|a, b| {
            let da = (a.get("duration").and_then(|d| d.as_f64()).unwrap_or(0.0) - target).abs();
            let db = (b.get("duration").and_then(|d| d.as_f64()).unwrap_or(0.0) - target).abs();
            da.total_cmp(&db)
        })
        .and_then(|r| parse_lrc(r.get("syncedLyrics")?.as_str()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times() {
        assert_eq!(parse_time_ms(&serde_json::json!(5.699)), 5699);
        assert_eq!(parse_time_ms(&serde_json::json!(5699)), 5699);
        assert_eq!(parse_time_ms(&serde_json::json!("1:02.5")), 62500);
        assert_eq!(parse_time_ms(&serde_json::json!("250ms")), 250);
        assert_eq!(parse_time_ms(&serde_json::json!("2.5s")), 2500);
    }

    #[test]
    fn clean() {
        assert_eq!(
            clean_metadata(
                "Kanye West - Stronger (Official Video)",
                "Kanye West - Topic"
            ),
            ("Stronger".into(), "Kanye West".into())
        );
        assert_eq!(
            clean_metadata("Song feat. X", "A"),
            ("Song".into(), "A".into())
        );
    }

    #[test]
    fn lrc() {
        let l = parse_lrc("[ar:x]\n[00:01.00]Hello world\n[00:03.00]\n[00:10.00][00:20.00]Again")
            .unwrap();
        assert_eq!(l.lines.len(), 3);
        assert_eq!(l.lines[0].end, 3000);
        assert_eq!(l.lines[2].start, 20000);
    }

    #[test]
    fn ttml() {
        let v = serde_json::json!({"data":[{"startTime":1.0,"endTime":3.0,"syllabus":[{"text":"Hi ","time":1000,"duration":500},{"text":"there","time":1500,"duration":500}]}]});
        let l = parse_ttml_json(&v).unwrap();
        assert_eq!(l.lines[0].text, "Hi there");
        assert!(l.lines[0].has_syls);
        assert_eq!(l.lines[0].start, 1000);
    }

    #[test]
    fn adlib_split() {
        let l = parse_lrc("[00:01.00]Every <00:02.00>into blue (oh-oh)\n[00:05.00]Next").unwrap();
        let texts: Vec<_> = l.lines[0].syls.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(texts, ["Every ", "into blue ", "(oh-oh)"]);
        let s = &l.lines[0].syls;
        assert_eq!(s[1].end, s[2].start);
        assert_eq!(s[2].end, l.lines[0].syls.last().unwrap().end);
    }

    /// Network test: `cargo test -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn live_fetch() {
        let store = LyricsStore::new(resolve_tokens(&Map::new()));
        for (t, a) in [("Blinding Lights", "The Weeknd"), ("Midnight City", "M83")] {
            let start = Instant::now();
            loop {
                match store.get(t, a, 0.0) {
                    LyricsState::Ready(l) => {
                        println!(
                            "{t}: {} lines from {} (word sync: {}) in {:?}",
                            l.lines.len(),
                            l.source,
                            l.word_synced,
                            start.elapsed()
                        );
                        break;
                    }
                    LyricsState::Missing => panic!("{t}: no lyrics"),
                    LyricsState::Loading => std::thread::sleep(Duration::from_millis(100)),
                }
                assert!(start.elapsed() < Duration::from_secs(20));
            }
        }
    }
}
