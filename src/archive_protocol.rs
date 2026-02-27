//! Протокол управления архивом по WEBRTC.md (Data Channel JSON).

use serde::{Deserialize, Serialize};

/// Один временной диапазон архива (мс с 1970-01-01).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimeRange {
    pub start_time: u64,
    pub end_time: u64,
    pub duration: u64,
}

// --- Запросы от клиента ---

#[derive(Debug, Serialize)]
pub struct GetRangesRequest {
    #[serde(rename = "type")]
    pub typ: &'static str,
    pub data: GetRangesData,
}

#[derive(Debug, Serialize)]
pub struct GetRangesData {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_time: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_time: Option<u64>,
}

#[derive(Debug, Serialize)]
pub struct GetArchiveFragmentRequest {
    #[serde(rename = "type")]
    pub typ: &'static str,
    pub data: GetArchiveFragmentData,
}

#[derive(Debug, Serialize)]
pub struct GetArchiveFragmentData {
    pub start_time: u64,
    pub duration: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub type_meta: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct DropBufferRequest {
    #[serde(rename = "type")]
    pub typ: &'static str,
    pub data: serde_json::Value,
}

#[derive(Debug, Serialize)]
pub struct PlayStreamRequest {
    #[serde(rename = "type")]
    pub typ: &'static str,
    pub data: serde_json::Value,
}

#[derive(Debug, Serialize)]
pub struct StopStreamRequest {
    #[serde(rename = "type")]
    pub typ: &'static str,
    pub data: serde_json::Value,
}

// --- Ответы сервера ---

#[derive(Debug, Deserialize)]
pub struct ServerMessage {
    #[serde(rename = "type")]
    pub typ: Option<String>,
    pub data: Option<serde_json::Value>,
    pub error: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct RangesResponseData {
    pub ranges: Vec<TimeRange>,
}

#[derive(Debug, Deserialize)]
pub struct ArchiveFragmentResponseData {
    pub start_time: u64,
    pub end_time: u64,
    #[allow(dead_code)]
    pub duration: u64,
}

// --- Конструкторы запросов ---

pub fn get_ranges(start_time: Option<u64>, end_time: Option<u64>) -> GetRangesRequest {
    GetRangesRequest {
        typ: "get_ranges",
        data: GetRangesData {
            start_time,
            end_time,
        },
    }
}

pub fn get_archive_fragment(
    start_time: u64,
    duration_ms: i64,
    with_key_frame: bool,
) -> GetArchiveFragmentRequest {
    GetArchiveFragmentRequest {
        typ: "get_archive_fragment",
        data: GetArchiveFragmentData {
            start_time,
            duration: duration_ms,
            key: Some(with_key_frame),
            type_meta: None,
        },
    }
}

pub fn drop_buffer() -> DropBufferRequest {
    DropBufferRequest {
        typ: "drop_buffer",
        data: serde_json::json!({}),
    }
}

pub fn play_stream() -> PlayStreamRequest {
    PlayStreamRequest {
        typ: "play_stream",
        data: serde_json::json!({}),
    }
}

pub fn stop_stream() -> StopStreamRequest {
    StopStreamRequest {
        typ: "stop_stream",
        data: serde_json::json!({}),
    }
}
