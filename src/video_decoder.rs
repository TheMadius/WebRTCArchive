//! Декодирование RTP H.264 в сырые кадры (RGB24) для вывода в UI.

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
use webrtc::rtp::packet::Packet as RtpPacket;
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

/// Декодер RTP H.264 -> RGB24 кадры.
pub struct VideoDecoder {
    h264_depacketizer: H264Packet,
    decoder: FfmpegVideoDecoder,
    scaler: Option<ScaleContext>,
    rgb_frame: FfmpegVideoFrame,
}

impl VideoDecoder {
    pub fn new() -> Result<Self> {
        ffmpeg_next::init().context("ffmpeg init")?;

        let codec = ffmpeg_decoder::find(Id::H264).ok_or_else(|| anyhow::anyhow!("H264 codec not found"))?;
        let mut decoder_ctx = ffmpeg_decoder::Decoder(codec::Context::new());
        decoder_ctx.set_flags(CodecFlags::LOW_DELAY);
        // Максимально загружаем ядра декодером, чтобы не отставать от потока и не давать видео «ползти».
        let threads = std::cmp::max(1, num_cpus::get());
        decoder_ctx.set_threading(codec::threading::Config {
            kind: codec::threading::Type::Frame,
            count: threads,
            ..codec::threading::Config::default()
        });
        let decoder = decoder_ctx
            .open_as(codec)
            .context("open H264 decoder")?
            .video()
            .context("decoder as video")?;

        Ok(Self {
            h264_depacketizer: H264Packet::default(),
            decoder,
            scaler: None,
            rgb_frame: FfmpegVideoFrame::empty(),
        })
    }

    /// Подаёт RTP-пакет, при необходимости возвращает декодированный кадр.
    pub fn push_rtp(&mut self, pkt: &RtpPacket) -> Result<Option<DecodedFrame>> {
        self.push_payload(&pkt.payload)
    }

    /// Подаёт payload RTP (для конвейера: чтение и декодирование в разных потоках).
    pub fn push_payload(&mut self, payload: &[u8]) -> Result<Option<DecodedFrame>> {
        if payload.is_empty() {
            return Ok(None);
        }
        let payload_bytes = Bytes::copy_from_slice(payload);
        let annex_b = match self.h264_depacketizer.depacketize(&payload_bytes) {
            Ok(b) => b,
            Err(_) => return Ok(None),
        };
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
