//! Декодирование RTP H.264/H.265 в сырые кадры (RGB24) для вывода в UI.

use anyhow::{Context as AnyhowContext, Result};
use ffmpeg_next::codec::decoder::video::Video as FfmpegVideoDecoder;
use ffmpeg_next::codec::decoder::{self as ffmpeg_decoder};
use ffmpeg_next::codec::{self, Flags as CodecFlags, Id};
use ffmpeg_next::software::scaling::{Context as ScaleContext, Flags as ScaleFlags};
use ffmpeg_next::util::frame::video::Video as FfmpegVideoFrame;
use ffmpeg_next::util::format::Pixel;
use std::sync::Arc;
use bytes::Bytes;
use webrtc::rtp::codecs::h264::H264Packet;
use webrtc::rtp::packetizer::Depacketizer;

/// Один декодированный кадр в формате RGB24 для отрисовки.
/// Время (pts/duration) не используется — на экране всегда последний пришедший кадр.
#[derive(Clone)]
pub struct DecodedFrame {
    pub width: u32,
    pub height: u32,
    /// RGB24, stride = width * 3
    pub data: Vec<u8>,
}

/// Тип видеокодека потока.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VideoCodecKind {
    H264,
    H265,
}

/// Декодер RTP H.264/H.265 -> RGB24 кадры.
pub struct VideoDecoder {
    kind: VideoCodecKind,
    h264_depacketizer: Option<H264Packet>,
    decoder: FfmpegVideoDecoder,
    scaler: Option<ScaleContext>,
    rgb_frame: FfmpegVideoFrame,
    /// Буфер для сборки фрагментированного HEVC NAL (FU; nal_type=49).
    hevc_fu_buffer: Option<Vec<u8>>,
    debug_packets_logged: u64,
    debug_frames_logged: u64,
}

impl VideoDecoder {
    pub fn new(kind: VideoCodecKind) -> Result<Self> {
        ffmpeg_next::init().context("ffmpeg init")?;

        let codec_id = match kind {
            VideoCodecKind::H264 => Id::H264,
            VideoCodecKind::H265 => Id::HEVC,
        };

        let codec = ffmpeg_decoder::find(codec_id)
            .ok_or_else(|| anyhow::anyhow!(format!("{:?} codec not found", kind)))?;
        let mut decoder_ctx = ffmpeg_decoder::Decoder(codec::Context::new());
        // Для онлайнового просмотра архива держим минимальную задержку и разрешаем вывод даже частично
        // повреждённых кадров — это лучше «серого экрана».
        decoder_ctx.set_flags(CodecFlags::LOW_DELAY | CodecFlags::OUTPUT_CORRUPT);
        // Максимально загружаем ядра декодером, чтобы не отставать от потока и не давать видео «ползти».
        let threads = std::cmp::max(1, num_cpus::get());
        decoder_ctx.set_threading(codec::threading::Config {
            kind: codec::threading::Type::Frame,
            count: threads,
            ..codec::threading::Config::default()
        });
        let decoder = decoder_ctx
            .open_as(codec)
            .context("open video decoder")?
            .video()
            .context("decoder as video")?;

        log::info!(
            "[video] ffmpeg decoder created: kind={:?} codec_id={:?}, threads={}",
            kind,
            codec_id,
            threads
        );

        Ok(Self {
            kind,
            h264_depacketizer: match kind {
                VideoCodecKind::H264 => Some(H264Packet::default()),
                VideoCodecKind::H265 => None,
            },
            decoder,
            scaler: None,
            rgb_frame: FfmpegVideoFrame::empty(),
            hevc_fu_buffer: None,
            debug_packets_logged: 0,
            debug_frames_logged: 0,
        })
    }

    /// Подаёт payload RTP (для конвейера: чтение и декодирование в разных потоках).
    pub fn push_payload(&mut self, payload: &[u8]) -> Result<Option<DecodedFrame>> {
        if payload.is_empty() {
            return Ok(None);
        }
        self.debug_packets_logged = self.debug_packets_logged.saturating_add(1);

        // В зависимости от кодека по‑разному собираем Annex B поток из RTP payload.
        let mut debug_path: &str = "";
        let annex_b = match self.kind {
            VideoCodecKind::H264 => {
                debug_path = "h264_depacketizer";
                let payload_bytes = Bytes::copy_from_slice(payload);
                let Some(h264_dep) = self.h264_depacketizer.as_mut() else {
                    return Ok(None);
                };
                match h264_dep.depacketize(&payload_bytes) {
                    Ok(b) => b,
                    Err(_) => return Ok(None),
                }
            }
            VideoCodecKind::H265 => {
                // Для H.265 (HEVC) используем формат RTP из RFC 7798:
                // - одиночные NAL (тип 0..47)
                // - агрегирующие пакеты AP (тип 48)
                // - фрагментированные NAL (FU; тип 49)
                if payload.len() < 3 {
                    return Ok(None);
                }
                let b0 = payload[0];
                let b1 = payload[1];
                let nal_type = (b0 & 0x7E) >> 1;
                match nal_type {
                    // Одиночный NAL‑юнит: просто добавляем старт‑код.
                    0..=47 => {
                        debug_path = "h265_single";
                        let mut buf = Vec::with_capacity(4 + payload.len());
                        buf.extend_from_slice(&[0, 0, 0, 1]);
                        buf.extend_from_slice(payload);
                        Bytes::from(buf)
                    }
                    // Aggregation Packet (AP): внутри несколько NAL‑юнитов с префиксом длины.
                    48 => {
                        debug_path = "h265_ap";
                        let mut out = Vec::with_capacity(payload.len() + 4 * 4);
                        let mut pos = 2usize; // пропускаем заголовок AP
                        let total = payload.len();
                        while pos + 2 <= total {
                            let nal_len = u16::from_be_bytes([payload[pos], payload[pos + 1]]) as usize;
                            pos += 2;
                            if nal_len == 0 || pos + nal_len > total {
                                break;
                            }
                            out.extend_from_slice(&[0, 0, 0, 1]);
                            out.extend_from_slice(&payload[pos..pos + nal_len]);
                            pos += nal_len;
                        }
                        Bytes::from(out)
                    }
                    // Fragmentation Unit (FU) — собираем один NAL из нескольких RTP‑пакетов.
                    49 => {
                        let fu_header = payload[2];
                        let s = (fu_header & 0x80) != 0;
                        let e = (fu_header & 0x40) != 0;
                        let orig_type = fu_header & 0x3F;

                        if s {
                            // Старт нового FU‑NAL: конструируем новый заголовок NAL
                            debug_path = if e { "h265_fu_single" } else { "h265_fu_start" };
                            let forbidden = b0 & 0x81;
                            let new_b0 = forbidden | ((orig_type << 1) & 0x7E);
                            let new_b1 = b1;
                            let mut buf = Vec::with_capacity(4 + 2 + (payload.len() - 3));
                            buf.extend_from_slice(&[0, 0, 0, 1, new_b0, new_b1]);
                            buf.extend_from_slice(&payload[3..]);
                            if e {
                                // NAL уместился в одном FU‑пакете.
                                Bytes::from(buf)
                            } else {
                                self.hevc_fu_buffer = Some(buf);
                                return Ok(None);
                            }
                        } else {
                            debug_path = if e { "h265_fu_end" } else { "h265_fu_mid" };
                            if let Some(ref mut buf) = self.hevc_fu_buffer {
                                buf.extend_from_slice(&payload[3..]);
                                if e {
                                    if let Some(buf_owned) = self.hevc_fu_buffer.take() {
                                        Bytes::from(buf_owned)
                                    } else {
                                        return Ok(None);
                                    }
                                } else {
                                    return Ok(None);
                                }
                            } else {
                                // Пришёл середина/конец FU без старта — дропаем.
                                return Ok(None);
                            }
                        }
                    }
                    // Прочие типы — оборачиваем как один NAL с префиксом.
                    _ => {
                        debug_path = "h265_unknown";
                        let mut buf = Vec::with_capacity(4 + payload.len());
                        buf.extend_from_slice(&[0, 0, 0, 1]);
                        buf.extend_from_slice(payload);
                        Bytes::from(buf)
                    }
                }
            }
        };

        // Отладка HEVC: первые несколько пакетов логируем подробно, чтобы понять формат потока.
        if matches!(self.kind, VideoCodecKind::H265) && self.debug_packets_logged <= 20 {
            let src_sample_len = payload.len().min(32);
            let annex_sample_len = annex_b.len().min(64);
            log::info!(
                "[hevc] pkt#{}, path={}, payload_len={}, annex_b_len={}, src[0..{}]={:02X?}, annex[0..{}]={:02X?}",
                self.debug_packets_logged,
                debug_path,
                payload.len(),
                annex_b.len(),
                src_sample_len,
                &payload[..src_sample_len],
                annex_sample_len,
                &annex_b[..annex_sample_len],
            );
        }

        if annex_b.is_empty() {
            return Ok(None);
        }

        let mut ff_packet = ffmpeg_next::codec::packet::Packet::copy(annex_b.as_ref());
        // Никакой зависимости от времени: pts/dts не передаём, на экране — только что пришедший кадр.
        ff_packet.set_pts(None);
        ff_packet.set_dts(None);
        ff_packet.set_duration(0);

        if self.decoder.send_packet(&ff_packet).is_err() {
            return Ok(None);
        }

        let mut decoded = FfmpegVideoFrame::empty();
        let mut out_frame = None;

        while self.decoder.receive_frame(&mut decoded).is_ok() {
            let w = decoded.width();
            let h = decoded.height();
            if w == 0 || h == 0 {
                continue;
            }

            if self.debug_frames_logged < 10 {
                self.debug_frames_logged += 1;
                log::info!(
                    "[video] raw decoded frame {:?} {}x{} pix_fmt={:?}",
                    self.kind,
                    w,
                    h,
                    decoded.format()
                );
            }

            // Создаём или обновляем scaler после первого кадра.
            if self.scaler.is_none() {
                match ScaleContext::get(
                    decoded.format(),
                    w,
                    h,
                    Pixel::RGB24,
                    w,
                    h,
                    ScaleFlags::BILINEAR,
                ) {
                    Ok(ctx) => self.scaler = Some(ctx),
                    Err(_) => continue,
                }
            }
            let scaler = self.scaler.as_mut().unwrap();
            scaler.cached(
                decoded.format(),
                w,
                h,
                Pixel::RGB24,
                w,
                h,
                ScaleFlags::BILINEAR,
            );
            if scaler.run(&decoded, &mut self.rgb_frame).is_err() {
                continue;
            }

            let stride = self.rgb_frame.stride(0) as usize;
            let size = stride * self.rgb_frame.height() as usize;
            let data = self.rgb_frame.data(0).to_vec();

            if self.debug_frames_logged <= 10 && !data.is_empty() {
                // Простейшая статистика по RGB, чтобы понять, не «серый» ли кадр целиком.
                let sample = &data[..data.len().min(3000)];
                let mut sum_r: u64 = 0;
                let mut sum_g: u64 = 0;
                let mut sum_b: u64 = 0;
                let mut count: u64 = 0;
                for chunk in sample.chunks_exact(3) {
                    sum_r += chunk[0] as u64;
                    sum_g += chunk[1] as u64;
                    sum_b += chunk[2] as u64;
                    count += 1;
                }
                if count > 0 {
                    log::info!(
                        "[video] RGB sample {:?}: avg R={} G={} B={} (from {} pixels)",
                        self.kind,
                        sum_r / count,
                        sum_g / count,
                        sum_b / count,
                        count
                    );
                }
            }

            out_frame = Some(DecodedFrame {
                width: w,
                height: h,
                data: data.into_iter().take(size).collect(),
            });
        }

        Ok(out_frame)
    }
}

/// Общий буфер последнего кадра для UI.
pub type SharedFrame = Arc<std::sync::Mutex<Option<DecodedFrame>>>;
