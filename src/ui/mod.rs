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

        let root = GtkBox::new(Orientation::Vertical, 4);

        let video_area = DrawingArea::new();
        video_area.set_content_width(1280);
        video_area.set_content_height(640);

        let frame_for_draw = Arc::clone(&shared_frame);
        video_area.set_draw_func(move |_area, cr, width, height| {
            // Копируем кадр под замком и сразу отпускаем — конвертация и отрисовка без блокировки,
            // чтобы декодер мог писать новые кадры и воспроизведение шло в реальном времени.
            let frame = {
                let guard = match frame_for_draw.lock() {
                    Ok(g) => g,
                    Err(_) => return,
                };
                guard.clone()
            };
            let Some(ref frame) = frame else {
                cr.set_source_rgb(0.1, 0.1, 0.1);
                cr.paint().ok();
                return;
            };
            log::debug!("[video] UI draw: frame {}x{}", frame.width, frame.height);
            let w = frame.width as i32;
            let h = frame.height as i32;
            let size_rgb = (w as usize).saturating_mul(h as usize).saturating_mul(3);
            if w <= 0 || h <= 0 || frame.data.len() < size_rgb {
                return;
            }
            // Cairo Rgb24 = 32 bpp (stride = width*4). Конвертируем RGB24 (3 bpp) -> padding 4.
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
            let scale_x = width as f64 / sw;
            let scale_y = height as f64 / sh;
            let scale = scale_x.min(scale_y);
            cr.save().ok();
            cr.scale(scale, scale);
            cr.set_source_surface(&surface, 0.0, 0.0).ok();
            cr.paint().ok();
            cr.restore().ok();
        });

        // Одна перерисовка ~30 fps: сливаем уведомления о кадрах и перерисовываем. Без коротких таймеров (2/16 мс) ядро не грузится в 100%.
        let video_area_redraw = video_area.clone();
        let rx = frame_updated_rx;
        gtk4::glib::timeout_add_local(std::time::Duration::from_millis(33), move || {
            while rx.try_recv().is_ok() {}
            video_area_redraw.queue_draw();
            gtk4::glib::ControlFlow::Continue
        });

        let timeline = timeline::new_timeline(state, cmd_tx);

        root.append(&video_area);
        root.append(&timeline);

        window.set_child(Some(&root));

        Self { window }
    }

    pub fn present(&self) {
        self.window.present();
    }
}

