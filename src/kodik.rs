use crate::util::{encode_form, header_map, http, is_playlist_url, norm_cmp, normalize_host, plugin_url, CHROME_UA};
use crate::{BrowseItem, BrowseResult, OnlineError, OnlineSource, ProviderHit, RawQuality, RawStream, TitleQuery, VoiceOption};
use base64::Engine;
use regex::Regex;
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::time::Duration;
use url::Url;

const DEFAULT_TOKEN: &str = "41dd95f84c21719b09d6c71182237a25";

fn base_of(server: &OnlineSource) -> String {
    if server.kind == "kodik" && !server.url.trim().is_empty() {
        normalize_host(&server.url, "https://kodik-api.com")
    } else {
        "https://kodik-api.com".to_string()
    }
}

fn token_of(server: &OnlineSource) -> &str {
    let t = server.token.trim();
    if t.is_empty() { DEFAULT_TOKEN } else { t }
}

async fn api_search(base: &str, token: &str, extra: &[(&str, &str)]) -> Result<Vec<Value>, String> {
    let mut u = Url::parse(&format!("{base}/search")).map_err(|e| e.to_string())?;
    {
        let mut q = u.query_pairs_mut();
        q.append_pair("token", token);
        q.append_pair("limit", "100");
        for (k, v) in extra {
            if !v.is_empty() {
                q.append_pair(k, v);
            }
        }
    }
    let resp = http().get(u).header("User-Agent", CHROME_UA).timeout(Duration::from_secs(12)).send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status()));
    }
    let body: Value = resp.json().await.map_err(|e| e.to_string())?;
    Ok(body.get("results").and_then(Value::as_array).cloned().unwrap_or_default())
}

pub async fn check(server: &OnlineSource, q: &TitleQuery) -> Result<Option<ProviderHit>, String> {
    let base = base_of(server);
    let token = token_of(server);
    let mut results = Vec::new();
    let mut id_param: Option<(&str, String)> = None;

    if let Some(kp) = q.kinopoisk_id.filter(|&k| k > 0) {
        let kp_s = kp.to_string();
        let r = api_search(&base, token, &[("kinopoisk_id", &kp_s)]).await?;
        if !r.is_empty() {
            results = r;
            id_param = Some(("kinopoisk_id", kp_s));
        }
    }
    if results.is_empty() {
        if let Some(im) = q.imdb_id.as_deref().filter(|s| !s.is_empty()) {
            let r = api_search(&base, token, &[("imdb_id", im)]).await?;
            if !r.is_empty() {
                results = r;
                id_param = Some(("imdb_id", im.to_string()));
            }
        }
    }
    if results.is_empty() {
        let r = api_search(&base, token, &[("title", &q.title)]).await?;
        let want_ru = norm_cmp(&q.title);
        let want_orig = q.original_title.as_deref().map(norm_cmp).unwrap_or_default();
        if let Some(hit) = r.into_iter().find(|it| {
            let t = norm_cmp(it.get("title").and_then(Value::as_str).unwrap_or(""));
            let o = norm_cmp(it.get("title_orig").and_then(Value::as_str).unwrap_or(""));
            let y_ok = match (q.year, it.get("year").and_then(Value::as_u64)) {
                (Some(a), Some(b)) => a.abs_diff(b as u32) <= 1,
                _ => true,
            };
            y_ok && (t == want_ru || (!want_orig.is_empty() && o == want_orig))
        }) {
            if let Some(kid) = hit.get("id").and_then(Value::as_str) {
                id_param = Some(("id", kid.to_string()));
                results = vec![hit];
            }
        }
    }

    if results.is_empty() {
        return Ok(None);
    }

    let (k, v) = id_param.unwrap_or(("title", q.title.clone()));
    let title = results[0].get("title").and_then(Value::as_str).unwrap_or(&q.title);
    Ok(Some(ProviderHit {
        server: server.name.clone(),
        name: "Kodik".into(),
        balancer: "kodik".into(),
        quality: Some("до 720p".into()),
        url: plugin_url(&server.id, "kodik", "browse", &[(k, &v), ("title", title)]),
    }))
}

pub async fn browse(server: &OnlineSource, params: &HashMap<String, String>) -> Result<BrowseResult, OnlineError> {
    let base = base_of(server);
    let token = token_of(server);

    let mut q_args: Vec<(&str, &str)> = vec![("with_episodes", "true")];
    for key in ["kinopoisk_id", "imdb_id", "id", "title"] {
        if let Some(v) = params.get(key).filter(|s| !s.is_empty()) {
            q_args.push((key, v));
            break;
        }
    }

    let results = api_search(&base, token, &q_args)
        .await
        .map_err(|e| OnlineError::upstream("kodik_error", format!("Kodik: {e}")))?;
    if results.is_empty() {
        return Ok(BrowseResult::message("Kodik: ничего не найдено"));
    }

    let mut voice_map: Vec<(String, String)> = Vec::new();
    let mut season_ids: BTreeMap<u32, ()> = BTreeMap::new();
    for item in &results {
        if let Some(tr) = item.get("translation") {
            let vid = tr.get("id").and_then(|v| v.as_u64().map(|n| n.to_string()).or_else(|| v.as_str().map(str::to_string))).unwrap_or_default();
            let vname = tr.get("title").and_then(Value::as_str).unwrap_or("Озвучка").to_string();
            if !vid.is_empty() && !voice_map.iter().any(|(id, _)| id == &vid) {
                voice_map.push((vid, vname));
            }
        }
        if let Some(seasons) = item.get("seasons").and_then(Value::as_object) {
            for sk in seasons.keys() {
                if let Ok(n) = sk.parse::<u32>() {
                    season_ids.insert(n, ());
                }
            }
        }
    }

    let key_pair = ["kinopoisk_id", "imdb_id", "id", "title"]
        .iter()
        .find_map(|&k| params.get(k).filter(|s| !s.is_empty()).map(|v| (k, v.as_str())))
        .unwrap_or(("title", ""));
    let title = params.get("title").map(String::as_str).unwrap_or("Видео");

    if season_ids.is_empty() {
        let items = results
            .iter()
            .filter_map(|item| {
                let link = item.get("link").and_then(Value::as_str)?;
                let vname = item.pointer("/translation/title").and_then(Value::as_str).unwrap_or(title);
                let qual = item.get("quality").and_then(Value::as_str).map(str::to_string);
                Some(BrowseItem {
                    method: "call".into(),
                    title: vname.to_string(),
                    season: None,
                    episode: None,
                    voice: None,
                    details: None,
                    quality: qual,
                    similar: false,
                    url: Some(plugin_url(&server.id, "kodik", "resolve", &[("link", link)])),
                    stream: None,
                })
            })
            .collect();
        return Ok(BrowseResult { voices: Vec::new(), items, message: None });
    }

    let active_voice = params
        .get("voice")
        .filter(|v| voice_map.iter().any(|(id, _)| id == *v))
        .cloned()
        .or_else(|| voice_map.first().map(|(id, _)| id.clone()))
        .unwrap_or_default();

    let voices = voice_map
        .iter()
        .map(|(vid, vname)| VoiceOption {
            name: vname.clone(),
            url: Some(plugin_url(&server.id, "kodik", "browse", &[key_pair, ("title", title), ("voice", vid)])),
            active: vid == &active_voice,
        })
        .collect();

    let chosen = results
        .iter()
        .find(|it| {
            it.pointer("/translation/id")
                .and_then(|v| v.as_u64().map(|n| n.to_string()).or_else(|| v.as_str().map(str::to_string)))
                .as_deref()
                == Some(active_voice.as_str())
        })
        .unwrap_or(&results[0]);

    let seasons_obj = chosen.get("seasons").and_then(Value::as_object);
    let mut seasons_num: Vec<u32> = seasons_obj.map(|m| m.keys().filter_map(|k| k.parse::<u32>().ok()).collect()).unwrap_or_default();
    seasons_num.sort();

    if seasons_num.len() > 1 && !params.contains_key("season") {
        let items = seasons_num
            .iter()
            .map(|sn| {
                let ss = sn.to_string();
                BrowseItem {
                    method: "link".into(),
                    title: format!("Сезон {sn}"),
                    season: Some(*sn),
                    episode: None,
                    voice: None,
                    details: None,
                    quality: None,
                    similar: false,
                    url: Some(plugin_url(&server.id, "kodik", "browse", &[key_pair, ("title", title), ("voice", &active_voice), ("season", &ss)])),
                    stream: None,
                }
            })
            .collect();
        return Ok(BrowseResult { voices, items, message: None });
    }

    let want_season = params.get("season").and_then(|s| s.parse::<u32>().ok()).or_else(|| seasons_num.first().copied()).unwrap_or(1);

    let mut ep_list: Vec<(u32, String)> = seasons_obj
        .and_then(|m| m.get(&want_season.to_string()))
        .and_then(|s| s.get("episodes"))
        .and_then(Value::as_object)
        .map(|eps| eps.iter().filter_map(|(k, v)| Some((k.parse::<u32>().ok()?, v.as_str()?.to_string()))).collect())
        .unwrap_or_default();
    ep_list.sort_by_key(|(n, _)| *n);

    let items = ep_list
        .into_iter()
        .map(|(ep_num, link)| BrowseItem {
            method: "call".into(),
            title: format!("Серия {ep_num}"),
            season: Some(want_season),
            episode: Some(ep_num),
            voice: None,
            details: None,
            quality: Some("360p – 720p".into()),
            similar: false,
            url: Some(plugin_url(&server.id, "kodik", "resolve", &[("link", &link)])),
            stream: None,
        })
        .collect();

    Ok(BrowseResult { voices, items, message: None })
}

pub async fn resolve(params: &HashMap<String, String>) -> Result<RawStream, OnlineError> {
    let raw_link = params
        .get("link")
        .filter(|s| !s.is_empty())
        .ok_or_else(|| OnlineError::bad_request("Kodik: отсутствует ссылка"))?;
    let iframe_url = if raw_link.starts_with("//") { format!("https:{raw_link}") } else { raw_link.clone() };
    let origin = Url::parse(&iframe_url)
        .ok()
        .and_then(|u| Some(format!("{}://{}", u.scheme(), u.host_str()?)))
        .unwrap_or_else(|| "https://kodikplayer.com".to_string());

    let html = http()
        .get(&iframe_url)
        .header("User-Agent", CHROME_UA)
        .header("Referer", format!("{origin}/"))
        .timeout(Duration::from_secs(12))
        .send()
        .await
        .map_err(|e| OnlineError::upstream("kodik_error", format!("Kodik: {e}")))?
        .text()
        .await
        .map_err(|e| OnlineError::upstream("kodik_error", e.to_string()))?;

    if html.contains("запрещено к просмотру в данной стране") {
        return Err(OnlineError::upstream("kodik_geo", "Kodik: видео недоступно в вашем регионе"));
    }

    let flat = html.replace('\n', " ");
    let url_params: Value = Regex::new(r"urlParams\s*=\s*'([^']+)'")
        .ok()
        .and_then(|r| r.captures(&flat))
        .and_then(|c| serde_json::from_str(&c[1]).ok())
        .ok_or_else(|| OnlineError::upstream("kodik_parse", "Kodik: не найдены параметры плеера"))?;

    let v_type = Regex::new(r"(?:videoInfo|vInfo)\.type\s*=\s*'([^']+)'").unwrap().captures(&flat).map(|c| c[1].to_string()).unwrap_or_default();
    let v_hash = Regex::new(r"(?:videoInfo|vInfo)\.hash\s*=\s*'([^']+)'").unwrap().captures(&flat).map(|c| c[1].to_string()).unwrap_or_default();
    let v_id = Regex::new(r"(?:videoInfo|vInfo)\.id\s*=\s*'([^']+)'").unwrap().captures(&flat).map(|c| c[1].to_string()).unwrap_or_default();
    let player_src = Regex::new(r#"<script[^>]*src="(/assets/js/app\.player_single[^"]+)""#)
        .unwrap()
        .captures(&flat)
        .map(|c| c[1].to_string())
        .ok_or_else(|| OnlineError::upstream("kodik_parse", "Kodik: не найден скрипт плеера"))?;

    let js = http()
        .get(format!("{origin}{player_src}"))
        .header("User-Agent", CHROME_UA)
        .timeout(Duration::from_secs(12))
        .send()
        .await
        .map_err(|e| OnlineError::upstream("kodik_error", e.to_string()))?
        .text()
        .await
        .map_err(|e| OnlineError::upstream("kodik_error", e.to_string()))?;

    let api_path = Regex::new(r#"\$\.ajax\(\{type:\s*"POST",\s*url:\s*atob\("([^"]+)"\)"#)
        .unwrap()
        .captures(&js.replace('\n', " "))
        .and_then(|c| base64::engine::general_purpose::STANDARD.decode(c[1].as_bytes()).ok())
        .and_then(|b| String::from_utf8(b).ok())
        .filter(|s| s.starts_with('/'))
        .ok_or_else(|| OnlineError::upstream("kodik_parse", "Kodik: не найден адрес потока"))?;

    let form = [
        ("d", url_params.get("d").and_then(Value::as_str).unwrap_or("")),
        ("d_sign", url_params.get("d_sign").and_then(Value::as_str).unwrap_or("")),
        ("pd", url_params.get("pd").and_then(Value::as_str).unwrap_or("")),
        ("pd_sign", url_params.get("pd_sign").and_then(Value::as_str).unwrap_or("")),
        ("ref", url_params.get("ref").and_then(Value::as_str).unwrap_or("")),
        ("ref_sign", url_params.get("ref_sign").and_then(Value::as_str).unwrap_or("")),
        ("bad_user", "true"),
        ("cdn_is_working", "true"),
        ("type", &v_type),
        ("hash", &v_hash),
        ("id", &v_id),
        ("info", "{}"),
    ];

    let data: Value = http()
        .post(format!("{origin}{api_path}"))
        .header("User-Agent", CHROME_UA)
        .header("Referer", &iframe_url)
        .header("Origin", &origin)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(encode_form(&form))
        .timeout(Duration::from_secs(12))
        .send()
        .await
        .map_err(|e| OnlineError::upstream("kodik_error", e.to_string()))?
        .json()
        .await
        .map_err(|e| OnlineError::upstream("kodik_json", e.to_string()))?;

    let links_obj = data
        .get("links")
        .and_then(Value::as_object)
        .ok_or_else(|| OnlineError::upstream("kodik_nostream", "Kodik: поток не получен"))?;

    let headers = header_map(&[("Referer", &format!("{origin}/")), ("User-Agent", CHROME_UA)]);
    let mut items: Vec<(u32, RawQuality)> = Vec::new();
    for (k, v) in links_obj {
        let Some(enc) = v.pointer("/0/src").and_then(Value::as_str) else { continue };
        let decoded = decode_kodik(enc);
        if decoded.is_empty() {
            continue;
        }
        let url = if decoded.starts_with("//") { format!("https:{decoded}") } else { decoded };
        let q_num = k.parse::<u32>().unwrap_or(0);
        let label = if q_num > 0 { format!("{q_num}p") } else { k.clone() };
        items.push((q_num, RawQuality { label, url }));
    }
    items.sort_by_key(|(q, _)| std::cmp::Reverse(*q));

    let main = items
        .first()
        .map(|(_, q)| q.url.clone())
        .ok_or_else(|| OnlineError::upstream("kodik_nostream", "Kodik: ссылки не расшифрованы"))?;
    let qualities: Vec<RawQuality> = items.into_iter().map(|(_, q)| q).collect();

    Ok(RawStream {
        hls: is_playlist_url(&main),
        url: main,
        headers,
        qualities,
        subtitles: Vec::new(),
    })
}

pub fn decode_kodik(src: &str) -> String {
    if src.starts_with("http") || src.starts_with("//") {
        return src.to_string();
    }
    let rotated: String = src
        .chars()
        .map(|c| {
            if c.is_ascii_uppercase() {
                ((((c as u8 - b'A') + 18) % 26) + b'A') as char
            } else if c.is_ascii_lowercase() {
                ((((c as u8 - b'a') + 18) % 26) + b'a') as char
            } else {
                c
            }
        })
        .collect();
    base64::engine::general_purpose::STANDARD
        .decode(rotated.as_bytes())
        .ok()
        .and_then(|b| String::from_utf8(b).ok())
        .unwrap_or_default()
}
