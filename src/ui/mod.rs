mod timeline;

use crate::app_state::{ArchiveCommand, ArchiveState, VideoViewState};
use crate::config::AppConfig;
use crate::video_decoder::SharedFrame;
use gtk4::gdk::Key;
use gtk4::prelude::*;
use gtk4::{
    Application, ApplicationWindow, Box as GtkBox, CssProvider, DrawingArea, EventControllerKey,
    EventControllerMotion, EventControllerScroll, GestureDrag, Orientation,
};
use std::cell::Cell;
use std::f64::consts::PI;
use std::rc::Rc;
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
        video_view: Arc<VideoViewState>,
        cmd_tx: mpsc::Sender<ArchiveCommand>,
        shared_frame: SharedFrame,
        frame_updated_rx: std::sync::mpsc::Receiver<()>,
    ) -> Self {
        let window = ApplicationWindow::new(app);
        window.set_title(Some("WebRTC Archive Player"));
        window.set_default_size(1280, 720);

        let root = GtkBox::new(Orientation::Vertical, 0);

        // Область видео (создаём до кнопок, чтобы кнопки могли вызывать queue_draw)
        let video_area = DrawingArea::new();
        video_area.set_hexpand(true);
        video_area.set_vexpand(true);

        let frame_for_draw = Arc::clone(&shared_frame);
        let video_view_draw = Arc::clone(&video_view);
        video_area.set_draw_func(move |_area, cr, width, height| {
            let area_w = width as f64;
            let area_h = height as f64;
            if area_w < 1.0 || area_h < 1.0 {
                return;
            }
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
                    // Cairo Format::Rgb24 использует 32 бита на пиксель в порядке 0x00RRGGBB (на
                    // little-endian в памяти это B,G,R,0). Наш кадр в RGB24 (R,G,B), поэтому
                    // раскладываем как B,G,R,0.
                    data[j] = chunk[2];       // B
                    data[j + 1] = chunk[1];   // G
                    data[j + 2] = chunk[0];   // R
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
            let zoom = video_view_draw.zoom_percent() as f64 / 100.0;
            let rot_deg = video_view_draw.rotation_deg() as f64;
            // Растягиваем кадр под размеры области (заполняем экран по ширине и высоте).
            let scale_x = (area_w / sw) * zoom;
            let scale_y = (area_h / sh) * zoom;
            let angle_rad = rot_deg * PI / 180.0;
            let cos_a = angle_rad.cos().abs();
            let sin_a = angle_rad.sin().abs();
            let drawn_w = sw * scale_x * cos_a + sh * scale_y * sin_a;
            let drawn_h = sw * scale_x * sin_a + sh * scale_y * cos_a;
            video_view_draw.clamp_pan_to_frame(area_w, area_h, drawn_w, drawn_h);
            let pan_x = video_view_draw.pan_x();
            let pan_y = video_view_draw.pan_y();
            let offset_x = (area_w - drawn_w) / 2.0;
            let offset_y = (area_h - drawn_h) / 2.0;
            cr.save().ok();
            cr.translate(
                offset_x + drawn_w / 2.0 + pan_x,
                offset_y + drawn_h / 2.0 + pan_y,
            );
            cr.rotate(angle_rad);
            cr.translate(-sw / 2.0 * scale_x, -sh / 2.0 * scale_y);
            cr.scale(scale_x, scale_y);
            cr.set_source_surface(&surface, 0.0, 0.0).ok();
            cr.paint().ok();
            cr.restore().ok();
        });

        video_area.set_hexpand(true);
        video_area.set_vexpand(true);
        video_area.set_focusable(true);
        let motion = EventControllerMotion::new();
        let video_for_focus = video_area.clone();
        motion.connect_enter(move |_, _x, _y| {
            video_for_focus.grab_focus();
        });
        video_area.add_controller(motion);

        // Управление мышью только над областью видео: колёсико = зум, Shift+колёсико = поворот 5°, перетаскивание = панорама.
        let shift_held: Rc<Cell<bool>> = Rc::new(Cell::new(false));
        let key_controller = EventControllerKey::new();
        let shift_held_press = Rc::clone(&shift_held);
        key_controller.connect_key_pressed(move |_, key, _modifiers, _| {
            if key == Key::Shift_L || key == Key::Shift_R {
                shift_held_press.set(true);
            }
            gtk4::glib::Propagation::Proceed
        });
        let shift_held_release = Rc::clone(&shift_held);
        key_controller.connect_key_released(move |_, key, _modifiers, _| {
            if key == Key::Shift_L || key == Key::Shift_R {
                shift_held_release.set(false);
            }
        });
        video_area.add_controller(key_controller);

        let scroll = EventControllerScroll::new(gtk4::EventControllerScrollFlags::VERTICAL);
        let vv_scroll = Arc::clone(&video_view);
        let va_scroll = video_area.clone();
        let shift_scroll = Rc::clone(&shift_held);
        scroll.connect_scroll(move |_, _dx, dy| {
            if shift_scroll.get() {
                if dy < 0.0 {
                    vv_scroll.rotate_5_cw();
                } else if dy > 0.0 {
                    vv_scroll.rotate_5_ccw();
                }
            } else {
                if dy < 0.0 {
                    vv_scroll.zoom_in();
                } else if dy > 0.0 {
                    vv_scroll.zoom_out();
                }
            }
            va_scroll.queue_draw();
            gtk4::glib::Propagation::Proceed
        });
        video_area.add_controller(scroll);

        let drag = GestureDrag::new();
        let vv_drag = Arc::clone(&video_view);
        let va_drag = video_area.clone();
        let last_drag: Rc<Cell<(f64, f64)>> = Rc::new(Cell::new((0.0, 0.0)));
        let last_drag_begin = Rc::clone(&last_drag);
        drag.connect_drag_begin(move |_, _x, _y| {
            last_drag_begin.set((0.0, 0.0));
        });
        let last_drag_up = Rc::clone(&last_drag);
        drag.connect_drag_update(move |_, offset_x, offset_y| {
            let (lx, ly) = last_drag_up.get();
            vv_drag.add_pan(offset_x - lx, offset_y - ly);
            last_drag_up.set((offset_x, offset_y));
            va_drag.queue_draw();
        });
        video_area.add_controller(drag);

        let timeline = timeline::new_timeline(state.clone(), cmd_tx.clone());
        timeline.set_hexpand(true);
        timeline.set_vexpand(false);

        // Перерисовка ~30 fps: сливаем уведомления о кадрах и перерисовываем видео + таймлайн.
        let video_area_redraw = video_area.clone();
        let timeline_redraw = timeline.clone();
        let rx = frame_updated_rx;
        gtk4::glib::timeout_add_local(std::time::Duration::from_millis(33), move || {
            while rx.try_recv().is_ok() {}
            video_area_redraw.queue_draw();
            timeline_redraw.queue_draw();
            gtk4::glib::ControlFlow::Continue
        });

        root.append(&video_area);
        root.append(&timeline);

        window.set_child(Some(&root));

        let window_clone = window.clone();
        window.connect_realize(move |_| {
            let display = gtk4::prelude::WidgetExt::display(&window_clone);
            let provider = CssProvider::new();
            let _ = provider.load_from_data(
                ".timeline-bar { background-color: #18181b; padding: 4px 0; } \
                 .timeline-controls { background-color: #18181b; padding: 2px 6px; border-radius: 4px; }"
            );
            gtk4::style_context_add_provider_for_display(
                &display,
                &provider,
                gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION,
            );
        });

        Self { window }
    }

    pub fn present(&self) {
        self.window.present();
    }
}

