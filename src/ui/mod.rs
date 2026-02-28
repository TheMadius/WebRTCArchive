mod timeline;

use crate::app_state::ArchiveState;
use crate::config::AppConfig;
use crate::video_decoder::SharedFrame;
use gtk4::prelude::*;
use gtk4::{Application, ApplicationWindow, Box as GtkBox, DrawingArea, Orientation};
use std::sync::Arc;
use tokio::sync::mpsc;


pub struct MainWindow {
    window: ApplicationWindow,
}

impl MainWindow {
    pub fn new(
        app: &Application,
        _config: AppConfig,
        state: Arc<ArchiveState>,
        cmd_tx: mpsc::Sender<crate::app_state::ArchiveCommand>,
        shared_frame: SharedFrame,
        frame_updated_rx: std::sync::mpsc::Receiver<()>,
    ) -> Self {
        let window = ApplicationWindow::new(app);
        window.set_title(Some("WebRTC Archive Player"));
        window.set_default_size(1280, 720);

        let root = GtkBox::new(Orientation::Vertical, 0);

        // Область видео: занимает всё доступное место, при изменении окна масштабируется
        let video_area = DrawingArea::new();
        video_area.set_hexpand(true);
        video_area.set_vexpand(true);

        let frame_for_draw = Arc::clone(&shared_frame);
        video_area.set_draw_func(move |_area, cr, width, height| {
            let area_w = width as f64;
            let area_h = height as f64;
            // Фон на всю область (видно при отсутствии кадра или при letterbox)
            cr.set_source_rgb(0.1, 0.1, 0.1);
            cr.rectangle(0.0, 0.0, area_w, area_h);
            cr.fill().ok();

            let frame = {
                let guard = match frame_for_draw.lock() {
                    Ok(g) => g,
                    Err(_) => return,
                };
                guard.clone()
            };
            let Some(ref frame) = frame else {
                return;
            };
            log::debug!("[video] UI draw: frame {}x{}", frame.width, frame.height);
            let w = frame.width as i32;
            let h = frame.height as i32;
            let size_rgb = (w as usize).saturating_mul(h as usize).saturating_mul(3);
            if w <= 0 || h <= 0 || frame.data.len() < size_rgb {
                return;
            }
            let stride = w * 4;
            let mut data = vec![0u8; (w as usize).saturating_mul(h as usize).saturating_mul(4)];
            let src = &frame.data[..size_rgb];
            for (i, chunk) in src.chunks_exact(3).enumerate() {
                let j = i * 4;
                if j + 4 <= data.len() {
                    data[j] = chunk[0];
                    data[j + 1] = chunk[1];
                    data[j + 2] = chunk[2];
                    data[j + 3] = 0;
                }
            }
            let surface: cairo::ImageSurface = match cairo::ImageSurface::create_for_data::<Vec<u8>>(
                data,
                cairo::Format::Rgb24,
                w,
                h,
                stride,
            ) {
                Ok(s) => s,
                Err(_) => return,
            };
            let sw = surface.width() as f64;
            let sh = surface.height() as f64;
            if sw <= 0.0 || sh <= 0.0 {
                return;
            }
            // Масштаб «вписать»: изображение целиком в области, пропорции сохраняются
            let scale_x = area_w / sw;
            let scale_y = area_h / sh;
            let scale = scale_x.min(scale_y);
            let drawn_w = sw * scale;
            let drawn_h = sh * scale;
            let offset_x = (area_w - drawn_w) / 2.0;
            let offset_y = (area_h - drawn_h) / 2.0;
            cr.save().ok();
            cr.translate(offset_x, offset_y);
            cr.scale(scale, scale);
            cr.set_source_surface(&surface, 0.0, 0.0).ok();
            cr.paint().ok();
            cr.restore().ok();
        });

        let timeline = timeline::new_timeline(state.clone(), cmd_tx);
        timeline.set_hexpand(true);
        timeline.set_vexpand(false);

        // Одна перерисовка ~30 fps: сливаем уведомления о кадрах и перерисовываем видео + таймлайн (маркер воспроизведения).
        let video_area_redraw = video_area.clone();
        let timeline_redraw = timeline.clone();
        let rx = frame_updated_rx;
        gtk4::glib::timeout_add_local(std::time::Duration::from_millis(33), move || {
            while rx.try_recv().is_ok() {}
            video_area_redraw.queue_draw();
            timeline_redraw.queue_draw();
            gtk4::glib::ControlFlow::Continue
        });

        // Резерв: если RTP перестал обновлять позицию (например, после смены фрагмента), двигаем маркер по реальному времени.
        let state_fallback = state.clone();
        let prev_position = std::cell::Cell::new(0u64);
        gtk4::glib::timeout_add_local(std::time::Duration::from_millis(400), move || {
            let gen = state_fallback.playback_generation.load(std::sync::atomic::Ordering::Relaxed);
            let start_ms = state_fallback.playback_start_ms.load(std::sync::atomic::Ordering::Relaxed);
            let end_ms = state_fallback.playback_end_ms.load(std::sync::atomic::Ordering::Relaxed);
            let pos = state_fallback.playback_position_ms.load(std::sync::atomic::Ordering::Relaxed);
            let prev = prev_position.get();
            if gen != 0 && start_ms < end_ms && pos < end_ms && pos == prev && prev != 0 {
                let new_pos = (pos + 400).min(end_ms);
                state_fallback.set_playback_position(new_pos);
            }
            prev_position.set(pos);
            gtk4::glib::ControlFlow::Continue
        });

        root.append(&video_area);
        root.append(&timeline);

        window.set_child(Some(&root));

        Self { window }
    }

    pub fn present(&self) {
        self.window.present();
    }
}

