use crate::util::{header_map, http, norm_cmp, normalize_host, plugin_url, CHROME_UA};
use crate::{BrowseItem, BrowseResult, OnlineError, OnlineSource, ProviderHit, RawQuality, RawStream, TitleQuery};
use serde_json::Value;
use std::collections::HashMap;
use std::time::Duration;
use url::Url;

const DEFAULT_BASE: &str = "https://api.anilibria.app/api/v1";

fn base_of(server: &OnlineSource) -> String {
    if server.kind == "anilibria" && !server.url.trim().is_empty() {
        normalize_host(&server.url, DEFAULT_BASE)
    } else {
        DEFAULT_BASE.to_string()
    }
}

pub async fn check(server: &OnlineSource, q: &TitleQuery) -> Result<Option<ProviderHit>, String> {
    // Для голливудских фильмов без японской/азиатской озвучки не делаем лишних ложных срабатываний,
    // но проверяем точное совпадение названия + года для всех запросов.
    if let Some(lang) = q.original_language.as_deref() {
        if !lang.is_empty() && !matches!(lang, "ja" | "zh" | "ko" | "en") {
            return Ok(None);
        }
    }

    let base = base_of(server);
    let mut terms = vec![q.title.clone()];
    if let Some(orig) = q.original_title.as_deref().filter(|s| !s.is_empty() && *s != q.title) {
        terms.push(orig.to_string());
    }

    let want_ru = norm_cmp(&q.title);
    let want_orig = q.original_title.as_deref().map(norm_cmp).unwrap_or_default();

    for term in terms {
        let Ok(mut u) = Url::parse(&format!("{base}/app/search/releases")) else { continue };
        u.query_pairs_mut().append_pair("query", &term);
        let resp = http().get(u).header("User-Agent", CHROME_UA).timeout(Duration::from_secs(10)).send().await.map_err(|e| e.to_string())?;
        if !resp.status().is_success() {
            continue;
        }
        let Ok(Value::Array(list)) = resp.json::<Value>().await else { continue };
        if let Some(hit) = list.into_iter().find(|it| {
            let main = norm_cmp(it.pointer("/name/main").and_then(Value::as_str).unwrap_or(""));
            let eng = norm_cmp(it.pointer("/name/english").and_then(Value::as_str).unwrap_or(""));
            let alt = norm_cmp(it.pointer("/name/alternative").and_then(Value::as_str).unwrap_or(""));
            let title_match = main == want_ru
                || eng == want_ru
                || (!want_orig.is_empty() && (eng == want_orig || main == want_orig || alt == want_orig));
            let year_match = match (q.year, it.get("year").and_then(Value::as_u64)) {
                (Some(a), Some(b)) => a.abs_diff(b as u32) <= 1,
                _ => true,
            };
            title_match && year_match
        }) {
            if let Some(id) = hit.get("id").and_then(Value::as_u64) {
                let title = hit.pointer("/name/main").and_then(Value::as_str).unwrap_or(&q.title);
                return Ok(Some(ProviderHit {
                    server: server.name.clone(),
                    name: "AniLibria".into(),
                    balancer: "anilibria".into(),
                    quality: Some("до 1080p".into()),
                    url: plugin_url(&server.id, "anilibria", "browse", &[("id", &id.to_string()), ("title", title)]),
                }));
            }
        }
    }

    Ok(None)
}

pub async fn browse(server: &OnlineSource, params: &HashMap<String, String>) -> Result<BrowseResult, OnlineError> {
    let id = params
        .get("id")
        .filter(|s| !s.is_empty())
        .ok_or_else(|| OnlineError::bad_request("AniLibria: отсутствует id релиза"))?;
    let base = base_of(server);
    let url = format!("{base}/anime/releases/{id}");

    let rel: Value = http()
        .get(&url)
        .header("User-Agent", CHROME_UA)
        .timeout(Duration::from_secs(12))
        .send()
        .await
        .map_err(|e| OnlineError::upstream("anilibria_error", format!("AniLibria: {e}")))?
        .json()
        .await
        .map_err(|e| OnlineError::upstream("anilibria_json", format!("AniLibria: {e}")))?;

    let episodes = rel.get("episodes").and_then(Value::as_array).cloned().unwrap_or_default();
    if episodes.is_empty() {
        return Ok(BrowseResult::message("AniLibria: серии не найдены"));
    }

    let is_movie = rel.pointer("/type/value").and_then(Value::as_str) == Some("MOVIE") && episodes.len() == 1;
    let rel_title = rel
        .pointer("/name/main")
        .and_then(Value::as_str)
        .or_else(|| params.get("title").map(String::as_str))
        .unwrap_or("Аниме");
    let headers = header_map(&[("User-Agent", CHROME_UA)]);

    let items: Vec<BrowseItem> = episodes
        .into_iter()
        .filter_map(|ep| {
            let mut qualities = Vec::new();
            for (field, label) in [("hls_1080", "1080p"), ("hls_720", "720p"), ("hls_480", "480p")] {
                if let Some(u) = ep.get(field).and_then(Value::as_str).filter(|s| !s.is_empty()) {
                    qualities.push(RawQuality { label: label.to_string(), url: u.to_string() });
                }
            }
            let main = qualities.first()?.url.clone();
            let ord = ep.get("ordinal").and_then(|v| v.as_u64().or_else(|| v.as_f64().map(|f| f as u64))).map(|n| n as u32);
            let ep_name = ep.get("name").and_then(Value::as_str).filter(|s| !s.is_empty());
            let title = if is_movie {
                rel_title.to_string()
            } else {
                match (ord, ep_name) {
                    (Some(n), Some(nm)) => format!("{n} серия · {nm}"),
                    (Some(n), None) => format!("{n} серия"),
                    (None, Some(nm)) => nm.to_string(),
                    _ => "Серия".to_string(),
                }
            };
            Some(BrowseItem {
                method: "play".into(),
                title,
                season: if is_movie { None } else { Some(1) },
                episode: if is_movie { None } else { ord },
                voice: Some("AniLibria".into()),
                details: None,
                quality: qualities.first().map(|q| q.label.clone()),
                similar: false,
                url: None,
                stream: Some(RawStream {
                    url: main,
                    hls: true,
                    headers: headers.clone(),
                    qualities,
                    subtitles: Vec::new(),
                }),
            })
        })
        .collect();

    Ok(BrowseResult { voices: Vec::new(), items, message: None })
}
