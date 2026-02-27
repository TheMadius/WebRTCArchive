use anyhow::{anyhow, bail, Result};
use serde::Deserialize;
use serde_json::Value;

/// Извлекает хост из URL сигналинга (например https://10.24.88.31:7200/webrtc... -> Some("10.24.88.31")).
pub fn host_from_webrtc_url(url: &str) -> Option<String> {
    url::Url::parse(url).ok().and_then(|u| u.host_str().map(str::to_string))
}

#[derive(Debug, Clone, Copy)]
pub struct HttpTlsOptions {
    pub insecure_skip_verify: bool,
}

#[derive(Debug, Deserialize)]
pub struct AnswerResponse {
    pub sdp: String,
    #[allow(dead_code)]
    pub code: Option<i32>,
    #[allow(dead_code)]
    pub id: Option<String>,
    #[allow(dead_code)]
    pub r#type: Option<String>,
}

pub async fn send_offer(url: &str, offer_sdp: &str, tls: HttpTlsOptions) -> Result<AnswerResponse> {
    let client = reqwest::ClientBuilder::new()
        .danger_accept_invalid_certs(tls.insecure_skip_verify)
        .build()?;

    // Сервер ожидает SDP в теле POST (строкой).
    // (На практике некоторые реализации принимают и JSON, но ваш сервер ругался на SDP парсер.)
    // Отладка: сохраняем последний offer, который реально ушёл на сервер.
    if let Err(e) = std::fs::write("offer_sent.sdp", offer_sdp) {
        log::warn!("Failed to write offer_sent.sdp: {:?}", e);
    }

    let resp = client
        .post(url)
        .header(reqwest::header::CONTENT_TYPE, "application/sdp")
        .body(offer_sdp.to_string())
        .send()
        .await?;

    let status = resp.status();
    let text = resp.text().await?;
    // Отладка: сохраняем тело HTTP-ответа сервера.
    if let Err(e) = std::fs::write("answer_raw.json", &text) {
        log::warn!("Failed to write answer_raw.json: {:?}", e);
    }
    if !status.is_success() {
        bail!("HTTP {}: {}", status, text);
    }

    // Сервер может вернуть как минимум два формата:
    // 1) {"sdp":"..."}
    // 2) {"code":0,"id":"...","sdp":"...","type":"answer"}
    let v: Value = serde_json::from_str(&text)
        .map_err(|e| anyhow!("invalid JSON answer: {e}; body={text}"))?;

    let sdp = v
        .get("sdp")
        .and_then(|x| x.as_str())
        .ok_or_else(|| anyhow!("missing field `sdp` in answer JSON: {text}"))?
        .to_string();

    Ok(AnswerResponse {
        sdp,
        code: v.get("code").and_then(|x| x.as_i64()).map(|x| x as i32),
        id: v.get("id").and_then(|x| x.as_str()).map(|x| x.to_string()),
        r#type: v
            .get("type")
            .and_then(|x| x.as_str())
            .map(|x| x.to_string()),
    })
}

pub fn extract_h26x_rtpmap_lines(sdp: &str) -> Vec<String> {
    sdp.lines()
        .filter(|l| l.contains("a=rtpmap:"))
        .filter(|l| {
            let lower = l.to_ascii_lowercase();
            lower.contains("h264") || lower.contains("h265")
        })
        .map(|l| l.to_string())
        .collect()
}

