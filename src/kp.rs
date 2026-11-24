use crate::util::{http, CHROME_UA};
use parking_lot::Mutex;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

const KODIK_SEARCH: &str = "https://kodik-api.com/search?token=41dd95f84c21719b09d6c71182237a25&limit=1";

fn cache() -> &'static Mutex<HashMap<String, (Instant, Option<u64>)>> {
    static C: OnceLock<Mutex<HashMap<String, (Instant, Option<u64>)>>> = OnceLock::new();
    C.get_or_init(Default::default)
}

/// Определяет `kinopoisk_id` по `imdb_id` через Wikidata (свойство P2603) и резервно через Kodik.
pub async fn resolve_kp_by_imdb(imdb_id: &str) -> Option<u64> {
    let imdb = imdb_id.trim();
    if !imdb.starts_with("tt") {
        return None;
    }
    if let Some(hit) = cache().lock().get(imdb).and_then(|(at, v)| (at.elapsed() < Duration::from_secs(3600)).then_some(*v)) {
        return hit;
    }

    let kp = wikidata_kp(imdb).await.or(kodik_kp(imdb).await);

    let mut guard = cache().lock();
    if guard.len() > 256 {
        guard.retain(|_, (at, _)| at.elapsed() < Duration::from_secs(3600));
    }
    guard.insert(imdb.to_string(), (Instant::now(), kp));
    kp
}

async fn wikidata_kp(imdb: &str) -> Option<u64> {
    let search_url = format!("https://www.wikidata.org/w/api.php?action=query&list=search&srsearch=haswbstatement:P345={imdb}&format=json");
    let resp = http().get(search_url).header("User-Agent", "Cinema/0.1.4").timeout(Duration::from_secs(4)).send().await.ok()?;
    let s: Value = resp.json().await.ok()?;
    let qid = s.pointer("/query/search/0/title").and_then(Value::as_str)?;
    if !qid.starts_with('Q') {
        return None;
    }

    let ent_url = format!("https://www.wikidata.org/w/api.php?action=wbgetentities&ids={qid}&props=claims&format=json");
    let resp2 = http().get(ent_url).header("User-Agent", "Cinema/0.1.4").timeout(Duration::from_secs(4)).send().await.ok()?;
    let ent: Value = resp2.json().await.ok()?;
    ent.pointer(&format!("/entities/{qid}/claims/P2603/0/mainsnak/datavalue/value"))
        .and_then(|v| v.as_str().and_then(|s| s.parse::<u64>().ok()).or_else(|| v.as_u64()))
}

async fn kodik_kp(imdb: &str) -> Option<u64> {
    let url = format!("{KODIK_SEARCH}&imdb_id={imdb}");
    let resp = http().get(url).header("User-Agent", CHROME_UA).timeout(Duration::from_secs(4)).send().await.ok()?;
    let v: Value = resp.json().await.ok()?;
    v.pointer("/results/0/kinopoisk_id")
        .and_then(|x| x.as_str().and_then(|s| s.parse::<u64>().ok()).or_else(|| x.as_u64()))
}
