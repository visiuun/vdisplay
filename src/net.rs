//! Tiny blocking HTTP helpers shared by the cover-art and lyrics fetchers.

use serde_json::Value;
use std::sync::LazyLock;
use std::time::Duration;

const BROWSER_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64)";
pub const APP_UA: &str = "Vdisplay/2.0";

static AGENT: LazyLock<ureq::Agent> = LazyLock::new(|| {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(4)))
        .user_agent(BROWSER_UA)
        .build()
        .into()
});

/// Percent-encodes a query component, always encoding spaces as `%20`.
pub fn enc(s: &str) -> String {
    urlencoding::encode(s).into_owned()
}

pub fn get_text(url: &str, headers: &[(&str, &str)]) -> Option<String> {
    let mut req = AGENT.get(url);
    for (k, v) in headers {
        req = req.header(*k, *v);
    }
    let mut resp = req.call().ok()?;
    resp.body_mut()
        .with_config()
        .limit(8 * 1024 * 1024)
        .read_to_string()
        .ok()
}

pub fn get_json(url: &str, headers: &[(&str, &str)]) -> Option<Value> {
    serde_json::from_str(&get_text(url, headers)?).ok()
}

pub fn get_bytes(url: &str) -> Option<Vec<u8>> {
    let mut resp = AGENT.get(url).call().ok()?;
    resp.body_mut()
        .with_config()
        .limit(16 * 1024 * 1024)
        .read_to_vec()
        .ok()
}
