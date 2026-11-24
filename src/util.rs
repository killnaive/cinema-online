use crate::{OnlineError, OnlineSource};
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::OnceLock;
use std::time::{Duration, Instant};
use url::Url;

pub const CHROME_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0 Safari/537.36";
const CACHE_TTL: Duration = Duration::from_secs(300);

pub fn http() -> reqwest::Client {
    static C: OnceLock<reqwest::Client> = OnceLock::new();
    C.get_or_init(|| {
        reqwest::Client::builder()
            .user_agent(CHROME_UA)
            .connect_timeout(Duration::from_secs(10))
            .build()
            .expect("http client")
    })
    .clone()
}

fn html_cache() -> &'static Mutex<HashMap<String, (Instant, (String, String))>> {
    static C: OnceLock<Mutex<HashMap<String, (Instant, (String, String))>>> = OnceLock::new();
    C.get_or_init(Default::default)
}

pub fn cache_get_html(key: &str) -> Option<(String, String)> {
    html_cache().lock().get(key).and_then(|(at, val)| {
        if at.elapsed() < CACHE_TTL {
            Some(val.clone())
        } else {
            None
        }
    })
}

pub fn cache_put_html(key: String, host: String, html: String) {
    let mut guard = html_cache().lock();
    if guard.len() > 64 {
        guard.retain(|_, (at, _)| at.elapsed() < CACHE_TTL);
    }
    guard.insert(key, (Instant::now(), (host, html)));
}

pub fn plugin_url(server_id: &str, kind: &str, action: &str, params: &[(&str, &str)]) -> String {
    let mut u = Url::parse("cinema://plugin").expect("valid base url");
    if let Ok(mut segs) = u.path_segments_mut() {
        segs.extend([server_id, kind, action]);
    }
    {
        let mut qp = u.query_pairs_mut();
        for (k, v) in params {
            if !v.is_empty() {
                qp.append_pair(k, v);
            }
        }
    }
    u.to_string()
}

pub fn parse_plugin_url(link: &str) -> Result<(String, String, String, HashMap<String, String>), OnlineError> {
    let u = Url::parse(link).map_err(|_| OnlineError::bad_request("неверная ссылка плагина"))?;
    if u.scheme() != "cinema" || u.host_str() != Some("plugin") {
        return Err(OnlineError::bad_request("неверная схема ссылки плагина"));
    }
    let segs: Vec<String> = u.path_segments().map(|s| s.map(str::to_string).collect()).unwrap_or_default();
    if segs.len() < 3 {
        return Err(OnlineError::bad_request("неполная ссылка плагина"));
    }
    let params = u.query_pairs().map(|(k, v)| (k.into_owned(), v.into_owned())).collect();
    Ok((segs[0].clone(), segs[1].clone(), segs[2].clone(), params))
}

pub fn plugin_server_id(link: &str) -> Option<String> {
    let u = Url::parse(link).ok()?;
    if u.scheme() != "cinema" || u.host_str() != Some("plugin") {
        return None;
    }
    u.path_segments()?.next().filter(|s| !s.is_empty()).map(str::to_string)
}

pub fn normalize_host(raw: &str, fallback: &str) -> String {
    let s = raw.trim();
    let s = if s.is_empty() { fallback } else { s };
    let with_proto = if s.starts_with("http://") || s.starts_with("https://") {
        s.to_string()
    } else {
        format!("https://{s}")
    };
    with_proto.trim_end_matches('/').to_string()
}

pub fn encode_form(pairs: &[(&str, &str)]) -> String {
    let mut s = url::form_urlencoded::Serializer::new(String::new());
    for (k, v) in pairs {
        s.append_pair(k, v);
    }
    s.finish()
}

pub fn header_map(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect()
}

pub fn is_playlist_url(u: &str) -> bool {
    let path = u.split('?').next().unwrap_or(u).to_lowercase();
    path.ends_with(".m3u8") || path.ends_with(".m3u") || (path.contains("/hls/") && !path.ends_with(".ts"))
}

pub fn unescape(s: &str) -> String {
    s.replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&#x27;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

pub fn strip_tags(s: &str) -> String {
    let mut out = String::new();
    let mut inside = false;
    for c in s.chars() {
        match c {
            '<' => inside = true,
            '>' => inside = false,
            _ if !inside => out.push(c),
            _ => {}
        }
    }
    unescape(out.split_whitespace().collect::<Vec<_>>().join(" ").trim())
}

pub fn norm_cmp(s: &str) -> String {
    s.to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric() || c.is_whitespace())
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn find_source<'a>(sources: &'a [OnlineSource], id: &str) -> Option<&'a OnlineSource> {
    sources.iter().find(|s| s.enabled && s.id == id)
}
