mod app_state;
mod archive_loop;
mod archive_protocol;
mod config;
mod ui;
mod video_decoder;
mod webrtc_client;
mod webrtc_offer;

use anyhow::Result;
use app_state::ArchiveState;
use config::AppConfig;
use video_decoder::SharedFrame;
use gtk4::prelude::*;
use gtk4::Application;
use std::sync::Arc;
use tokio::sync::mpsc;
use ui::MainWindow;
use webrtc::ice_transport::ice_candidate::RTCIceCandidateInit;

fn main() -> Result<()> {
    env_logger::init();

    let config = AppConfig::load().unwrap_or_default();
    let state = Arc::new(ArchiveState::default());
    let state_for_thread = Arc::clone(&state);
    let shared_frame: SharedFrame = Arc::new(std::sync::Mutex::new(None));
    let shared_frame_for_thread = Arc::clone(&shared_frame);
    let (frame_updated_tx, frame_updated_rx) = std::sync::mpsc::sync_channel(0);
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
            let built = match webrtc_offer::build_offer_h264_h265(server_host, &ice_servers, shared_frame_for_thread, frame_updated_tx).await {
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

            let answer_sdp = answer.sdp;
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
                    if let Some(line) = answer_sdp
                        .lines()
                        .find(|l| l.trim_start().starts_with("a=candidate:"))
                    {
                        let trimmed = line.trim();
                        if let Some(rest) = trimmed.strip_prefix("a=candidate:") {
                            let parts: Vec<&str> = rest.split_whitespace().collect();
                            if parts.len() >= 6 {
                                let foundation = parts[0];
                                let component = parts[1];
                                let transport = parts[2];
                                let priority = parts[3];
                                let port = parts[5];
                                let candidate_str = format!(
                                    "candidate:{} {} {} {} {} {} typ host",
                                    foundation, component, transport, priority, host, port
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
            cmd_tx_ui.clone(),
            Arc::clone(&shared_frame),
            rx,
        );
        win.present();
    });

    app.run();

    Ok(())
}

