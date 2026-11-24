use crate::util::{http, normalize_host};
use crate::{BrowseResult, OnlineError, OnlineSource, PluginManifest, ProviderHit, RawStream, TitleQuery};
use serde_json::{json, Value};
use std::time::Duration;

/// Загружает JSON-манифест плагина Cinema (например, с GitHub).
pub async fn fetch_manifest(url: &str) -> Result<PluginManifest, OnlineError> {
    let target = normalize_host(url, "");
    if target.is_empty() {
        return Err(OnlineError::bad_request("Укажите URL манифеста плагина (.json)"));
    }
    let resp = http()
        .get(&target)
        .timeout(Duration::from_secs(15))
        .send()
        .await
        .map_err(|e| OnlineError::upstream("manifest_unreachable", format!("Манифест недоступен: {e}")))?;
    if !resp.status().is_success() {
        return Err(OnlineError::upstream("manifest_status", format!("Манифест ответил {}", resp.status())));
    }
    resp.json::<PluginManifest>()
        .await
        .map_err(|e| OnlineError::upstream("manifest_json", format!("Неверный формат манифеста Cinema: {e}")))
}

/// Запрос к собственному удалённому HTTP-провайдеру Cinema (`POST {url}/providers`).
pub async fn remote_check(server: &OnlineSource, q: &TitleQuery, uid: &str) -> Result<Vec<ProviderHit>, String> {
    let base = normalize_host(&server.url, "");
    if base.is_empty() {
        return Ok(Vec::new());
    }
    let mut req = http()
        .post(format!("{base}/providers"))
        .header("X-Cinema-UID", uid)
        .json(q)
        .timeout(Duration::from_secs(20));
    if !server.token.trim().is_empty() {
        req = req.bearer_auth(server.token.trim());
    }
    let resp = req.send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status()));
    }
    let v: Value = resp.json().await.map_err(|e| e.to_string())?;
    let list = v.get("providers").and_then(Value::as_array).cloned().unwrap_or_default();
    Ok(list
        .into_iter()
        .filter_map(|it| {
            let name = it.get("name").and_then(Value::as_str)?.to_string();
            let raw_url = it.get("url").and_then(Value::as_str)?;
            let abs = if raw_url.starts_with("http://") || raw_url.starts_with("https://") {
                raw_url.to_string()
            } else {
                format!("{base}/{}", raw_url.trim_start_matches('/'))
            };
            Some(ProviderHit {
                server: server.name.clone(),
                name,
                balancer: it.get("balancer").or_else(|| it.get("balanser")).and_then(Value::as_str).unwrap_or("").to_string(),
                quality: it.get("quality").and_then(Value::as_str).map(str::to_string),
                url: abs,
            })
        })
        .collect())
}

pub async fn remote_browse(server: &OnlineSource, link: &str, uid: &str) -> Result<BrowseResult, OnlineError> {
    let base = normalize_host(&server.url, "");
    let mut req = http()
        .post(format!("{base}/browse"))
        .header("X-Cinema-UID", uid)
        .json(&json!({ "url": link }))
        .timeout(Duration::from_secs(20));
    if !server.token.trim().is_empty() {
        req = req.bearer_auth(server.token.trim());
    }
    let resp = req
        .send()
        .await
        .map_err(|e| OnlineError::upstream("remote_unreachable", e.to_string()))?;
    if !resp.status().is_success() {
        return Err(OnlineError::upstream("remote_status", format!("Сервер ответил {}", resp.status())));
    }
    resp.json::<BrowseResult>()
        .await
        .map_err(|e| OnlineError::upstream("remote_json", e.to_string()))
}

pub async fn remote_resolve(server: &OnlineSource, link: &str, uid: &str) -> Result<RawStream, OnlineError> {
    let base = normalize_host(&server.url, "");
    let mut req = http()
        .post(format!("{base}/resolve"))
        .header("X-Cinema-UID", uid)
        .json(&json!({ "url": link }))
        .timeout(Duration::from_secs(20));
    if !server.token.trim().is_empty() {
        req = req.bearer_auth(server.token.trim());
    }
    let resp = req
        .send()
        .await
        .map_err(|e| OnlineError::upstream("remote_unreachable", e.to_string()))?;
    if !resp.status().is_success() {
        return Err(OnlineError::upstream("remote_status", format!("Сервер ответил {}", resp.status())));
    }
    resp.json::<RawStream>()
        .await
        .map_err(|e| OnlineError::upstream("remote_json", e.to_string()))
}
