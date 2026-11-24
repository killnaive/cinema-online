use crate::util::{encode_form, header_map, http, is_playlist_url, norm_cmp, normalize_host, plugin_url, strip_tags, CHROME_UA};
use crate::{BrowseItem, BrowseResult, OnlineError, OnlineSource, ProviderHit, RawQuality, RawStream, RawSubtitle, TitleQuery, VoiceOption};
use base64::Engine;
use parking_lot::Mutex;
use regex::Regex;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::OnceLock;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use url::Url;

fn session_cache() -> &'static Mutex<HashMap<String, (Instant, String)>> {
    static C: OnceLock<Mutex<HashMap<String, (Instant, String)>>> = OnceLock::new();
    C.get_or_init(Default::default)
}

fn candidate_hosts(server: &OnlineSource) -> Vec<String> {
    let mut hosts = Vec::new();
    if !server.mirror.trim().is_empty() {
        hosts.push(normalize_host(&server.mirror, "https://rezka.ag"));
    }
    if server.kind == "rezka" && !server.url.trim().is_empty() {
        let u = normalize_host(&server.url, "https://rezka.ag");
        if !hosts.contains(&u) {
            hosts.push(u);
        }
    }
    for fallback in ["https://rezka.ag", "https://kvk.zone"] {
        if !hosts.iter().any(|h| h == fallback) {
            hosts.push(fallback.to_string());
        }
    }
    hosts
}

fn primary_host(server: &OnlineSource) -> String {
    candidate_hosts(server).into_iter().next().unwrap_or_else(|| "https://rezka.ag".to_string())
}

fn host_from_url(u: &str, fallback: &str) -> String {
    Url::parse(u)
        .ok()
        .and_then(|p| Some(format!("{}://{}", p.scheme(), p.host_str()?)))
        .unwrap_or_else(|| fallback.to_string())
}

fn base_cookie(server: &OnlineSource) -> String {
    let c = server.cookie.trim();
    if c.is_empty() {
        "PHPSESSID=cinema_session_01".to_string()
    } else if c.contains("PHPSESSID=") {
        c.to_string()
    } else {
        format!("PHPSESSID=cinema_session_01; {c}")
    }
}

async fn ensure_cookie(server: &OnlineSource, host: &str) -> String {
    let base = base_cookie(server);
    if base.contains("dle_user_id=") || server.login.trim().is_empty() || server.password.trim().is_empty() {
        return base;
    }
    let key = format!("{host}:{}", server.login.trim());
    if let Some(hit) = session_cache().lock().get(&key).and_then(|(at, c)| if at.elapsed() < Duration::from_secs(1800) { Some(c.clone()) } else { None }) {
        return hit;
    }

    let url = format!("{host}/ajax/login/");
    let form = [
        ("login_name", server.login.trim()),
        ("login_password", server.password.trim()),
        ("login_not_save", "0"),
    ];
    let Ok(resp) = http()
        .post(&url)
        .header("User-Agent", CHROME_UA)
        .header("Referer", format!("{host}/"))
        .header("Origin", host)
        .header("Cookie", &base)
        .header("X-Requested-With", "XMLHttpRequest")
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(encode_form(&form))
        .timeout(Duration::from_secs(12))
        .send()
        .await
    else {
        return base;
    };

    let mut pairs = vec![base.clone()];
    for val in resp.headers().get_all(reqwest::header::SET_COOKIE) {
        if let Ok(s) = val.to_str() {
            if let Some(kv) = s.split(';').next() {
                let kv = kv.trim();
                if !kv.contains("=deleted") && !kv.is_empty() {
                    pairs.push(kv.to_string());
                }
            }
        }
    }
    let merged = pairs.join("; ");
    if merged.contains("dle_user_id=") {
        session_cache().lock().insert(key, (Instant::now(), merged.clone()));
    }
    merged
}

fn friendly_err(host: &str, err: reqwest::Error) -> String {
    if err.is_timeout() {
        format!("таймаут подключения к {host}")
    } else if err.is_connect() {
        format!("адрес {host} недоступен (укажите рабочее зеркало HDrezka в настройках)")
    } else {
        format!("ошибка запроса к {host}")
    }
}

fn detect_block(html: &str) -> Option<&'static str> {
    if html.contains("id=\"anubis_challenge\"") {
        return Some("зеркало защищено проверкой — укажите личное зеркало (например, из @hdrezka_bot), cookie или логин/пароль");
    }
    if html.contains("id=\"check-form\"") || html.contains("id=\"loginForm\"") {
        return Some("зеркало требует вход — укажите логин/пароль или cookie HDrezka в настройках (либо личное зеркало из @hdrezka_bot)");
    }
    if html.contains("<span>MIRROR</span>") {
        return Some("зеркало требует подтверждение в браузере — укажите другое зеркало или cookie");
    }
    None
}

async fn post_form(host: &str, path: &str, cookie: &str, form: &[(&str, &str)]) -> Result<String, String> {
    let url = format!("{host}/{}", path.trim_start_matches('/'));
    let resp = http()
        .post(&url)
        .header("User-Agent", CHROME_UA)
        .header("Referer", format!("{host}/"))
        .header("Origin", host)
        .header("Cookie", cookie)
        .header("X-Requested-With", "XMLHttpRequest")
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(encode_form(form))
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .map_err(|e| friendly_err(host, e))?;
    if !resp.status().is_success() {
        return Err(format!("{host} ответил HTTP {}", resp.status()));
    }
    resp.text().await.map_err(|e| e.to_string())
}

async fn get_page(url: &str, host: &str, cookie: &str) -> Result<String, String> {
    let resp = http()
        .get(url)
        .header("User-Agent", CHROME_UA)
        .header("Referer", format!("{host}/"))
        .header("Origin", host)
        .header("Cookie", cookie)
        .timeout(Duration::from_secs(12))
        .send()
        .await
        .map_err(|e| friendly_err(host, e))?;
    if !resp.status().is_success() {
        return Err(format!("{host} ответил HTTP {}", resp.status()));
    }
    resp.text().await.map_err(|e| e.to_string())
}

struct Hit {
    title: String,
    orig_title: String,
    year: Option<u32>,
    url: String,
}

fn clean_query(title: &str) -> String {
    title.split([':', '/', '|']).next().unwrap_or(title).trim().to_string()
}

fn parse_search(html: &str, host: &str) -> Vec<Hit> {
    let re_li = Regex::new(r#"(?s)<li>\s*<a[^>]+href="([^"]+)"[^>]*>(.*?)</a>\s*</li>"#).unwrap();
    let re_enty = Regex::new(r#"(?s)<span class="enty"[^>]*>(.*?)</span>"#).unwrap();
    let re_rating = Regex::new(r#"(?s)<span class="rating"[^>]*>.*?</span>"#).unwrap();
    let re_meta = Regex::new(r#"\(([^()]*)\)\s*$"#).unwrap();
    let re_year = Regex::new(r#"\b(19\d{2}|20\d{2})\b"#).unwrap();

    let mut out = Vec::new();
    for cap in re_li.captures_iter(html) {
        let raw_url = &cap[1];
        let inner = &cap[2];
        let url = if raw_url.starts_with("http") {
            raw_url.to_string()
        } else {
            format!("{host}/{}", raw_url.trim_start_matches('/'))
        };
        let title = re_enty.captures(inner).map(|c| strip_tags(&c[1])).unwrap_or_else(|| strip_tags(inner));
        let without_enty = re_enty.replace_all(inner, "").into_owned();
        let tail = strip_tags(&re_rating.replace_all(&without_enty, ""));
        let mut orig_title = String::new();
        let mut year = None;
        if let Some(m) = re_meta.captures(&tail) {
            let inside = &m[1];
            if let Some(y) = re_year.find(inside).and_then(|y| y.as_str().parse::<u32>().ok()) {
                year = Some(y);
            }
            if let Some(comma) = inside.find(',') {
                orig_title = inside[..comma].trim().to_string();
            }
        }
        if !title.is_empty() {
            out.push(Hit { title, orig_title, year, url });
        }
    }
    out
}

fn pick_hit<'a>(hits: &'a [Hit], q: &TitleQuery) -> Option<&'a Hit> {
    let want_ru = norm_cmp(&q.title);
    let want_orig = q.original_title.as_deref().map(norm_cmp).unwrap_or_default();
    hits.iter().max_by_key(|h| {
        let mut score = 0i32;
        if let (Some(wy), Some(hy)) = (q.year, h.year) {
            if wy == hy {
                score += 50;
            } else if wy.abs_diff(hy) <= 1 {
                score += 20;
            } else {
                score -= 20;
            }
        }
        let ht = norm_cmp(&h.title);
        let ho = norm_cmp(&h.orig_title);
        if ht == want_ru || (!want_orig.is_empty() && ho == want_orig) {
            score += 40;
        } else if ht.contains(&want_ru) || want_ru.contains(&ht) {
            score += 15;
        }
        score
    })
}

async fn search_on_host(server: &OnlineSource, host: &str, q: &TitleQuery) -> Result<Vec<Hit>, String> {
    let cookie = ensure_cookie(server, host).await;
    let queries = [q.title.clone(), clean_query(&q.title), q.original_title.clone().unwrap_or_default()];
    let mut tried = Vec::new();
    for term in queries {
        let term = term.trim().to_string();
        if term.is_empty() || tried.contains(&term) {
            continue;
        }
        tried.push(term.clone());
        let html = post_form(host, "engine/ajax/search.php", &cookie, &[("q", &term)]).await?;
        if let Some(msg) = detect_block(&html) {
            return Err(msg.to_string());
        }
        let hits = parse_search(&html, host);
        if !hits.is_empty() {
            return Ok(hits);
        }
    }
    Ok(Vec::new())
}

pub async fn check(server: &OnlineSource, q: &TitleQuery) -> Result<Option<ProviderHit>, String> {
    let hosts = candidate_hosts(server);
    let mut last_err = None;
    let mut hits = Vec::new();

    for host in &hosts {
        match search_on_host(server, host, q).await {
            Ok(h) if !h.is_empty() => {
                hits = h;
                last_err = None;
                break;
            }
            Ok(_) => {
                last_err = None;
                break;
            }
            Err(e) => {
                last_err = Some(e);
            }
        }
    }

    if let Some(err) = last_err {
        return Err(err);
    }

    let Some(best) = pick_hit(&hits, q) else {
        return Ok(None);
    };

    Ok(Some(ProviderHit {
        server: server.name.clone(),
        name: "HDrezka".into(),
        balancer: "rezka".into(),
        url: plugin_url(&server.id, "rezka", "browse", &[("page_url", &best.url), ("title", &best.title)]),
    }))
}

pub async fn browse(server: &OnlineSource, params: &HashMap<String, String>) -> Result<BrowseResult, OnlineError> {
    let page_url = params
        .get("page_url")
        .filter(|s| !s.is_empty())
        .ok_or_else(|| OnlineError::bad_request("HDrezka: отсутствует адрес страницы"))?;
    let host = host_from_url(page_url, &primary_host(server));
    let cookie = ensure_cookie(server, &host).await;

    let html = get_page(page_url, &host, &cookie)
        .await
        .map_err(|e| OnlineError::upstream("rezka_error", format!("HDrezka: {e}")))?;

    let flat = html.replace('\n', " ");
    let re_series = Regex::new(r"\.initCDNSeriesEvents\(\s*(\d+)\s*,\s*(\d+)\s*,\s*(\d+)\s*,\s*(\d+)").unwrap();
    let re_movie = Regex::new(r"\.initCDNMoviesEvents\(\s*(\d+)\s*,\s*(\d+)\s*,\s*(\d+)\s*,\s*(\d+)\s*,\s*(\d+)").unwrap();
    let re_favs = Regex::new(r#"id="ctrl_favs"\s+value="([^"]*)""#).unwrap();

    let favs = re_favs.captures(&flat).map(|c| c[1].to_string()).unwrap_or_default();
    let mut is_series = false;
    let film_id;
    let def_tr_id;
    let mut def_flags = ("0".to_string(), "0".to_string(), "0".to_string());
    let mut def_season = "1".to_string();
    let mut def_episode = "1".to_string();

    if let Some(c) = re_series.captures(&flat) {
        is_series = true;
        film_id = c[1].to_string();
        def_tr_id = c[2].to_string();
        def_season = c[3].to_string();
        def_episode = c[4].to_string();
    } else if let Some(c) = re_movie.captures(&flat) {
        film_id = c[1].to_string();
        def_tr_id = c[2].to_string();
        def_flags = (c[3].to_string(), c[4].to_string(), c[5].to_string());
    } else if let Some(msg) = detect_block(&html) {
        return Ok(BrowseResult::message(&format!("HDrezka: {msg}")));
    } else {
        return Ok(BrowseResult::message("HDrezka: плеер на странице не найден"));
    }

    let mut translators = parse_translators(&flat);
    if translators.is_empty() && !def_tr_id.is_empty() {
        translators.push(Translator {
            id: def_tr_id.clone(),
            name: "Оригинал".into(),
            camrip: def_flags.0,
            ads: def_flags.1,
            director: def_flags.2,
        });
    }

    let active_tr_id = params
        .get("translator_id")
        .filter(|s| translators.iter().any(|t| &t.id == *s))
        .cloned()
        .or_else(|| translators.first().map(|t| t.id.clone()))
        .unwrap_or(def_tr_id.clone());

    let title = params.get("title").map(String::as_str).unwrap_or("Фильм");
    let voices: Vec<VoiceOption> = translators
        .iter()
        .map(|t| VoiceOption {
            name: t.name.clone(),
            url: Some(plugin_url(&server.id, "rezka", "browse", &[("page_url", page_url), ("title", title), ("translator_id", &t.id)])),
            active: t.id == active_tr_id,
        })
        .collect();

    if !is_series {
        let tr = translators.iter().find(|t| t.id == active_tr_id);
        let cam = tr.map(|t| t.camrip.as_str()).unwrap_or("0");
        let ads = tr.map(|t| t.ads.as_str()).unwrap_or("0");
        let dir = tr.map(|t| t.director.as_str()).unwrap_or("0");
        let tr_name = tr.map(|t| t.name.as_str()).unwrap_or("");
        return Ok(BrowseResult {
            voices,
            items: vec![BrowseItem {
                method: "call".into(),
                title: if tr_name.is_empty() { title.to_string() } else { format!("{title} ({tr_name})") },
                season: None,
                episode: None,
                voice: if tr_name.is_empty() { None } else { Some(tr_name.to_string()) },
                details: None,
                similar: false,
                url: Some(plugin_url(
                    &server.id,
                    "rezka",
                    "resolve",
                    &[("host", &host), ("film_id", &film_id), ("translator_id", &active_tr_id), ("favs", &favs), ("is_series", "0"), ("camrip", cam), ("ads", ads), ("director", dir)],
                )),
                stream: None,
            }],
            message: None,
        });
    }

    let (seasons_html, episodes_html) = if active_tr_id != def_tr_id {
        let ts = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
        let resp_text = post_form(
            &host,
            &format!("ajax/get_cdn_series/?t={ts}"),
            &cookie,
            &[("id", &film_id), ("translator_id", &active_tr_id), ("favs", &favs), ("action", "get_episodes")],
        )
        .await
        .map_err(|e| OnlineError::upstream("rezka_error", format!("HDrezka: {e}")))?;
        let v: Value = serde_json::from_str(&resp_text).unwrap_or(Value::Null);
        (
            v.get("seasons").and_then(Value::as_str).unwrap_or("").to_string(),
            v.get("episodes").and_then(Value::as_str).unwrap_or("").to_string(),
        )
    } else {
        (flat.clone(), flat.clone())
    };

    let mut seasons = parse_seasons(&seasons_html);
    if seasons.is_empty() {
        seasons.push((def_season.clone(), format!("Сезон {def_season}")));
    }
    let mut episodes = parse_episodes(&episodes_html);
    if episodes.is_empty() {
        episodes.push((def_season, def_episode.clone(), format!("Серия {def_episode}")));
    }

    if seasons.len() > 1 && !params.contains_key("season") {
        let items = seasons
            .iter()
            .map(|(sid, sname)| BrowseItem {
                method: "link".into(),
                title: sname.clone(),
                season: sid.parse::<u32>().ok(),
                episode: None,
                voice: None,
                details: None,
                similar: false,
                url: Some(plugin_url(&server.id, "rezka", "browse", &[("page_url", page_url), ("title", title), ("translator_id", &active_tr_id), ("season", sid)])),
                stream: None,
            })
            .collect();
        return Ok(BrowseResult { voices, items, message: None });
    }

    let want_season = params.get("season").cloned().unwrap_or_else(|| seasons[0].0.clone());
    let items = episodes
        .into_iter()
        .filter(|(s, _, _)| s == &want_season)
        .map(|(s, e, ename)| BrowseItem {
            method: "call".into(),
            title: ename,
            season: s.parse::<u32>().ok(),
            episode: e.parse::<u32>().ok(),
            voice: None,
            details: None,
            similar: false,
            url: Some(plugin_url(
                &server.id,
                "rezka",
                "resolve",
                &[("host", &host), ("film_id", &film_id), ("translator_id", &active_tr_id), ("season", &s), ("episode", &e), ("favs", &favs), ("is_series", "1")],
            )),
            stream: None,
        })
        .collect();

    Ok(BrowseResult { voices, items, message: None })
}

struct Translator {
    id: String,
    name: String,
    camrip: String,
    ads: String,
    director: String,
}

fn attr_val(tag: &str, name: &str) -> Option<String> {
    let re = Regex::new(&format!(r#"{}=["']([^"']*)["']"#, regex::escape(name))).ok()?;
    re.captures(tag).map(|c| c[1].trim().to_string())
}

fn parse_translators(html: &str) -> Vec<Translator> {
    let re_ul = Regex::new(r#"(?s)<ul[^>]+id="translators-list"[^>]*>(.*?)</ul>"#).unwrap();
    let Some(ul_cap) = re_ul.captures(html) else {
        return Vec::new();
    };
    let ul = &ul_cap[1];
    let re_li = Regex::new(r#"(?s)<li[^>]*>(.*?)</li>"#).unwrap();
    let mut out = Vec::new();
    for cap in re_li.captures_iter(ul) {
        let tag = &cap[0];
        let Some(id) = attr_val(tag, "data-translator_id") else { continue };
        let name = attr_val(tag, "title").filter(|s| !s.is_empty()).unwrap_or_else(|| strip_tags(&cap[1]));
        out.push(Translator {
            id,
            name: if name.is_empty() { "Перевод".into() } else { name },
            camrip: attr_val(tag, "data-camrip").unwrap_or_else(|| "0".into()),
            ads: attr_val(tag, "data-ads").unwrap_or_else(|| "0".into()),
            director: attr_val(tag, "data-director").unwrap_or_else(|| "0".into()),
        });
    }
    out
}

fn parse_seasons(html: &str) -> Vec<(String, String)> {
    let re = Regex::new(r#"(?s)<li[^>]*b-simple_season__item[^>]*>(.*?)</li>"#).unwrap();
    re.captures_iter(html)
        .filter_map(|cap| attr_val(&cap[0], "data-tab_id").map(|id| (id, strip_tags(&cap[1]))))
        .collect()
}

fn parse_episodes(html: &str) -> Vec<(String, String, String)> {
    let re = Regex::new(r#"(?s)<li[^>]*b-simple_episode__item[^>]*>(.*?)</li>"#).unwrap();
    re.captures_iter(html)
        .filter_map(|cap| {
            let tag = &cap[0];
            Some((attr_val(tag, "data-season_id")?, attr_val(tag, "data-episode_id")?, strip_tags(&cap[1])))
        })
        .collect()
}

pub async fn resolve(server: &OnlineSource, params: &HashMap<String, String>) -> Result<RawStream, OnlineError> {
    let host = params
        .get("host")
        .filter(|s| !s.is_empty())
        .map(|s| normalize_host(s, "https://rezka.ag"))
        .unwrap_or_else(|| primary_host(server));
    let cookie = ensure_cookie(server, &host).await;
    let film_id = params.get("film_id").map(String::as_str).unwrap_or("");
    let tr_id = params.get("translator_id").map(String::as_str).unwrap_or("");
    let favs = params.get("favs").map(String::as_str).unwrap_or("");
    let is_series = params.get("is_series").map(String::as_str) == Some("1");

    let mut form = vec![("id", film_id), ("translator_id", tr_id), ("favs", favs)];
    let season = params.get("season").map(String::as_str).unwrap_or("1");
    let episode = params.get("episode").map(String::as_str).unwrap_or("1");
    let cam = params.get("camrip").map(String::as_str).unwrap_or("0");
    let ads = params.get("ads").map(String::as_str).unwrap_or("0");
    let dir = params.get("director").map(String::as_str).unwrap_or("0");

    if is_series {
        form.extend([("season", season), ("episode", episode), ("action", "get_stream")]);
    } else {
        form.extend([("is_camrip", cam), ("is_ads", ads), ("is_director", dir), ("action", "get_movie")]);
    }

    let ts = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
    let resp_text = post_form(&host, &format!("ajax/get_cdn_series/?t={ts}"), &cookie, &form)
        .await
        .map_err(|e| OnlineError::upstream("rezka_error", format!("HDrezka: {e}")))?;
    let v: Value = serde_json::from_str(&resp_text)
        .map_err(|_| OnlineError::upstream("rezka_json", "HDrezka вернула некорректный ответ"))?;

    let raw_url = v.get("url").and_then(Value::as_str).filter(|s| !s.is_empty()).ok_or_else(|| {
        let msg = v.get("message").and_then(Value::as_str).unwrap_or("поток недоступен");
        OnlineError::upstream("rezka_nostream", format!("HDrezka: {msg}"))
    })?;

    let decoded = decode_rezka(raw_url);
    let referer = format!("{host}/");
    let headers = header_map(&[("Referer", &referer), ("Origin", &host), ("User-Agent", CHROME_UA)]);

    let qualities = parse_rezka_streams(&decoded);
    let main = qualities
        .first()
        .map(|q| q.url.clone())
        .ok_or_else(|| OnlineError::upstream("rezka_nostream", "HDrezka: не удалось разобрать ссылки потока"))?;

    let subtitles = v
        .get("subtitle")
        .and_then(Value::as_str)
        .map(parse_rezka_subtitles)
        .unwrap_or_default();

    Ok(RawStream {
        hls: is_playlist_url(&main),
        url: main,
        headers,
        qualities,
        subtitles,
    })
}

pub fn decode_rezka(data: &str) -> String {
    if !data.starts_with('#') {
        return data.to_string();
    }
    let mut x = data.chars().skip(2).collect::<String>();
    for trash in ["$$!!@$$@^!@#$$@", "@@@@@!##!^^^", "####^!!##!@@", "^^^!@##!!##", "$$#!!@#!@##"] {
        let marker = format!("//_//{}", base64::engine::general_purpose::STANDARD.encode(trash.as_bytes()));
        x = x.replace(&marker, "");
    }
    base64::engine::general_purpose::STANDARD
        .decode(x.as_bytes())
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .unwrap_or_default()
}

fn parse_playlist_items(s: &str) -> Vec<(String, Vec<String>)> {
    let mut out = Vec::new();
    let trimmed = s.trim();
    if !trimmed.starts_with('[') {
        if !trimmed.is_empty() {
            let links = trimmed.split(" or ").map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect();
            out.push(("Auto".to_string(), links));
        }
        return out;
    }
    let re_split = Regex::new(r",\s*\[").unwrap();
    for part in re_split.split(&trimmed[1..]) {
        let item = part.trim().trim_end_matches(',').trim();
        let Some(end) = item.find(']') else { continue };
        let label = item[..end].trim().to_string();
        let links: Vec<String> = item[end + 1..].trim().split(" or ").map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect();
        if !links.is_empty() {
            out.push((label, links));
        }
    }
    out
}

fn quality_rank(label: &str) -> u32 {
    if let Some(m) = Regex::new(r"(\d+)K").ok().and_then(|r| r.captures(label)) {
        return m[1].parse::<u32>().unwrap_or(0) * 1000;
    }
    if let Some(m) = Regex::new(r"(\d{3,})").ok().and_then(|r| r.captures(label)) {
        return m[1].parse::<u32>().unwrap_or(0);
    }
    0
}

pub fn parse_rezka_streams(decoded: &str) -> Vec<RawQuality> {
    let mut items: Vec<(u32, RawQuality)> = Vec::new();
    for (label, links) in parse_playlist_items(decoded) {
        let chosen = links
            .iter()
            .find(|u| u.to_ascii_lowercase().contains(".m3u8"))
            .or_else(|| links.first())
            .cloned()
            .unwrap_or_default();
        if chosen.is_empty() {
            continue;
        }
        let url = if chosen.starts_with("//") { format!("https:{chosen}") } else { chosen };
        items.push((quality_rank(&label), RawQuality { label, url }));
    }
    items.sort_by_key(|(rank, _)| std::cmp::Reverse(*rank));
    items.into_iter().map(|(_, v)| v).collect()
}

pub fn parse_rezka_subtitles(raw: &str) -> Vec<RawSubtitle> {
    parse_playlist_items(raw)
        .into_iter()
        .filter_map(|(label, links)| {
            let link = links.into_iter().next()?;
            let url = if link.starts_with("//") { format!("https:{link}") } else { link };
            Some(RawSubtitle { label, url })
        })
        .collect()
}
