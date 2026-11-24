use crate::util::{cache_get_html, cache_put_html, header_map, http, normalize_host, plugin_url, CHROME_UA};
use crate::{BrowseItem, BrowseResult, OnlineError, OnlineSource, ProviderHit, RawStream, RawSubtitle, TitleQuery, VoiceOption};
use regex::Regex;
use serde_json::Value;
use std::collections::HashMap;
use std::time::Duration;

fn collaps_hosts(server: &OnlineSource) -> Vec<String> {
    let mut hosts = Vec::new();
    if !server.mirror.trim().is_empty() {
        hosts.push(normalize_host(&server.mirror, "https://api.nextembed.ws"));
    }
    let u = server.url.trim();
    if !u.is_empty() && server.kind == "collaps" {
        hosts.push(normalize_host(u, "https://api.nextembed.ws"));
    }
    for def in ["https://api.nextembed.ws", "https://api.kinogram.best"] {
        if !hosts.iter().any(|h| h == def) {
            hosts.push(def.to_string());
        }
    }
    hosts
}

async fn fetch_html(server: &OnlineSource, imdb: Option<&str>, kp: Option<u64>) -> Result<Option<(String, String)>, String> {
    let mut paths = Vec::new();
    if let Some(k) = kp.filter(|&k| k > 0) {
        paths.push(format!("kp/{k}"));
    }
    if let Some(im) = imdb.filter(|s| !s.trim().is_empty()) {
        paths.push(format!("imdb/{}", im.trim()));
    }
    if paths.is_empty() {
        return Ok(None);
    }

    let hosts = collaps_hosts(server);
    let cache_key = format!("collaps:{}:{}", hosts.join(","), paths.join(","));
    if let Some(hit) = cache_get_html(&cache_key) {
        return Ok(Some(hit));
    }

    let mut last_err = None;
    for path in &paths {
        for host in &hosts {
            let embed_url = format!("{host}/embed/{path}");
            let res = http()
                .get(&embed_url)
                .header("User-Agent", CHROME_UA)
                .header("Referer", format!("{host}/"))
                .header("Origin", host.as_str())
                .timeout(Duration::from_secs(12))
                .send()
                .await;
            match res {
                Ok(r) if r.status().is_success() => {
                    let text = r.text().await.map_err(|e| e.to_string())?;
                    if text.contains("makePlayer(") {
                        cache_put_html(cache_key, host.clone(), text.clone());
                        return Ok(Some((host.clone(), text)));
                    }
                }
                Ok(r) if r.status() == reqwest::StatusCode::NOT_FOUND => continue,
                Ok(r) => last_err = Some(format!("HTTP {}", r.status())),
                Err(e) => last_err = Some(e.to_string()),
            }
        }
    }

    if let Some(e) = last_err { Err(e) } else { Ok(None) }
}

pub async fn check(server: &OnlineSource, q: &TitleQuery) -> Result<Option<ProviderHit>, String> {
    let imdb = q.imdb_id.as_deref();
    let kp = q.kinopoisk_id;
    let Some((_, html)) = fetch_html(server, imdb, kp).await? else {
        return Ok(None);
    };
    if extract_js_object(&html, "source").is_none() && extract_js_object(&html, "playlist").is_none() {
        return Ok(None);
    }
    let kp_str = kp.map(|k| k.to_string()).unwrap_or_default();
    Ok(Some(ProviderHit {
        server: server.name.clone(),
        name: "Collaps".into(),
        balancer: "collaps".into(),
        url: plugin_url(&server.id, "collaps", "browse", &[("imdb", imdb.unwrap_or("")), ("kp", &kp_str), ("title", &q.title)]),
    }))
}

pub async fn browse(server: &OnlineSource, params: &HashMap<String, String>) -> Result<BrowseResult, OnlineError> {
    let imdb = params.get("imdb").map(String::as_str);
    let kp = params.get("kp").and_then(|v| v.parse::<u64>().ok());
    let fallback_title = params.get("title").map(String::as_str).unwrap_or("Видео");

    let (host, html) = fetch_html(server, imdb, kp)
        .await
        .map_err(|e| OnlineError::upstream("collaps_error", format!("Collaps: {e}")))?
        .ok_or_else(|| OnlineError::not_found("Collaps: видео не найдено"))?;

    let referer = format!("{host}/");
    let headers = header_map(&[("Referer", &referer), ("Origin", &host), ("User-Agent", CHROME_UA)]);

    if let Some(playlist) = extract_js_object(&html, "playlist") {
        let mut seasons: Vec<Value> = playlist.get("seasons").and_then(Value::as_array).cloned().unwrap_or_default();
        seasons.sort_by_key(|s| s.get("season").and_then(Value::as_u64).unwrap_or(0));
        if seasons.is_empty() {
            return Ok(BrowseResult::message("Collaps: сезоны не найдены"));
        }

        let want_season = params
            .get("season")
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or_else(|| seasons[0].get("season").and_then(Value::as_u64).unwrap_or(1));

        let kp_str = kp.map(|k| k.to_string()).unwrap_or_default();
        let voices: Vec<VoiceOption> = seasons
            .iter()
            .filter_map(|s| {
                let n = s.get("season").and_then(Value::as_u64)?;
                let ns = n.to_string();
                Some(VoiceOption {
                    name: format!("Сезон {n}"),
                    url: Some(plugin_url(&server.id, "collaps", "browse", &[("imdb", imdb.unwrap_or("")), ("kp", &kp_str), ("title", fallback_title), ("season", &ns)])),
                    active: n == want_season,
                })
            })
            .collect();

        let current_season = seasons.iter().find(|s| s.get("season").and_then(Value::as_u64) == Some(want_season)).unwrap_or(&seasons[0]);
        let s_num = current_season.get("season").and_then(Value::as_u64).unwrap_or(1) as u32;

        let mut items = Vec::new();
        if let Some(eps) = current_season.get("episodes").and_then(Value::as_array) {
            for ep in eps {
                let ep_num = ep
                    .get("episode")
                    .and_then(|v| v.as_u64().or_else(|| v.as_str().and_then(|s| s.parse().ok())))
                    .unwrap_or(0) as u32;
                let title = ep
                    .get("title")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("Серия {ep_num}"));
                let Some(stream) = stream_of(ep, &headers) else { continue };
                items.push(BrowseItem {
                    method: "play".into(),
                    title,
                    season: Some(s_num),
                    episode: Some(ep_num),
                    voice: None,
                    details: audio_summary(ep),
                    similar: false,
                    url: None,
                    stream: Some(stream),
                });
            }
        }
        return Ok(BrowseResult { voices, items, message: None });
    }

    if let Some(source) = extract_js_object(&html, "source") {
        let Some(stream) = stream_of(&source, &headers) else {
            return Ok(BrowseResult::message("Collaps: поток недоступен"));
        };
        return Ok(BrowseResult {
            voices: Vec::new(),
            items: vec![BrowseItem {
                method: "play".into(),
                title: fallback_title.to_string(),
                season: None,
                episode: None,
                voice: None,
                details: audio_summary(&source),
                similar: false,
                url: None,
                stream: Some(stream),
            }],
            message: None,
        });
    }

    Ok(BrowseResult::message("Collaps: видео заблокировано или недоступно"))
}

fn audio_summary(node: &Value) -> Option<String> {
    let names = node.pointer("/audio/names").and_then(Value::as_array)?;
    let list: Vec<&str> = names.iter().filter_map(Value::as_str).filter(|s| !s.is_empty() && *s != "delete").take(4).collect();
    if list.is_empty() { None } else { Some(list.join(", ")) }
}

fn stream_of(node: &Value, headers: &HashMap<String, String>) -> Option<RawStream> {
    let hls = node.get("hls").and_then(Value::as_str).filter(|s| !s.is_empty())?;
    let mut subtitles = Vec::new();
    if let Some(cc) = node.get("cc").and_then(Value::as_array) {
        for c in cc {
            if let Some(u) = c.get("url").and_then(Value::as_str).filter(|s| !s.is_empty()) {
                subtitles.push(RawSubtitle {
                    label: c.get("name").and_then(Value::as_str).unwrap_or("Субтитры").to_string(),
                    url: u.to_string(),
                });
            }
        }
    }
    Some(RawStream {
        url: hls.to_string(),
        hls: true,
        headers: headers.clone(),
        qualities: Vec::new(),
        subtitles,
    })
}

pub fn extract_js_object(src: &str, key: &str) -> Option<Value> {
    let re = Regex::new(&format!(r"\b{}\s*:\s*\{{", regex::escape(key))).ok()?;
    let m = re.find(src)?;
    let start = m.end() - 1;
    let bytes = src.as_bytes();
    let mut i = start;
    let mut depth = 0i32;
    let mut in_str = false;
    let mut quote = b'"';
    let mut end = None;

    while i < bytes.len() {
        let b = bytes[i];
        if in_str {
            if b == b'\\' {
                i += 2;
                continue;
            } else if b == quote {
                in_str = false;
            }
        } else {
            match b {
                b'"' | b'\'' => {
                    in_str = true;
                    quote = b;
                }
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        end = Some(i + 1);
                        break;
                    }
                }
                _ => {}
            }
        }
        i += 1;
    }

    let raw = &src[start..end?];
    serde_json::from_str(&js_literal_to_json(raw)).ok()
}

fn js_literal_to_json(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len() + 64);
    let chars: Vec<char> = raw.chars().collect();
    let mut i = 0;

    while i < chars.len() {
        let ch = chars[i];
        if ch == '"' || ch == '\'' {
            let q = ch;
            i += 1;
            out.push('"');
            while i < chars.len() {
                let c = chars[i];
                if c == '\\' && i + 1 < chars.len() {
                    let next = chars[i + 1];
                    if q == '\'' && next == '\'' {
                        out.push('\'');
                    } else if q == '\'' && next == '"' {
                        out.push_str("\\\"");
                    } else {
                        out.push('\\');
                        out.push(next);
                    }
                    i += 2;
                    continue;
                }
                if c == q {
                    i += 1;
                    break;
                }
                if c == '"' {
                    out.push_str("\\\"");
                } else {
                    out.push(c);
                }
                i += 1;
            }
            out.push('"');
        } else if ch == '{' || ch == ',' {
            out.push(ch);
            i += 1;
            while i < chars.len() && chars[i].is_whitespace() {
                out.push(chars[i]);
                i += 1;
            }
            if i < chars.len() && (chars[i].is_ascii_alphabetic() || chars[i] == '_') {
                let id_start = i;
                while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '_') {
                    i += 1;
                }
                let mut k = i;
                while k < chars.len() && chars[k].is_whitespace() {
                    k += 1;
                }
                let ident: String = chars[id_start..i].iter().collect();
                if k < chars.len() && chars[k] == ':' {
                    out.push('"');
                    out.push_str(&ident);
                    out.push('"');
                } else {
                    out.push_str(&ident);
                }
            }
        } else {
            out.push(ch);
            i += 1;
        }
    }

    Regex::new(r",\s*([\}\]])")
        .map(|re| re.replace_all(&out, "$1").into_owned())
        .unwrap_or(out)
}
