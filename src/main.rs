mod app_state;
mod archive_loop;
mod archive_protocol;
mod config;
mod ui;
mod video_decoder;
mod webrtc_client;
mod webrtc_offer;

use anyhow::Result;
use app_state::{ArchiveState, VideoViewState};
use config::AppConfig;
use video_decoder::SharedFrame;
use gtk4::prelude::*;
use gtk4::Application;
use std::net::{IpAddr, ToSocketAddrs};
use std::sync::Arc;
use tokio::sync::mpsc;
use ui::MainWindow;
use webrtc::ice_transport::ice_candidate::RTCIceCandidateInit;

/// Пытается разименовать hostname (evs.eltex.loc) в IP-адрес, используя системный DNS.
fn resolve_host_to_ip(host: &str) -> Option<IpAddr> {
    // Порт не важен, берём 0; интересует только IP-часть.
    let addrs = (host, 0).to_socket_addrs().ok()?;
    for addr in addrs {
        return Some(addr.ip());
    }
    None
}

fn main() -> Result<()> {
    env_logger::init();

    let config = AppConfig::load().unwrap_or_default();
    let state = Arc::new(ArchiveState::default());
    let state_for_thread = Arc::clone(&state);
    let video_view = Arc::new(VideoViewState::new());
    let shared_frame: SharedFrame = Arc::new(std::sync::Mutex::new(None));
    let shared_frame_for_thread = Arc::clone(&shared_frame);
    // Ёмкость 1: декодер делает try_send и не блокируется на UI; лишние уведомления отбрасываются.
    let (frame_updated_tx, frame_updated_rx) = std::sync::mpsc::sync_channel(1);
    let frame_updated_rx = Arc::new(std::sync::Mutex::new(Some(frame_updated_rx)));
    let (cmd_tx, cmd_rx) = mpsc::channel(32);
    let cmd_tx_ui = cmd_tx.clone();

    // WebRTC offer/answer делаем в отдельном потоке, чтобы не блокировать GTK main loop.
    let webrtc_url = config.webrtc_url.clone();
    let tls_insecure = config.tls_insecure_skip_verify;
    let ice_servers = config.ice_servers.clone();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build();

        let Ok(rt) = rt else {
            log::error!("Failed to build tokio runtime for WebRTC thread");
            return;
        };

        rt.block_on(async move {
            let server_host_opt = webrtc_client::host_from_webrtc_url(&webrtc_url);
            let server_host = server_host_opt.as_deref();
            let built = match webrtc_offer::build_offer_h264_h265(server_host, &ice_servers, shared_frame_for_thread, frame_updated_tx, state_for_thread.clone()).await {
                Ok(built) => built,
                Err(err) => {
                    log::error!("Failed to build WebRTC offer: {:?}", err);
                    return;
                }
            };

            log::info!(
                "Built offer SDP (len={}). Expect to contain H264/H265 rtpmap lines.",
                built.offer_sdp.len()
            );
            for line in webrtc_client::extract_h26x_rtpmap_lines(&built.offer_sdp) {
                log::info!("Offer codec: {}", line);
            }

            log::info!(
                "[CLIENT -> SERVER] Отправляем offer SDP:\n---\n{}\n---",
                built.offer_sdp
            );

            let tls = webrtc_client::HttpTlsOptions {
                insecure_skip_verify: tls_insecure,
            };

            let answer = match webrtc_client::send_offer(&webrtc_url, &built.offer_sdp, tls).await {
                Ok(answer) => answer,
                Err(err) => {
                    log::error!("Failed to send WebRTC offer to {}: {:?}", webrtc_url, err);
                    return;
                }
            };

            log::info!(
                "Got WebRTC answer SDP (len={}): code={:?} id={:?} type={:?}",
                answer.sdp.len(),
                answer.code,
                answer.id,
                answer.r#type,
            );
            log::info!(
                "[SERVER -> CLIENT] Получен answer SDP:\n---\n{}\n---",
                answer.sdp
            );

            for line in answer
                .sdp
                .lines()
                .filter(|l| l.starts_with("a=candidate:"))
            {
                log::info!("Remote ICE candidate: {}", line);
            }

            // Отладка: сохраняем SDP ответа сервера.
            if let Err(e) = std::fs::write("answer_last.sdp", &answer.sdp) {
                log::warn!("Failed to write answer_last.sdp: {:?}", e);
            }

            // Разименовываем hostname в ICE-кандидатах (evs.eltex.loc -> IP), т.к. webrtc-rs ICE
            // ожидает IP-адреса в полях кандидатов.
            let raw_answer_sdp = answer.sdp;
            let mut answer_sdp = String::new();
            for line in raw_answer_sdp.lines() {
                let trimmed = line.trim_start();
                if let Some(rest) = trimmed.strip_prefix("a=candidate:") {
                    let parts: Vec<&str> = rest.split_whitespace().collect();
                    if parts.len() >= 6 {
                        let foundation = parts[0];
                        let component = parts[1];
                        let transport = parts[2];
                        let priority = parts[3];
                        let addr = parts[4];
                        let port = parts[5];
                        let tail = &parts[6..];

                        let ip = match addr.parse::<IpAddr>() {
                            Ok(ip) => {
                                log::info!(
                                    "Using IP ICE candidate from answer SDP: {}",
                                    line.trim()
                                );
                                Some(ip)
                            }
                            Err(_) => {
                                match resolve_host_to_ip(addr) {
                                    Some(ip) => {
                                        log::info!(
                                            "Resolved ICE candidate host {} -> {}",
                                            addr,
                                            ip
                                        );
                                        Some(ip)
                                    }
                                    None => {
                                        log::warn!(
                                            "Failed to resolve ICE candidate host {}, dropping candidate: {}",
                                            addr,
                                            line.trim()
                                        );
                                        None
                                    }
                                }
                            }
                        };

                        if let Some(ip) = ip {
                            let mut new_line = format!(
                                "a=candidate:{} {} {} {} {} {}",
                                foundation, component, transport, priority, ip, port
                            );
                            for t in tail {
                                new_line.push(' ');
                                new_line.push_str(t);
                            }
                            answer_sdp.push_str(&new_line);
                            answer_sdp.push('\n');
                        }
                        continue;
                    } else {
                        log::warn!(
                            "Cannot parse ICE candidate line in answer SDP, dropping: {}",
                            line.trim()
                        );
                        continue;
                    }
                }
                // Все остальные строки копируем как есть.
                answer_sdp.push_str(line);
                answer_sdp.push('\n');
            }

            let remote = match webrtc::peer_connection::sdp::session_description::RTCSessionDescription::answer(answer_sdp.clone()) {
                Ok(r) => r,
                Err(err) => {
                    log::error!("Failed to parse answer SDP: {:?}", err);
                    return;
                }
            };

            if let Err(err) = built.pc.set_remote_description(remote).await {
                log::error!("Failed to set remote description: {:?}", err);
            } else {
                log::info!("Remote description set. WebRTC handshake should proceed.");

                // Прямое подключение к кандидату из answer: берём host из URL, порт из a=candidate.
                if let Some(host) = webrtc_client::host_from_webrtc_url(&webrtc_url) {
                    let host_ip = match host.parse::<IpAddr>() {
                        Ok(ip) => Some(ip),
                        Err(_) => {
                            let resolved = resolve_host_to_ip(&host);
                            match resolved {
                                Some(ip) => {
                                    log::info!(
                                        "Resolved webrtc_url host {} -> {} for explicit ICE candidate",
                                        host,
                                        ip
                                    );
                                    Some(ip)
                                }
                                None => {
                                    log::warn!(
                                        "webrtc_url host is not an IP and cannot be resolved ({}), skipping explicit host ICE candidate",
                                        host
                                    );
                                    None
                                }
                            }
                        }
                    };
                    if let Some(line) = answer_sdp
                        .lines()
                        .find(|l| l.trim_start().starts_with("a=candidate:"))
                    {
                        let trimmed = line.trim();
                        if let Some(rest) = trimmed.strip_prefix("a=candidate:") {
                            let parts: Vec<&str> = rest.split_whitespace().collect();
                            if parts.len() >= 6 {
                                let Some(ip) = host_ip else {
                                    // Нет IP — пропускаем добавление ручного кандидата.
                                    return;
                                };
                                let foundation = parts[0];
                                let component = parts[1];
                                let transport = parts[2];
                                let priority = parts[3];
                                let port = parts[5];
                                let candidate_str = format!(
                                    "candidate:{} {} {} {} {} {} typ host",
                                    foundation, component, transport, priority, ip, port
                                );
                                log::info!(
                                    "Adding explicit remote ICE candidate from answer: {}",
                                    candidate_str
                                );
                                let init = RTCIceCandidateInit {
                                    candidate: candidate_str,
                                    sdp_mid: None,
                                    sdp_mline_index: Some(0),
                                    username_fragment: None,
                                };
                                if let Err(e) = built.pc.add_ice_candidate(init).await {
                                    log::error!(
                                        "Failed to add explicit remote ICE candidate: {:?}",
                                        e
                                    );
                                }
                            } else {
                                log::warn!(
                                    "Cannot parse candidate line for direct ICE candidate: {}",
                                    line
                                );
                            }
                        }
                    } else {
                        log::warn!(
                            "No a=candidate lines in answer SDP, explicit ICE candidate not added"
                        );
                    }
                } else {
                    log::warn!("Failed to extract host from webrtc_url for explicit ICE candidate");
                }
            }

            // Цикл архива: get_ranges при открытии DC, обработка команд PlayFrom и пополнение буфера.
            let message_rx = built.message_rx;
            tokio::spawn(archive_loop::run_archive_loop(
                built.data_channel,
                state_for_thread,
                cmd_rx,
                message_rx,
            ));

            // Держим runtime живым.
            let (_tx, rx) = tokio::sync::oneshot::channel::<()>();
            let _ = rx.await;
        });
    });

    let app = Application::new(
        Some("com.example.webrtc-archive-player"),
        gtk4::gio::ApplicationFlags::FLAGS_NONE,
    );

    app.connect_activate(move |app| {
        let rx = frame_updated_rx.lock().unwrap().take().unwrap();
        let win = MainWindow::new(
            app,
            config.clone(),
            Arc::clone(&state),
            Arc::clone(&video_view),
            cmd_tx_ui.clone(),
            Arc::clone(&shared_frame),
            rx,
        );
        win.present();
    });

    app.run();

    Ok(())
}

