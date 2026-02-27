//! Цикл управления архивом: приём/отправка по Data Channel, пополнение буфера за 2 сек до конца.

use crate::app_state::{ArchiveCommand, ArchiveState};
use crate::archive_protocol::{
    get_archive_fragment, get_ranges, drop_buffer, play_stream, stop_stream,
    ArchiveFragmentResponseData, ServerMessage, RangesResponseData,
};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use webrtc::data_channel::RTCDataChannel;

// Длительность одного фрагмента архива (10 секунд).
const FRAGMENT_DURATION_MS: i64 = 10_000;
const REQUEST_NEXT_FRAGMENT_BEFORE_END_MS: u64 = 2_000; // запрашивать следующий фрагмент за 2 сек до конца

/// Запускает цикл: при открытии Data Channel отправляет get_ranges,
/// обрабатывает ответы (ranges, archive_fragment) из message_rx, обрабатывает команды (PlayFrom, Stop).
/// message_rx должен быть привязан к on_message при создании DC (в build_offer), иначе ответы можно пропустить.
pub async fn run_archive_loop(
    dc: Arc<RTCDataChannel>,
    state: Arc<ArchiveState>,
    mut cmd_rx: mpsc::Receiver<ArchiveCommand>,
    mut msg_rx: mpsc::Receiver<String>,
) {
    let session_id = Arc::new(AtomicU64::new(0));

    // Не вызываем dc.on_open здесь — он уже установлен в webrtc_offer (on_message + get_ranges). Иначе перезаписали бы и сообщения не доходили бы.

    loop {
        tokio::select! {
            Some(cmd) = cmd_rx.recv() => {
                match cmd {
                    ArchiveCommand::GetRanges { start_time, end_time } => {
                        let req = get_ranges(start_time, end_time);
                        if let Ok(json) = serde_json::to_string(&req) {
                            if let Err(e) = dc.send_text(json).await {
                                log::error!("Failed to send get_ranges: {:?}", e);
                            }
                        }
                    }
                    ArchiveCommand::PlayFrom { timestamp_ms } => {
                        session_id.fetch_add(1, Ordering::SeqCst);
                        if let Err(e) = dc.send_text(serde_json::to_string(&drop_buffer()).unwrap()).await {
                            log::error!("drop_buffer send error: {:?}", e);
                        }
                        let req = get_archive_fragment(timestamp_ms, FRAGMENT_DURATION_MS, true);
                        if let Ok(json) = serde_json::to_string(&req) {
                            if let Err(e) = dc.send_text(json).await {
                                log::error!("get_archive_fragment (key) send error: {:?}", e);
                            }
                        }
                        state.set_playback_span(timestamp_ms, timestamp_ms + FRAGMENT_DURATION_MS as u64);
                        if let Err(e) = dc.send_text(serde_json::to_string(&play_stream()).unwrap()).await {
                            log::error!("play_stream send error: {:?}", e);
                        }
                        log::info!("PlayFrom {} (with key frame), play_stream sent", timestamp_ms);
                        // Следующий фрагмент планируем только по ответу archive_fragment, чтобы не дублировать запрос.
                    }
                    ArchiveCommand::Stop => {
                        let req = stop_stream();
                        if let Ok(json) = serde_json::to_string(&req) {
                            let _ = dc.send_text(json).await;
                        }
                    }
                }
            }
            Some(json_str) = msg_rx.recv() => {
                log::info!(
                    "[DC] archive_loop: received message len={}, preview={}",
                    json_str.len(),
                    json_str.trim().chars().take(100).collect::<String>()
                );
                if let Ok(msg) = serde_json::from_str::<ServerMessage>(&json_str) {
                    if let Some(err) = msg.error {
                        log::warn!("[DC] Server error: {}", err);
                        continue;
                    }
                    log::info!("[DC] parsed message type={:?}", msg.typ);
                    if let (Some(typ), Some(data)) = (msg.typ.as_deref(), msg.data) {
                        match typ {
                            "ranges" => {
                                if let Ok(ranges_data) = serde_json::from_value::<RangesResponseData>(data.clone()) {
                                    state.set_ranges(ranges_data.ranges.clone());
                                    let n = ranges_data.ranges.len();
                                    log::info!("[DC] ranges: {} range(s), state updated", n);
                                    for (i, r) in ranges_data.ranges.iter().enumerate() {
                                        log::info!("[DC]   range[{}]: {} - {} (duration {} ms)", i, r.start_time, r.end_time, r.duration);
                                    }
                                } else {
                                    log::warn!("[DC] failed to parse ranges data: {:?}", data);
                                }
                            }
                            "archive_fragment" => {
                                if let Ok(frag) = serde_json::from_value::<ArchiveFragmentResponseData>(data.clone()) {
                                    state.set_playback_span(frag.start_time, frag.end_time);
                                    log::info!("[DC] archive_fragment: {} - {}", frag.start_time, frag.end_time);
                                    schedule_next_fragment(
                                        Arc::clone(&dc),
                                        Arc::clone(&session_id),
                                        frag.end_time,
                                    );
                                }
                            }
                            _ => {
                                log::info!("[DC] unhandled message type: {}", typ);
                            }
                        }
                    } else {
                        log::warn!("[DC] message missing type or data: typ={:?}", msg.typ);
                    }
                } else {
                    log::warn!("[DC] failed to parse as ServerMessage: {}", json_str.trim().chars().take(80).collect::<String>());
                }
            }
        }
    }
}

fn schedule_next_fragment(dc: Arc<RTCDataChannel>, session_id: Arc<AtomicU64>, next_start_ms: u64) {
    let delay_ms = FRAGMENT_DURATION_MS as u64 - REQUEST_NEXT_FRAGMENT_BEFORE_END_MS;
    let delay = Duration::from_millis(delay_ms);
    let my_session = session_id.load(Ordering::SeqCst);
    tokio::spawn(async move {
        tokio::time::sleep(delay).await;
        if session_id.load(Ordering::SeqCst) != my_session {
            return;
        }
        let req = get_archive_fragment(next_start_ms, FRAGMENT_DURATION_MS, false);
        if let Ok(json) = serde_json::to_string(&req) {
            if let Err(e) = dc.send_text(json).await {
                log::error!("get_archive_fragment (next) send error: {:?}", e);
            } else {
                log::info!("Requested next fragment from {} (no key)", next_start_ms);
            }
        }
    });
}
