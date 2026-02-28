use anyhow::{anyhow, Result};
use std::net::IpAddr;
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use tokio::sync::mpsc;
use webrtc::api::interceptor_registry::register_default_interceptors;
use webrtc::api::media_engine::{MediaEngine, MIME_TYPE_H264, MIME_TYPE_HEVC};
use webrtc::api::setting_engine::SettingEngine;
use webrtc::api::APIBuilder;
use webrtc::dtls_transport::dtls_role::DTLSRole;
use webrtc::ice::agent::agent_config::IpFilterFn;
use webrtc::ice_transport::ice_connection_state::RTCIceConnectionState;
use webrtc::ice_transport::ice_server::RTCIceServer;
use webrtc::ice::mdns::MulticastDnsMode;
use webrtc::ice::network_type::NetworkType;
use webrtc::interceptor::registry::Registry;
use webrtc::peer_connection::configuration::RTCConfiguration;
use webrtc::peer_connection::sdp::session_description::RTCSessionDescription;
use webrtc::rtp_transceiver::rtp_codec::{RTCRtpCodecCapability, RTCRtpCodecParameters, RTPCodecType};
use webrtc::rtp_transceiver::{RTCPFeedback, RTCRtpTransceiverInit};
use webrtc::rtp_transceiver::rtp_transceiver_direction::RTCRtpTransceiverDirection;

use crate::app_state::ArchiveState;
use crate::video_decoder::{SharedFrame, VideoDecoder};

pub struct BuiltOffer {
    pub pc: Arc<webrtc::peer_connection::RTCPeerConnection>,
    pub offer_sdp: String,
    /// Data Channel "data" для команд архива (get_ranges, play_stream, get_archive_fragment и т.д.).
    pub data_channel: Arc<webrtc::data_channel::RTCDataChannel>,
    /// Приёмник сообщений с DC — обработчик вешается при создании канала, чтобы не пропустить ответы.
    pub message_rx: mpsc::Receiver<String>,
}

/// Частота дискретизации RTP timestamp для H.264/HEVC (90 kHz).
const RTP_CLOCK_RATE_HZ: u64 = 90_000;

/// Разворачивает 32-битный RTP timestamp с учётом переполнения (обнуления в середине потока).
#[inline]
fn unwrap_rtp_timestamp(raw_ts: u32, prev_raw: u32, prev_unwrapped: i64) -> i64 {
    let diff = raw_ts as i64 - prev_raw as i64;
    let delta = if diff >= 0 && diff < 0x8000_0000 {
        diff
    } else if diff < 0 && diff >= -0x8000_0000 {
        diff
    } else if diff < 0 {
        diff + 0x1_0000_0000
    } else {
        diff - 0x1_0000_0000
    };
    prev_unwrapped + delta
}

/// Строит offer под SFU: один локальный кандидат (IP сервера), без задержки host-пары — быстрый выход из Checking в Connected и DTLS.
/// `shared_frame`: буфер последнего декодированного кадра для отрисовки в UI.
/// `frame_updated`: при каждом новом кадре отправляется () для немедленной перерисовки (без привязки ко времени).
/// `state`: для обновления playback_position_ms из RTP timestamp (движение ползунка на timeline).
pub async fn build_offer_h264_h265(
    server_host: Option<&str>,
    ice_servers: &[String],
    shared_frame: SharedFrame,
    frame_updated: std::sync::mpsc::SyncSender<()>,
    state: Arc<ArchiveState>,
) -> Result<BuiltOffer> {
    let mut m = MediaEngine::default();

    // Регистрируем только H.264 и H.265, чтобы они попали в offer SDP.
    m.register_codec(
        RTCRtpCodecParameters {
            capability: RTCRtpCodecCapability {
                mime_type: MIME_TYPE_H264.to_string(),
                clock_rate: 90_000,
                channels: 0,
                sdp_fmtp_line:
                    "level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42e01f"
                        .to_string(),
                rtcp_feedback: vec![
                    RTCPFeedback {
                        typ: "goog-remb".to_string(),
                        parameter: "".to_string(),
                    },
                    RTCPFeedback {
                        typ: "ccm".to_string(),
                        parameter: "fir".to_string(),
                    },
                    RTCPFeedback {
                        typ: "nack".to_string(),
                        parameter: "".to_string(),
                    },
                    RTCPFeedback {
                        typ: "nack".to_string(),
                        parameter: "pli".to_string(),
                    },
                    RTCPFeedback {
                        typ: "transport-cc".to_string(),
                        parameter: "".to_string(),
                    },
                ],
            },
            payload_type: 102,
            ..Default::default()
        },
        RTPCodecType::Video,
    )?;

    m.register_codec(
        RTCRtpCodecParameters {
            capability: RTCRtpCodecCapability {
                mime_type: MIME_TYPE_HEVC.to_string(),
                clock_rate: 90_000,
                channels: 0,
                // Часто хватает просто mime_type + clock_rate; fmtp зависит от сервера.
                sdp_fmtp_line: "".to_string(),
                rtcp_feedback: vec![
                    RTCPFeedback {
                        typ: "goog-remb".to_string(),
                        parameter: "".to_string(),
                    },
                    RTCPFeedback {
                        typ: "ccm".to_string(),
                        parameter: "fir".to_string(),
                    },
                    RTCPFeedback {
                        typ: "nack".to_string(),
                        parameter: "".to_string(),
                    },
                    RTCPFeedback {
                        typ: "nack".to_string(),
                        parameter: "pli".to_string(),
                    },
                    RTCPFeedback {
                        typ: "transport-cc".to_string(),
                        parameter: "".to_string(),
                    },
                ],
            },
            payload_type: 103,
            ..Default::default()
        },
        RTPCodecType::Video,
    )?;

    let mut registry = Registry::new();
    registry = register_default_interceptors(registry, &mut m)?;

    // SFU: только UDP4, без mDNS, не ждём перед принятием host-пары — выходим из Checking и переходим к DTLS.
    let mut se = SettingEngine::default();
    se.set_network_types(vec![NetworkType::Udp4]);
    se.set_ice_multicast_dns_mode(MulticastDnsMode::Disabled);
    se.set_host_acceptance_min_wait(Some(Duration::from_millis(0)));
    // Клиент всегда инициирует DTLS (отправляет ClientHello), не ждёт инициативы от сервера — как в WHEP/плеере.
    se.set_answering_dtls_role(DTLSRole::Client)?;
    // Чтобы ICE не выбирал docker-интерфейсы (172.x) и ходил прямо к SFU по тому же IP,
    // фильтруем локальные кандидаты по IP сервера, если он известен.
    if let Some(host) = server_host {
        if let Ok(server_ip) = host.parse::<IpAddr>() {
            let filter: IpFilterFn = Box::new(move |ip| ip == server_ip);
            se.set_ip_filter(filter);
            log::info!(
                "ICE: restricting local candidates to IP {} (match SFU), избегаем 172.x docker-интерфейсов",
                server_ip
            );
        }
    }

    let api = APIBuilder::new()
        .with_setting_engine(se)
        .with_media_engine(m)
        .with_interceptor_registry(registry)
        .build();

    // Пробрасываем STUN/TURN в конфигурацию PeerConnection.
    let ice_servers_cfg: Vec<RTCIceServer> = ice_servers
        .iter()
        .map(|url| RTCIceServer {
            urls: vec![url.clone()],
            ..Default::default()
        })
        .collect();

    if ice_servers_cfg.is_empty() {
        log::info!("ICE: no STUN/TURN servers configured (direct SFU connection only)");
    } else {
        for s in &ice_servers_cfg {
            for u in &s.urls {
                log::info!("ICE: using STUN/TURN server URL {}", u);
            }
        }
    }

    let pc = Arc::new(
        api.new_peer_connection(RTCConfiguration {
            ice_servers: ice_servers_cfg,
            ..Default::default()
        })
        .await?,
    );

    // Отладка: состояние PeerConnection (общее).
    {
        let pc_clone = Arc::clone(&pc);
        pc.on_peer_connection_state_change(Box::new(move |state| {
            log::info!("PeerConnection state => {:?}", state);
            let pc2 = Arc::clone(&pc_clone);
            Box::pin(async move {
                log::info!(
                    "Current signaling={:?}, ice_connection={:?}, ice_gathering={:?}",
                    pc2.signaling_state(),
                    pc2.ice_connection_state(),
                    pc2.ice_gathering_state()
                );
            })
        }));
    }

    // Отладка: состояние ICE-gathering.
    pc.on_ice_gathering_state_change(Box::new(|state| {
        log::info!("ICE gathering state => {:?}", state);
        Box::pin(async {})
    }));

    // Подробные логи при Checking: что происходит, выбранная пара, DTLS.
    {
        let pc_clone = Arc::clone(&pc);
        pc.on_ice_connection_state_change(Box::new(move |state| {
            let is_checking = state == RTCIceConnectionState::Checking;
            if is_checking {
                log::warn!(
                    "[ICE CHECKING] ICE перешёл в состояние Checking: агент отправляет STUN binding request \
                     по всем парам (local, remote). Пока ни одна пара не ответила — выбранной пары нет. \
                     Если зависаем здесь: нет доступа по UDP до сервера или нет общих кандидатов."
                );
            } else {
                log::info!("ICE connection state => {:?}", state);
            }
            let pc2 = Arc::clone(&pc_clone);
            Box::pin(async move {
                let dtls = pc2.dtls_transport();
                let ice = dtls.ice_transport();
                let dtls_state = dtls.state();
                let selected = ice.get_selected_candidate_pair().await;
                if is_checking {
                    log::warn!(
                        "[ICE CHECKING] selected_pair={} dtls_state={:?} (ожидаем: pair=None, DTLS=New до успеха ICE)",
                        if selected.is_some() { "Some" } else { "None" },
                        dtls_state
                    );
                    if let Some(ref p) = selected {
                        log::info!("Selected candidate pair: local={:?} remote={:?}", p.local, p.remote);
                    }
                } else if let Some(ref p) = selected {
                    log::info!(
                        "Selected candidate pair: local={:?} remote={:?}",
                        p.local,
                        p.remote
                    );
                } else {
                    log::info!("No selected candidate pair yet");
                }
            })
        }));
    }

    // Как только выбрана пара — выходим из Checking, логируем.
    pc.dtls_transport()
        .ice_transport()
        .on_selected_candidate_pair_change(Box::new(|pair| {
            log::info!(
                "[ICE] Выбрана пара кандидатов: local={} remote={} (Checking -> Connected)",
                pair.local,
                pair.remote
            );
            Box::pin(async {})
        }));

    pc.on_ice_candidate(Box::new(|cand| {
        if let Some(c) = cand {
            log::info!("Local ICE candidate: {}", c);
        } else {
            log::info!("ICE gathering completed (no more local candidates)");
        }
        Box::pin(async {})
    }));

    // Параллельный конвейер: поток чтения RTP и поток декодирования работают независимо.
    let frame_for_track = shared_frame.clone();
    let frame_updated_tx = frame_updated;
    pc.on_track(Box::new(move |track, _receiver, _transceiver| {
        log::info!(
            "[video] Remote track arrived: kind={:?}, codec={:?}, ssrc={}",
            track.kind(),
            track.codec(),
            track.ssrc()
        );
        if track.kind() != RTPCodecType::Video {
            return Box::pin(async {});
        }
        let frame_buf = frame_for_track.clone();
        let notify_new_frame = frame_updated_tx.clone();
        let track = Arc::clone(&track);

        // Буфер 8 пакетов: при кратковременных задержках декодера читатель не блокируется сразу — меньше «подвисаний».
        let (payload_tx, payload_rx) = std::sync::mpsc::sync_channel::<Vec<u8>>(8);

        // Поток 1: чтение RTP, разворот timestamp (32-bit wrap), обновление playback_position_ms, отправка payload в канал.
        let track_reader = Arc::clone(&track);
        let payload_tx_reader = payload_tx.clone();
        let state_rtp = Arc::clone(&state);
        std::thread::spawn(move || {
            let rt = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(r) => r,
                Err(e) => {
                    log::error!("[video] Failed to create runtime for reader: {:?}", e);
                    return;
                }
            };
            log::info!("[video] RTP reader loop started for track ssrc={}", track_reader.ssrc());
            let mut rtp_packets: u64 = 0;
            let mut prev_raw_ts: u32 = 0;
            let mut unwrapped_rtp: i64 = 0;
            let mut ref_real_ms: u64 = 0;
            let mut ref_unwrapped_rtp: i64 = 0;
            let mut last_generation: u64 = 0;
            rt.block_on(async {
                loop {
                    let (pkt, _attrs) = match track_reader.read_rtp().await {
                        Ok(x) => x,
                        Err(_) => break,
                    };
                    rtp_packets += 1;
                    let raw_ts = pkt.header.timestamp;
                    let current_start = state_rtp.playback_start_ms.load(std::sync::atomic::Ordering::Relaxed);
                    let current_gen = state_rtp.playback_generation();

                    if current_gen != last_generation {
                        last_generation = current_gen;
                        ref_real_ms = current_start;
                        ref_unwrapped_rtp = 0;
                        prev_raw_ts = raw_ts;
                        unwrapped_rtp = 0;
                    } else {
                        unwrapped_rtp = unwrap_rtp_timestamp(raw_ts, prev_raw_ts, unwrapped_rtp);
                        prev_raw_ts = raw_ts;
                    }

                    let delta_ticks = unwrapped_rtp - ref_unwrapped_rtp;
                    let delta_ms = (delta_ticks as i64 * 1000) / RTP_CLOCK_RATE_HZ as i64;
                    let position_ms = if delta_ms >= 0 {
                        ref_real_ms.saturating_add(delta_ms as u64)
                    } else {
                        ref_real_ms.saturating_sub((-delta_ms) as u64)
                    };
                    let end_ms = state_rtp.playback_end_ms.load(std::sync::atomic::Ordering::Relaxed);
                    if current_gen != 0
                        && position_ms >= ref_real_ms
                        && position_ms <= end_ms
                    {
                        state_rtp.set_playback_position(position_ms);
                    }

                    if rtp_packets <= 3 || rtp_packets % 300 == 0 {
                        log::info!("[video] RTP reader: packet #{} ({} bytes) -> channel", rtp_packets, pkt.payload.len());
                    }
                    if payload_tx_reader.send(pkt.payload.to_vec()).is_err() {
                        log::info!("[video] RTP reader loop ended after {} packets (channel closed)", rtp_packets);
                        break;
                    }
                }
            });
            log::info!("[video] RTP reader loop ended, total packets received={}", rtp_packets);
        });

        // Поток 2: приём payload из канала, декодирование, запись кадра в shared_frame.
        std::thread::spawn(move || {
            let mut decoder = match VideoDecoder::new() {
                Ok(d) => d,
                Err(e) => {
                    log::error!("[video] Failed to create decoder: {:?}", e);
                    return;
                }
            };
            log::info!("[video] Decode loop started for track ssrc={}", track.ssrc());
            let mut frame_count: u64 = 0;
            let mut frames_in_this_sec: u64 = 0;
            let mut last_sec = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            while let Ok(payload) = payload_rx.recv() {
                if let Ok(Some(decoded)) = decoder.push_payload(&payload) {
                    frame_count += 1;
                    frames_in_this_sec += 1;
                    let w = decoded.width;
                    let h = decoded.height;
                    log::debug!(
                        "[video] frame decoded #{} {}x{} (written to SharedFrame)",
                        frame_count, w, h
                    );
                    if frame_count <= 5 || frame_count % 30 == 0 {
                        log::info!(
                            "[video] decoded frame #{} {}x{} (total decoded so far)",
                            frame_count, w, h
                        );
                    }
                    let now_sec = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs();
                    if now_sec > last_sec {
                        log::info!(
                            "[video] decode rate: {} frames in last second (total #{})",
                            frames_in_this_sec, frame_count
                        );
                        last_sec = now_sec;
                        frames_in_this_sec = 0;
                    }
                    if let Ok(mut guard) = frame_buf.lock() {
                        *guard = Some(decoded);
                        drop(guard);
                        // try_send: не блокируем декодер на ожидании UI; при переполнении канала просто пропускаем уведомление.
                        if notify_new_frame.try_send(()).is_err() {
                            log::debug!("[video] frame #{}: frame_updated channel full, skip notify", frame_count);
                        }
                    } else {
                        log::warn!("[video] frame #{}: failed to lock SharedFrame", frame_count);
                    }
                }
            }
            log::info!("[video] Decode loop ended for track ssrc={}, total frames decoded={}", track.ssrc(), frame_count);
        });
        Box::pin(async {})
    }));

    // Сообщения из DC в архив-луп: синхронная отправка в callback (не зависят от опроса Future),
    // поток-мост пересылает из std::mpsc в tokio::mpsc.
    let (sync_tx, sync_rx) = std::sync::mpsc::sync_channel::<String>(64);
    let (tokio_tx, message_rx) = mpsc::channel(64);
    let sync_tx_remote = sync_tx.clone();
    let tokio_tx_for_bridge = tokio_tx.clone();

    thread::spawn(move || {
        while let Ok(s) = sync_rx.recv() {
            if !s.contains("\"type\":\"meta\"") {
                log::info!("[DC] bridge: forwarding to archive_loop ({} bytes)", s.len());
            }
            if let Err(e) = tokio_tx_for_bridge.blocking_send(s) {
                log::warn!("[DC] bridge: send to loop failed: {:?}", e);
                break;
            }
        }
        log::info!("[DC] bridge thread exiting");
    });

    // Сервер может открыть свой Data Channel (в answer) и слать ответы по нему — подписываемся.
    pc.on_data_channel(Box::new(move |dc| {
        log::info!(
            "[DC] DataChannel from REMOTE: label={} id={:?}, registering on_message",
            dc.label(),
            dc.id()
        );
        let tx = sync_tx_remote.clone();
        dc.on_message(Box::new(move |msg| {
            let tx = tx.clone();
            if msg.is_string {
                if let Ok(s) = String::from_utf8(msg.data.to_vec()) {
                    if !s.contains("\"type\":\"meta\"") {
                        log::info!("[DC] message on REMOTE channel ({} bytes): {}", s.len(), s.trim().chars().take(120).collect::<String>());
                    }
                    if let Err(e) = tx.send(s) {
                        log::warn!("[DC] REMOTE channel send to bridge: {:?}", e);
                    }
                }
            } else {
                log::info!("[DC] binary message on REMOTE channel ({} bytes)", msg.data.len());
            }
            Box::pin(async {})
        }));
        Box::pin(async {})
    }));

    // Сервер sendonly, клиент recvonly.
    pc.add_transceiver_from_kind(
        RTPCodecType::Video,
        Some(RTCRtpTransceiverInit {
            direction: RTCRtpTransceiverDirection::Recvonly,
            send_encodings: vec![],
        }),
    )
    .await?;

    // Канал данных: on_message ставим СРАЗУ при создании, иначе в webrtc handle_open сначала
    // спавнит do_open() (наш on_open в отдельной задаче), потом сразу спавнит read_loop — данные
    // могут прийти и вызваться handler до того, как on_open успеет установить on_message.
    let data_channel = pc.create_data_channel("data", None).await?;
    log::info!("[DC] our DataChannel created: label={}", data_channel.label());
    {
        let sync_tx_our = sync_tx.clone();
        data_channel.on_message(Box::new(move |msg| {
            log::info!("on_message");
            let tx = sync_tx_our.clone();
            if msg.is_string {
                if let Ok(s) = String::from_utf8(msg.data.to_vec()) {
                    log::info!("[DC] message on OUR channel ({} bytes): {}", s.len(), s.trim().chars().take(120).collect::<String>());
                    if let Err(e) = tx.send(s) {
                        log::warn!("[DC] OUR channel send to bridge: {:?}", e);
                    }
                }
            } else {
                log::info!("[DC] binary message on OUR channel ({} bytes)", msg.data.len());
            }
            Box::pin(async {})
        }));
    }
    {
        let dc_send = data_channel.clone();
        data_channel.on_open(Box::new(move || {
            log::info!("[DC] our channel OPEN, sending get_ranges");
            tokio::spawn(async move {
                let req = crate::archive_protocol::get_ranges(None, None);
                if let Ok(json) = serde_json::to_string(&req) {
                    if let Err(e) = dc_send.send_text(json).await {
                        log::error!("[DC] get_ranges send failed: {:?}", e);
                    }
                }
            });
            Box::pin(async {})
        }));
    }

    let offer: RTCSessionDescription = pc.create_offer(None).await?;

    // Как в mediakit WHEP: offer должен содержать локальные кандидаты, чтобы после
    // set_remote_description(answer) ICE сразу мог формировать пары и делать connectivity check.
    // Ждём завершения gathering до отправки offer (как в тестах webrtc-rs).
    let mut gathering_complete = pc.gathering_complete_promise().await;
    pc.set_local_description(offer).await?;
    let _ = gathering_complete.recv().await;
    log::info!("ICE gathering complete, offer SDP now includes local candidates");

    let local = pc
        .local_description()
        .await
        .ok_or_else(|| anyhow!("local_description is None after set_local_description"))?;

    if let Err(e) = std::fs::write("offer_last.sdp", &local.sdp) {
        log::warn!("Failed to write offer_last.sdp: {:?}", e);
    }

    // Клиент везде active: в offer объявляем a=setup:active (мы — DTLS client, инициируем рукопожатие).
    let offer_sdp = if local.sdp.contains("setup:actpass") {
        log::info!("Offer: forcing a=setup:active (client is DTLS client everywhere)");
        local.sdp.replace("setup:actpass", "setup:active")
    } else {
        local.sdp
    };

    Ok(BuiltOffer {
        pc,
        offer_sdp,
        data_channel,
        message_rx,
    })
}

