use crate::util::{cache_get_html, cache_put_html, header_map, http, is_playlist_url, normalize_host, plugin_url, CHROME_UA};
use crate::{BrowseItem, BrowseResult, OnlineError, OnlineSource, ProviderHit, RawQuality, RawStream, TitleQuery, VoiceOption};
use serde_json::Value;
use std::collections::{BTreeSet, HashMap};
use std::time::Duration;

const DEFAULT_BASE: &str = "https://plapi.cdnvideohub.com/api/v1/player/sv";

fn base_of(server: &OnlineSource) -> String {
    if server.kind == "cdnvideohub" && !server.url.trim().is_empty() {
        normalize_host(&server.url, DEFAULT_BASE)
    } else {
        DEFAULT_BASE.to_string()
    }
}

async fn fetch_playlist(base: &str, kp: Option<u64>, imdb: Option<&str>) -> Result<Option<Value>, String> {
    let mut urls = Vec::new();
    if let Some(k) = kp.filter(|&k| k > 0) {
        urls.push(format!("{base}/playlist?pub=12&aggr=kp&id={k}"));
    }
    if let Some(im) = imdb.filter(|s| !s.trim().is_empty()) {
        urls.push(format!("{base}/playlist?pub=12&aggr=imdb&id={}", im.trim()));
    }
    if urls.is_empty() {
        return Ok(None);
    }

    let cache_key = format!("cvh:{}", urls.join("|"));
    if let Some((_, raw)) = cache_get_html(&cache_key) {
        if let Ok(v) = serde_json::from_str::<Value>(&raw) {
            return Ok(Some(v));
        }
    }

    let mut last_err = None;
    for url in urls {
        let resp = http().get(&url).header("User-Agent", CHROME_UA).timeout(Duration::from_secs(10)).send().await;
        match resp {
            Ok(r) if r.status().is_success() => {
                let text = r.text().await.map_err(|e| e.to_string())?;
                if let Ok(v) = serde_json::from_str::<Value>(&text) {
                    if v.get("items").and_then(Value::as_array).is_some_and(|a| !a.is_empty()) {
                        cache_put_html(cache_key, base.to_string(), text);
                        return Ok(Some(v));
                    }
                }
            }
            Ok(r) if r.status() == reqwest::StatusCode::NOT_FOUND || r.status() == reqwest::StatusCode::INTERNAL_SERVER_ERROR => continue,
            Ok(r) => last_err = Some(format!("HTTP {}", r.status())),
            Err(e) => last_err = Some(e.to_string()),
        }
    }

    if let Some(e) = last_err { Err(e) } else { Ok(None) }
}

pub async fn check(server: &OnlineSource, q: &TitleQuery) -> Result<Option<ProviderHit>, String> {
    let base = base_of(server);
    let kp = q.kinopoisk_id;
    let imdb = q.imdb_id.as_deref();
    let Some(_) = fetch_playlist(&base, kp, imdb).await? else {
        return Ok(None);
    };
    let kp_str = kp.map(|k| k.to_string()).unwrap_or_default();
    Ok(Some(ProviderHit {
        server: server.name.clone(),
        name: "CDNVideoHub".into(),
        balancer: "cdnvideohub".into(),
        url: plugin_url(
            &server.id,
            "cdnvideohub",
            "browse",
            &[("kp", &kp_str), ("imdb", imdb.unwrap_or("")), ("title", &q.title)],
        ),
    }))
}

fn voice_of(item: &Value) -> String {
    let studio = item.get("voiceStudio").and_then(Value::as_str).unwrap_or("").trim();
    let vtype = item.get("voiceType").and_then(Value::as_str).unwrap_or("").trim();
    if !studio.is_empty() {
        studio.to_string()
    } else if !vtype.is_empty() {
        vtype.to_string()
    } else {
        "Оригинал".to_string()
    }
}

pub async fn browse(server: &OnlineSource, params: &HashMap<String, String>) -> Result<BrowseResult, OnlineError> {
    let base = base_of(server);
    let kp = params.get("kp").and_then(|v| v.parse::<u64>().ok());
    let imdb = params.get("imdb").map(String::as_str);
    let fallback_title = params.get("title").map(String::as_str).unwrap_or("Видео");

    let json = fetch_playlist(&base, kp, imdb)
        .await
        .map_err(|e| OnlineError::upstream("cvh_error", format!("CDNVideoHub: {e}")))?
        .ok_or_else(|| OnlineError::not_found("CDNVideoHub: видео не найдено"))?;

    let items = json.get("items").and_then(Value::as_array).cloned().unwrap_or_default();
    if items.is_empty() {
        return Ok(BrowseResult::message("CDNVideoHub: список пуст"));
    }

    let is_serial = json.get("isSerial").and_then(Value::as_bool).unwrap_or(false)
        || items.iter().any(|x| x.get("season").and_then(Value::as_u64).is_some());

    if !is_serial {
        let out: Vec<BrowseItem> = items
            .iter()
            .filter_map(|it| {
                let vk_id = it.get("vkId").and_then(|v| v.as_str().map(str::to_string).or_else(|| v.as_u64().map(|n| n.to_string())))?;
                let studio = it.get("voiceStudio").and_then(Value::as_str).unwrap_or("").trim();
                let vtype = it.get("voiceType").and_then(Value::as_str).unwrap_or("").trim();
                let label = match (studio.is_empty(), vtype.is_empty()) {
                    (false, false) if studio != vtype => format!("{studio} ({vtype})"),
                    (false, _) => studio.to_string(),
                    (_, false) => vtype.to_string(),
                    _ => fallback_title.to_string(),
                };
                Some(BrowseItem {
                    method: "call".into(),
                    title: label,
                    season: None,
                    episode: None,
                    voice: None,
                    details: Some("до 4K / 1080p".into()),
                    similar: false,
                    url: Some(plugin_url(&server.id, "cdnvideohub", "resolve", &[("vk_id", &vk_id)])),
                    stream: None,
                })
            })
            .collect();
        return Ok(BrowseResult { voices: Vec::new(), items: out, message: None });
    }

    let mut voice_names: Vec<String> = Vec::new();
    for it in &items {
        let v = voice_of(it);
        if !voice_names.contains(&v) {
            voice_names.push(v);
        }
    }
    let active_voice = params
        .get("voice")
        .filter(|v| voice_names.contains(*v))
        .cloned()
        .or_else(|| voice_names.first().cloned())
        .unwrap_or_default();

    let kp_str = kp.map(|k| k.to_string()).unwrap_or_default();
    let voices: Vec<VoiceOption> = voice_names
        .iter()
        .map(|v| VoiceOption {
            name: v.clone(),
            url: Some(plugin_url(
                &server.id,
                "cdnvideohub",
                "browse",
                &[("kp", &kp_str), ("imdb", imdb.unwrap_or("")), ("title", fallback_title), ("voice", v)],
            )),
            active: v == &active_voice,
        })
        .collect();

    let voice_items: Vec<&Value> = items.iter().filter(|it| voice_of(it) == active_voice).collect();
    let seasons: BTreeSet<u32> = voice_items
        .iter()
        .filter_map(|it| it.get("season").and_then(Value::as_u64).map(|n| n as u32))
        .collect();

    if seasons.len() > 1 && !params.contains_key("season") {
        let out = seasons
            .into_iter()
            .map(|sn| {
                let ss = sn.to_string();
                BrowseItem {
                    method: "link".into(),
                    title: format!("Сезон {sn}"),
                    season: Some(sn),
                    episode: None,
                    voice: Some(active_voice.clone()),
                    details: None,
                    similar: false,
                    url: Some(plugin_url(
                        &server.id,
                        "cdnvideohub",
                        "browse",
                        &[("kp", &kp_str), ("imdb", imdb.unwrap_or("")), ("title", fallback_title), ("voice", &active_voice), ("season", &ss)],
                    )),
                    stream: None,
                }
            })
            .collect();
        return Ok(BrowseResult { voices, items: out, message: None });
    }

    let want_season = params
        .get("season")
        .and_then(|s| s.parse::<u32>().ok())
        .or_else(|| seasons.iter().next().copied())
        .unwrap_or(1);

    let mut eps: Vec<(u32, String)> = voice_items
        .into_iter()
        .filter(|it| it.get("season").and_then(Value::as_u64).unwrap_or(1) as u32 == want_season)
        .filter_map(|it| {
            let ep = it.get("episode").and_then(Value::as_u64).unwrap_or(0) as u32;
            let vk_id = it.get("vkId").and_then(|v| v.as_str().map(str::to_string).or_else(|| v.as_u64().map(|n| n.to_string())))?;
            Some((ep, vk_id))
        })
        .collect();
    eps.sort_by_key(|(ep, _)| *ep);

    let out = eps
        .into_iter()
        .map(|(ep, vk_id)| BrowseItem {
            method: "call".into(),
            title: format!("Серия {ep}"),
            season: Some(want_season),
            episode: Some(ep),
            voice: Some(active_voice.clone()),
            details: None,
            similar: false,
            url: Some(plugin_url(&server.id, "cdnvideohub", "resolve", &[("vk_id", &vk_id)])),
            stream: None,
        })
        .collect();

    Ok(BrowseResult { voices, items: out, message: None })
}

pub async fn resolve(server: &OnlineSource, params: &HashMap<String, String>) -> Result<RawStream, OnlineError> {
    let vk_id = params
        .get("vk_id")
        .filter(|s| !s.is_empty())
        .ok_or_else(|| OnlineError::bad_request("CDNVideoHub: отсутствует идентификатор видео"))?;
    let base = base_of(server);
    let url = format!("{base}/video/{vk_id}");

    let data: Value = http()
        .get(&url)
        .header("User-Agent", CHROME_UA)
        .timeout(Duration::from_secs(12))
        .send()
        .await
        .map_err(|e| OnlineError::upstream("cvh_error", format!("CDNVideoHub: {e}")))?
        .json()
        .await
        .map_err(|e| OnlineError::upstream("cvh_json", format!("CDNVideoHub: {e}")))?;

    let sources = data
        .get("sources")
        .ok_or_else(|| OnlineError::upstream("cvh_nostream", "CDNVideoHub: поток не найден"))?;

    let map = [
        ("mpeg2kUrl", "4K"),
        ("mpeg4kUrl", "2K"),
        ("mpegQhdUrl", "1440p"),
        ("mpegFullHdUrl", "1080p"),
        ("mpegHighUrl", "720p"),
        ("mpegMediumUrl", "480p"),
        ("mpegLowUrl", "360p"),
        ("hlsUrl", "HLS"),
    ];

    let mut qualities = Vec::new();
    for (field, label) in map {
        if let Some(u) = sources.get(field).and_then(Value::as_str).filter(|s| !s.is_empty()) {
            let abs = if u.starts_with("//") { format!("https:{u}") } else { u.to_string() };
            if !qualities.iter().any(|q: &RawQuality| q.url == abs) {
                qualities.push(RawQuality { label: label.to_string(), url: abs });
            }
        }
    }

    let main = qualities
        .first()
        .map(|q| q.url.clone())
        .ok_or_else(|| OnlineError::upstream("cvh_nostream", "CDNVideoHub: ссылки качества отсутствуют"))?;

    Ok(RawStream {
        hls: is_playlist_url(&main),
        url: main,
        headers: header_map(&[("User-Agent", CHROME_UA)]),
        qualities,
        subtitles: Vec::new(),
    })
}
