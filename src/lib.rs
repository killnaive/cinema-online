//! Самостоятельный плагин-модуль онлайн-источников для Cinema.
//!
//! Не зависит от ядра приложения и других проектов: содержит встроенные балансеры
//! (Collaps, HDrezka, Kodik), поддержку подключаемых JSON-манифестов (например, с GitHub)
//! и собственного удалённого протокола Cinema Online API.

pub mod collaps;
pub mod kodik;
pub mod manifest;
pub mod rezka;
pub mod util;

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct OnlineSource {
    pub id: String,
    pub name: String,
    /// Тип источника: "collaps", "rezka", "kodik", "remote"
    pub kind: String,
    pub url: String,
    pub token: String,
    pub mirror: String,
    pub login: String,
    pub password: String,
    pub cookie: String,
    pub enabled: bool,
}

impl Default for OnlineSource {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            kind: "collaps".into(),
            url: String::new(),
            token: String::new(),
            mirror: String::new(),
            login: String::new(),
            password: String::new(),
            cookie: String::new(),
            enabled: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TitleQuery {
    pub tmdb_id: Option<u64>,
    pub imdb_id: Option<String>,
    pub kinopoisk_id: Option<u64>,
    pub title: String,
    pub original_title: Option<String>,
    pub original_language: Option<String>,
    pub year: Option<u32>,
    #[serde(default)]
    pub serial: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderHit {
    pub server: String,
    pub name: String,
    pub balancer: String,
    pub url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VoiceOption {
    pub name: String,
    pub url: Option<String>,
    pub active: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawQuality {
    pub label: String,
    pub url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawSubtitle {
    pub label: String,
    pub url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawStream {
    pub url: String,
    pub hls: bool,
    #[serde(default)]
    pub headers: HashMap<String, String>,
    #[serde(default)]
    pub qualities: Vec<RawQuality>,
    #[serde(default)]
    pub subtitles: Vec<RawSubtitle>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrowseItem {
    pub method: String,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub season: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub episode: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub voice: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<String>,
    #[serde(default)]
    pub similar: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream: Option<RawStream>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrowseResult {
    #[serde(default)]
    pub voices: Vec<VoiceOption>,
    #[serde(default)]
    pub items: Vec<BrowseItem>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

impl BrowseResult {
    pub fn message(msg: &str) -> Self {
        Self { voices: Vec::new(), items: Vec::new(), message: Some(msg.to_string()) }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginManifest {
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub sources: Vec<OnlineSource>,
}

#[derive(Debug, Clone)]
pub struct OnlineError {
    pub status: u16,
    pub code: &'static str,
    pub message: String,
}

impl OnlineError {
    pub fn bad_request(msg: impl Into<String>) -> Self {
        Self { status: 400, code: "bad_request", message: msg.into() }
    }
    pub fn not_found(msg: impl Into<String>) -> Self {
        Self { status: 404, code: "not_found", message: msg.into() }
    }
    pub fn upstream(code: &'static str, msg: impl Into<String>) -> Self {
        Self { status: 502, code, message: msg.into() }
    }
}

/// Источники по умолчанию, встроенные в модуль Cinema Online.
pub fn default_sources() -> Vec<OnlineSource> {
    serde_json::from_str::<PluginManifest>(include_str!("../manifest.json"))
        .map(|m| m.sources)
        .unwrap_or_default()
}

pub fn is_playlist_url(u: &str) -> bool {
    util::is_playlist_url(u)
}

pub fn owner_source<'a>(sources: &'a [OnlineSource], link: &str) -> Option<&'a OnlineSource> {
    if let Some(sid) = util::plugin_server_id(link) {
        return util::find_source(sources, &sid);
    }
    sources.iter().find(|s| {
        if !s.enabled {
            return false;
        }
        let b = util::normalize_host(&s.url, "");
        !b.is_empty() && link.starts_with(&b)
    })
}

pub async fn fetch_manifest(url: &str) -> Result<PluginManifest, OnlineError> {
    manifest::fetch_manifest(url).await
}

pub async fn check_sources(sources: &[OnlineSource], q: &TitleQuery, uid: &str) -> (Vec<ProviderHit>, Vec<String>) {
    let tasks = sources.iter().filter(|s| s.enabled).map(|s| async move {
        match s.kind.as_str() {
            "collaps" => match collaps::check(s, q).await {
                Ok(Some(hit)) => (vec![hit], None),
                Ok(None) => (Vec::new(), None),
                Err(e) => (Vec::new(), Some(format!("{}: {e}", s.name))),
            },
            "rezka" => match rezka::check(s, q).await {
                Ok(Some(hit)) => (vec![hit], None),
                Ok(None) => (Vec::new(), None),
                Err(e) => (Vec::new(), Some(format!("{}: {e}", s.name))),
            },
            "kodik" => match kodik::check(s, q).await {
                Ok(Some(hit)) => (vec![hit], None),
                Ok(None) => (Vec::new(), None),
                Err(e) => (Vec::new(), Some(format!("{}: {e}", s.name))),
            },
            "remote" => match manifest::remote_check(s, q, uid).await {
                Ok(hits) => (hits, None),
                Err(e) => (Vec::new(), Some(format!("{}: {e}", s.name))),
            },
            _ => (Vec::new(), None),
        }
    });

    let mut providers = Vec::new();
    let mut errors = Vec::new();
    for (hits, err) in futures::future::join_all(tasks).await {
        providers.extend(hits);
        if let Some(e) = err {
            errors.push(e);
        }
    }
    (providers, errors)
}

pub async fn browse(sources: &[OnlineSource], link: &str, uid: &str) -> Result<BrowseResult, OnlineError> {
    let source = owner_source(sources, link).ok_or_else(|| OnlineError::bad_request("источник не найден среди включённых в настройках"))?;
    if util::plugin_server_id(link).is_some() {
        let (_sid, kind, _action, params) = util::parse_plugin_url(link)?;
        return match kind.as_str() {
            "collaps" => collaps::browse(source, &params).await,
            "rezka" => rezka::browse(source, &params).await,
            "kodik" => kodik::browse(source, &params).await,
            _ => Err(OnlineError::bad_request("неизвестный балансер")),
        };
    }
    manifest::remote_browse(source, link, uid).await
}

pub async fn resolve(sources: &[OnlineSource], link: &str, uid: &str) -> Result<RawStream, OnlineError> {
    let source = owner_source(sources, link).ok_or_else(|| OnlineError::bad_request("источник не найден среди включённых в настройках"))?;
    if util::plugin_server_id(link).is_some() {
        let (_sid, kind, _action, params) = util::parse_plugin_url(link)?;
        return match kind.as_str() {
            "rezka" => rezka::resolve(source, &params).await,
            "kodik" => kodik::resolve(&params).await,
            _ => Err(OnlineError::bad_request("разрешение потока для этого балансера не поддерживается")),
        };
    }
    manifest::remote_resolve(source, link, uid).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;

    #[test]
    fn loads_embedded_manifest_defaults() {
        let srcs = default_sources();
        assert_eq!(srcs.len(), 3);
        assert!(srcs.iter().any(|s| s.kind == "collaps"));
        assert!(srcs.iter().any(|s| s.kind == "rezka"));
        assert!(srcs.iter().any(|s| s.kind == "kodik"));
    }

    #[test]
    fn decodes_rezka_obfuscated_string() {
        let payload = "[720p]https://cdn.example/720.m3u8,[1080p]https://cdn.example/1080.mp4";
        let b64 = base64::engine::general_purpose::STANDARD.encode(payload.as_bytes());
        let trash = format!("//_//{}", base64::engine::general_purpose::STANDARD.encode("$$!!@$$@^!@#$$@"));
        let mid = b64.len() / 2;
        let obf = format!("#h{}{}{}", &b64[..mid], trash, &b64[mid..]);
        assert_eq!(rezka::decode_rezka(&obf), payload);
    }

    #[test]
    fn parses_rezka_qualities_sorted_desc() {
        let s = "[360p]https://cdn.example/360.mp4 or https://cdn.example/360.m3u8,[1080p]https://cdn.example/1080.mp4 or https://cdn.example/1080.m3u8";
        let qs = rezka::parse_rezka_streams(s);
        assert_eq!(qs.len(), 2);
        assert_eq!(qs[0].label, "1080p");
        assert!(qs[0].url.ends_with("1080.m3u8"));
        assert_eq!(qs[1].label, "360p");
    }

    #[test]
    fn extracts_collaps_js_object() {
        let html = r#"<script>makePlayer({ blocked: false, source: { hls: 'https://cdn.example/master.m3u8?a=1', audio: { names: ['Русский', 'Eng'], }, cc: [] }, p2p: { tracker: function() {} } });</script>"#;
        let src = collaps::extract_js_object(html, "source").unwrap();
        assert_eq!(src["hls"], "https://cdn.example/master.m3u8?a=1");
        assert_eq!(src["audio"]["names"][0], "Русский");
    }

    #[test]
    fn decodes_kodik_rot18() {
        let orig = "https://cdn.kodik.example/master.m3u8";
        let b64 = base64::engine::general_purpose::STANDARD.encode(orig.as_bytes());
        let rot_minus18: String = b64
            .chars()
            .map(|c| {
                if c.is_ascii_uppercase() {
                    ((((c as u8 - b'A') + 8) % 26) + b'A') as char
                } else if c.is_ascii_lowercase() {
                    ((((c as u8 - b'a') + 8) % 26) + b'a') as char
                } else {
                    c
                }
            })
            .collect();
        assert_eq!(kodik::decode_kodik(&rot_minus18), orig);
    }
}
