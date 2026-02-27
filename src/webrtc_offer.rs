use anyhow::{anyhow, Result};
use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;
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

pub struct BuiltOffer {
    pub pc: Arc<webrtc::peer_connection::RTCPeerConnection>,
    pub offer_sdp: String,
}

/// Строит offer под SFU: один локальный кандидат (IP сервера), без задержки host-пары — быстрый выход из Checking в Connected и DTLS.
pub async fn build_offer_h264_h265(
    server_host: Option<&str>,
    ice_servers: &[String],
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

    // Отладка: треки и datachannel.
    pc.on_track(Box::new(|track, _receiver, _transceiver| {
        log::info!(
            "Remote track arrived: kind={:?}, codec={:?}, ssrc={}",
            track.kind(),
            track.codec(),
            track.ssrc()
        );
        Box::pin(async {})
    }));

    pc.on_data_channel(Box::new(|dc| {
        log::info!(
            "DataChannel from remote: label={} id={:?}",
            dc.label(),
            dc.id()
        );
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

    // Канал данных нужен для команд (get_ranges / play_stream / ...).
    let _dc = pc.create_data_channel("data", None).await?;

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
    })
}

