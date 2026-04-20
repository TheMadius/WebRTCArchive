//! Цикл управления архивом: приём/отправка по Data Channel, пополнение буфера за 2 сек до конца.

use crate::app_state::{ArchiveCommand, ArchiveState, MetaObject};
use crate::archive_protocol::{
    get_archive_fragment, get_ranges, drop_buffer, play_stream, stop_stream, set_speed,
    ArchiveFragmentResponseData, ServerMessage, RangesResponseData,
};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use webrtc::data_channel::RTCDataChannel;

// Длительность одного фрагмента архива (10 секунд).
const FRAGMENT_DURATION_MS: i64 = 10_000;
/// Запрашивать следующий фрагмент за столько миллисекунд контента до конца текущего.
const REQUEST_NEXT_FRAGMENT_BEFORE_END_MS: u64 = 2_000;

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
                        state.set_playback_position(timestamp_ms);
                        state.set_playback_wall_start(timestamp_ms);
                        state.pending_fragment_start_ms.store(0, Ordering::Relaxed);
                        state.pending_fragment_end_ms.store(0, Ordering::Relaxed);
                        state.last_play_from_requested_ms.store(timestamp_ms, Ordering::Relaxed);
                        state.next_playback_generation();
                        if let Err(e) = dc.send_text(serde_json::to_string(&play_stream()).unwrap()).await {
                            log::error!("play_stream send error: {:?}", e);
                        }
                        let speed_f = state.playback_speed() as f64;
                        if let Ok(json) = serde_json::to_string(&set_speed(speed_f)) {
                            let _ = dc.send_text(json).await;
                        }
                        log::info!("PlayFrom {} (with key frame), play_stream + set_speed {}x sent", timestamp_ms, state.playback_speed());
                        // Следующий фрагмент планируем только по ответу archive_fragment, чтобы не дублировать запрос.
                    }
                    ArchiveCommand::Stop => {
                        let req = stop_stream();
                        if let Ok(json) = serde_json::to_string(&req) {
                            let _ = dc.send_text(json).await;
                        }
                        let current_pos = state.current_playback_position_ms();
                        state.clear_playback_wall_start();
                        state.set_playback_position(current_pos);
                    }
                    ArchiveCommand::SeekTo { timestamp_ms } => {
                        session_id.fetch_add(1, Ordering::SeqCst);
                        if let Err(e) = dc.send_text(serde_json::to_string(&drop_buffer()).unwrap()).await {
                            log::error!("drop_buffer (SeekTo) send error: {:?}", e);
                        }
                        let req = get_archive_fragment(timestamp_ms, FRAGMENT_DURATION_MS, true);
                        if let Ok(json) = serde_json::to_string(&req) {
                            if let Err(e) = dc.send_text(json).await {
                                log::error!("get_archive_fragment (SeekTo) send error: {:?}", e);
                            }
                        }
                        state.set_playback_span(timestamp_ms, timestamp_ms + FRAGMENT_DURATION_MS as u64);
                        state.set_playback_position(timestamp_ms);
                        state.clear_playback_wall_start();
                        state.pending_fragment_start_ms.store(0, Ordering::Relaxed);
                        state.pending_fragment_end_ms.store(0, Ordering::Relaxed);
                        state.last_play_from_requested_ms.store(timestamp_ms, Ordering::Relaxed);
                        state.next_playback_generation();
                        log::info!("SeekTo {} (no play)", timestamp_ms);
                    }
                    ArchiveCommand::Pause => {
                        let req = stop_stream();
                        if let Ok(json) = serde_json::to_string(&req) {
                            if let Err(e) = dc.send_text(json).await {
                                log::error!("stop_stream (pause) send error: {:?}", e);
                            }
                        }
                        state.set_playback_paused();
                        log::info!("Pause: stop_stream sent; timeline always uses RTP position (last received)");
                    }
                    ArchiveCommand::Play => {
                        let req = play_stream();
                        if let Ok(json) = serde_json::to_string(&req) {
                            if let Err(e) = dc.send_text(json).await {
                                log::error!("play_stream (resume) send error: {:?}", e);
                            }
                        }
                        state.set_playback_resumed();
                        log::info!("Play: play_stream sent");
                    }
                    ArchiveCommand::SetSpeed { speed } => {
                        state.set_playback_speed(speed);
                        let req = set_speed(speed as f64);
                        if let Ok(json) = serde_json::to_string(&req) {
                            if let Err(e) = dc.send_text(json).await {
                                log::error!("set_speed send error: {:?}", e);
                            } else {
                                log::info!("SetSpeed: {}x sent", speed);
                            }
                        }
                    }
                }
            }
            Some(json_str) = msg_rx.recv() => {
                // Мета приходит часто: парсим только нужную часть (objects.points) и обновляем state.
                if json_str.contains("\"type\":\"meta\"") {
                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&json_str) {
                        let mut objects: Vec<MetaObject> = Vec::new();
                        let arr_opt = v
                            .get("data")
                            .and_then(|d| d.get("motionModel"))
                            .and_then(|m| m.get("objects"))
                            .and_then(|o| o.as_array());
                        if let Some(arr) = arr_opt {
                            for item in arr {
                                let id = item
                                    .get("id")
                                    .and_then(|x| x.as_str())
                                    .unwrap_or_default()
                                    .to_string();
                                let kind = item
                                    .get("type")
                                    .and_then(|x| x.as_str())
                                    .unwrap_or("OBJ")
                                    .to_string();
                                let points = item.get("points");
                                let x = points
                                    .and_then(|p| p.get("x"))
                                    .and_then(|v| v.as_f64())
                                    .unwrap_or(0.0);
                                let y = points
                                    .and_then(|p| p.get("y"))
                                    .and_then(|v| v.as_f64())
                                    .unwrap_or(0.0);
                                let w = points
                                    .and_then(|p| p.get("w"))
                                    .and_then(|v| v.as_f64())
                                    .unwrap_or(0.0);
                                let h = points
                                    .and_then(|p| p.get("h"))
                                    .and_then(|v| v.as_f64())
                                    .unwrap_or(0.0);
                                objects.push(MetaObject { id, kind, x, y, w, h });
                            }
                        }
                        state.set_meta_objects(objects);
                    }
                    continue;
                }
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
                                    let requested = state.last_play_from_requested_ms.load(Ordering::Relaxed);
                                    let margin_ms = 5_000u64;
                                    let is_for_current_request = requested == 0
                                        || (frag.start_time <= requested + margin_ms && frag.end_time >= requested.saturating_sub(margin_ms));
                                    if is_for_current_request {
                                        let current_pos = state.current_playback_position_ms();
                                        let start = state.playback_start_ms.load(Ordering::Relaxed);
                                        let end = state.playback_end_ms.load(Ordering::Relaxed);
                                        let span_unset = start == 0 && end == 0;
                                        let fragment_contains_position =
                                            current_pos >= frag.start_time && current_pos <= frag.end_time;
                                        if span_unset || fragment_contains_position {
                                            state.set_playback_span(frag.start_time, frag.end_time);
                                            state.pending_fragment_start_ms.store(0, Ordering::Relaxed);
                                            state.pending_fragment_end_ms.store(0, Ordering::Relaxed);
                                            log::info!("[DC] archive_fragment: {} - {} (applied)", frag.start_time, frag.end_time);
                                        } else {
                                            state.pending_fragment_start_ms.store(frag.start_time, Ordering::Relaxed);
                                            state.pending_fragment_end_ms.store(frag.end_time, Ordering::Relaxed);
                                            log::info!(
                                                "[DC] archive_fragment: {} - {} (pending, pos={})",
                                                frag.start_time,
                                                frag.end_time,
                                                current_pos
                                            );
                                        }
                                        schedule_next_fragment(
                                            Arc::clone(&dc),
                                            Arc::clone(&state),
                                            Arc::clone(&session_id),
                                            frag.end_time,
                                        );
                                    } else {
                                        log::info!(
                                            "[DC] archive_fragment: {} - {} (ignored, requested was {})",
                                            frag.start_time,
                                            frag.end_time,
                                            requested
                                        );
                                    }
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

fn schedule_next_fragment(
    dc: Arc<RTCDataChannel>,
    state: Arc<ArchiveState>,
    session_id: Arc<AtomicU64>,
    next_start_ms: u64,
) {
    let content_delay_ms = FRAGMENT_DURATION_MS as u64 - REQUEST_NEXT_FRAGMENT_BEFORE_END_MS;
    let speed = state.playback_speed().max(1) as u64;
    let delay_ms = content_delay_ms / speed;
    let delay = Duration::from_millis(delay_ms);
    let my_session = session_id.load(Ordering::SeqCst);
    log::debug!("schedule_next_fragment: content_delay={}ms speed={} -> wall_delay={}ms", content_delay_ms, speed, delay_ms);
    tokio::spawn(async move {
        tokio::time::sleep(delay).await;
        if session_id.load(Ordering::SeqCst) != my_session {
            log::debug!("schedule_next_fragment cancelled: session changed");
            return;
        }
        let req = get_archive_fragment(next_start_ms, FRAGMENT_DURATION_MS, false);
        if let Ok(json) = serde_json::to_string(&req) {
            if let Err(e) = dc.send_text(json).await {
                log::error!("get_archive_fragment (next) send error: {:?}", e);
            } else {
                // Важно: обновляем ожидаемый timestamp последнего запроса, иначе последующие archive_fragment
                // будут считаться "не для текущего PlayFrom" и цикл пополнения оборвётся.
                state
                    .last_play_from_requested_ms
                    .store(next_start_ms, Ordering::Relaxed);
                log::info!("Requested next fragment from {} (no key)", next_start_ms);
            }
        }
    });
}
