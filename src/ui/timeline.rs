//! Таймлайн архива: шкала, зум (колёсико), панорама (Shift+колёсико или перетаскивание), клик -> PlayFrom.

use crate::app_state::ArchiveCommand;
use crate::archive_protocol::TimeRange;
use chrono::{DateTime, Local, Utc};
use gtk4::prelude::*;
use gtk4::gdk::ModifierType;
use gtk4::{DrawingArea, EventControllerScroll, EventControllerMotion, GestureClick, GestureDrag};
use std::sync::Arc;
use std::sync::Mutex;
use tokio::sync::mpsc;

const MARGIN: f64 = 10.0;
const BAR_HEIGHT: f64 = 28.0;
const LABEL_AREA_HEIGHT: f64 = 22.0;
const TICK_HEIGHT_SHORT: f64 = 10.0;
const PLAYHEAD_WIDTH: f64 = 2.5;
const PLAYHEAD_TRIANGLE_H: f64 = 8.0;
const MIN_VIEW_SPAN_MS: u64 = 5_000;   // минимум 5 сек при зуме
const ZOOM_FACTOR_PER_STEP: f64 = 1.15;
const PAN_FRACTION_PER_STEP: f64 = 0.2;

/// Видимое окно таймлайна (view_start..view_end). (0, 0) = «ещё не задано».
#[derive(Clone, Copy, Default)]
struct ViewState {
    view_start_ms: u64,
    view_end_ms: u64,
}

impl ViewState {
    fn span_ms(&self) -> u64 {
        self.view_end_ms.saturating_sub(self.view_start_ms)
    }
    fn is_unset(&self) -> bool {
        self.view_start_ms == 0 && self.view_end_ms == 0
    }
}

// Тёмная палитра (zinc-подобная)
mod palette {
    // Фон таймлайна — мягкий тёмно-серый (zinc-900)
    pub const BG: (f64, f64, f64) = (0.095, 0.095, 0.106);
    // Канавка полосы — на тон светлее (zinc-800)
    pub const TRACK: (f64, f64, f64) = (0.161, 0.161, 0.165);
    // Доступные отрезки — приглушённый синий/slate
    pub const RANGE: (f64, f64, f64) = (0.38, 0.52, 0.72);
    // Засечки — светлые на тёмной полосе (zinc-300)
    pub const TICK: (f64, f64, f64) = (0.78, 0.78, 0.82);
    // Подписи временной шкалы — хорошо читаемый светлый (zinc-200)
    pub const LABEL: (f64, f64, f64) = (0.88, 0.88, 0.91);
    // Ползунок воспроизведения — тёплый акцент (amber)
    pub const PLAYHEAD: (f64, f64, f64) = (0.965, 0.620, 0.043);
    // Подпись текущего времени над плейхедом — светлый текст
    pub const TIME_LABEL_TEXT: (f64, f64, f64) = (0.93, 0.93, 0.95);
    // Маркер при наведении — полупрозрачный светлый
    pub const HOVER_MARKER: (f64, f64, f64, f64) = (1.0, 1.0, 1.0, 0.45);
}
/// Цвет фона таймлайна в формате CSS (тёмная тема)
#[allow(dead_code)]
pub const TIMELINE_BG_CSS: &str = "#18181b";

fn ms_to_datetime_utc(ms: u64) -> Option<DateTime<Utc>> {
    DateTime::from_timestamp_millis(ms as i64)
}

/// Преобразование в локальное время для отображения (учёт timezone пользователя).
fn ms_to_local(ms: u64) -> Option<DateTime<Local>> {
    ms_to_datetime_utc(ms).map(|utc| utc.with_timezone(&Local))
}

/// Форматирование времени для шкалы (ЧЧ:ММ:СС), в локальной временной зоне.
fn format_scale(ms: u64) -> String {
    ms_to_local(ms)
        .map(|dt| dt.format("%H:%M:%S").to_string())
        .unwrap_or_else(|| "--:--:--".to_string())
}

/// Полная дата и время для подписи над плейхедом (ДД.ММ.ГГГГ ЧЧ:ММ:СС).
fn format_playhead_label(ms: u64) -> String {
    ms_to_local(ms)
        .map(|dt| dt.format("%d.%m.%Y %H:%M:%S").to_string())
        .unwrap_or_else(|| "--.--.---- --:--:--".to_string())
}

/// Форматирование времени для подсказки при наведении (полное), в локальной временной зоне.
fn format_tooltip(ms: u64) -> String {
    ms_to_local(ms)
        .map(|dt| dt.format("%Y-%m-%d %H:%M:%S %Z").to_string())
        .unwrap_or_default()
}

fn span(ranges: &[TimeRange]) -> (u64, u64) {
    let mut t_min = u64::MAX;
    let mut t_max = 0u64;
    for r in ranges {
        t_min = t_min.min(r.start_time);
        t_max = t_max.max(r.end_time);
    }
    if t_min == u64::MAX {
        t_min = 0;
    }
    (t_min, t_max)
}

/// Преобразование X (в области полосы) в timestamp.
fn x_to_timestamp(x: f64, track_width: f64, t_min: u64, t_max: u64) -> u64 {
    if track_width <= 0.0 {
        return t_min;
    }
    let t = t_min as f64 + (x / track_width).clamp(0.0, 1.0) * (t_max - t_min) as f64;
    t.round() as u64
}

/// Преобразование timestamp в X относительно начала полосы (track_left).
fn timestamp_to_x(ts_ms: u64, track_left: f64, track_width: f64, t_min: u64, t_max: u64) -> f64 {
    if t_max <= t_min || track_width <= 0.0 {
        return track_left;
    }
    let span_ms = (t_max - t_min) as f64;
    let frac = ((ts_ms.saturating_sub(t_min)) as f64 / span_ms).clamp(0.0, 1.0);
    track_left + frac * track_width
}

/// Шаг засечек в минутах в зависимости от длины диапазона.
fn tick_step_minutes(span_ms: u64) -> u64 {
    let span_min = span_ms / 60_000;
    if span_min > 24 * 60 {
        60
    } else if span_min > 6 * 60 {
        30
    } else if span_min > 120 {
        15
    } else if span_min > 60 {
        10
    } else if span_min > 30 {
        5
    } else {
        1
    }
}

/// Создаёт виджет таймлайна с зумом, панорамой и навигацией по клику.
pub fn new_timeline(
    state: Arc<crate::app_state::ArchiveState>,
    cmd_tx: mpsc::Sender<ArchiveCommand>,
) -> DrawingArea {
    let area = DrawingArea::new();
    area.set_content_height(96);

    let view_state = Arc::new(Mutex::new(ViewState::default()));
    let last_mouse_x = Arc::new(Mutex::new(0.0f64));
    let hover_x: Arc<Mutex<Option<f64>>> = Arc::new(Mutex::new(None));

    area.set_draw_func({
        let state_draw = Arc::clone(&state);
        let view_state_draw = Arc::clone(&view_state);
        let hover_x_draw = Arc::clone(&hover_x);
        move |_, cr, width, height| {
            let w = width as f64;
            let h = height as f64;
            cr.set_source_rgb(palette::BG.0, palette::BG.1, palette::BG.2);
            cr.rectangle(0.0, 0.0, w, h);
            cr.fill().ok();

            let ranges = state_draw.get_ranges();
            if ranges.is_empty() {
                return;
            }
            let (t_min, t_max) = span(ranges.as_slice());
            let full_span_ms = t_max.saturating_sub(t_min);
            if full_span_ms == 0 {
                return;
            }

            let track_left = MARGIN;
            let track_width = (w - 2.0 * MARGIN).max(1.0);
            let bar_top = MARGIN + LABEL_AREA_HEIGHT;
            let bar_bottom = bar_top + BAR_HEIGHT;

            let (view_start, view_end) = {
                let mut vs = view_state_draw.lock().unwrap();
                if vs.is_unset() || vs.view_start_ms < t_min || vs.view_end_ms > t_max {
                    vs.view_start_ms = t_min;
                    vs.view_end_ms = t_max;
                }
                (vs.view_start_ms, vs.view_end_ms)
            };
            let view_span_ms = view_end.saturating_sub(view_start).max(1);

            // Временная шкала: подпись начала и конца видимого окна над полосой
            cr.set_source_rgb(palette::LABEL.0, palette::LABEL.1, palette::LABEL.2);
            cr.select_font_face("Sans", cairo::FontSlant::Normal, cairo::FontWeight::Normal);
            cr.set_font_size(11.0);
            cr.move_to(track_left, MARGIN + 12.0);
            cr.show_text(&format_scale(view_start)).ok();
            let end_text = format_scale(view_end);
            let ext = cr.text_extents(&end_text).ok();
            let end_x = (track_left + track_width - ext.map(|e| e.width()).unwrap_or(0.0)).max(track_left);
            cr.move_to(end_x, MARGIN + 12.0);
            cr.show_text(&end_text).ok();

            // Фон полосы (тёмная канавка)
            cr.set_source_rgb(palette::TRACK.0, palette::TRACK.1, palette::TRACK.2);
            cr.rectangle(track_left, bar_top, track_width, BAR_HEIGHT);
            cr.fill().ok();

            // Подписи временной шкалы под полосой (вертикальные линии рисуем после отрезков)
            let step_min = tick_step_minutes(view_span_ms);
            let step_ms = step_min * 60 * 1000;
            let first_tick_ms = (view_start / step_ms).saturating_mul(step_ms);
            if step_ms > 0 {
                cr.select_font_face("Sans", cairo::FontSlant::Normal, cairo::FontWeight::Normal);
                cr.set_font_size(11.0);
                let mut t = first_tick_ms;
                while t <= view_end {
                    let x = timestamp_to_x(t, track_left, track_width, view_start, view_end);
                    if x >= track_left && x <= track_left + track_width {
                        let label = format_scale(t);
                        let ext = cr.text_extents(&label).ok();
                        let lw = ext.map(|e| e.width()).unwrap_or(0.0);
                        cr.set_source_rgb(palette::LABEL.0, palette::LABEL.1, palette::LABEL.2);
                        cr.move_to((x - lw / 2.0).max(track_left), bar_bottom + 14.0);
                        cr.show_text(&label).ok();
                    }
                    t = t.saturating_add(step_ms);
                }
            }

            // Отрезки доступного видео (в видимом окне)
            for r in &ranges {
                let r0 = r.start_time.max(view_start);
                let r1 = r.end_time.min(view_end);
                if r1 <= r0 {
                    continue;
                }
                let x0 = timestamp_to_x(r0, track_left, track_width, view_start, view_end);
                let x1 = timestamp_to_x(r1, track_left, track_width, view_start, view_end);
                let seg_w = (x1 - x0).max(1.0);
                cr.set_source_rgb(palette::RANGE.0, palette::RANGE.1, palette::RANGE.2);
                cr.rectangle(x0, bar_top, seg_w, BAR_HEIGHT);
                cr.fill().ok();
            }

            // Ползунок воспроизведения (позиция из RTP timestamp с учётом разворота 32-bit)
            let start_ms = state_draw.playback_start_ms.load(std::sync::atomic::Ordering::Relaxed);
            let end_ms = state_draw.playback_end_ms.load(std::sync::atomic::Ordering::Relaxed);
            let position_ms = state_draw.playback_position_ms.load(std::sync::atomic::Ordering::Relaxed);
            let raw_pos = if position_ms > 0 { position_ms } else { start_ms };
            let draw_pos_ms = if end_ms > start_ms {
                raw_pos.min(end_ms)
            } else {
                raw_pos
            };
            let pos_in_view = draw_pos_ms >= view_start && draw_pos_ms <= view_end;
            if end_ms > start_ms && pos_in_view {
                let px = timestamp_to_x(draw_pos_ms, track_left, track_width, view_start, view_end);
                // Сначала рисуем плейхед, чтобы подпись времени поверх него не перекрывалась
                cr.set_source_rgb(palette::PLAYHEAD.0, palette::PLAYHEAD.1, palette::PLAYHEAD.2);
                cr.rectangle(px - PLAYHEAD_WIDTH / 2.0, bar_top, PLAYHEAD_WIDTH, BAR_HEIGHT);
                cr.fill().ok();
                cr.move_to(px, bar_top - PLAYHEAD_TRIANGLE_H);
                cr.line_to(px - 6.0, bar_top);
                cr.line_to(px + 6.0, bar_top);
                cr.close_path();
                cr.fill().ok();
                // Подпись времени над плейхедом: рисуем поверх маркера, с запасом по высоте
                let time_str = format_playhead_label(draw_pos_ms);
                cr.select_font_face("Sans", cairo::FontSlant::Normal, cairo::FontWeight::Normal);
                cr.set_font_size(11.0);
                let ext = cr.text_extents(&time_str).ok();
                if let Some(te) = ext {
                    let pad_x = 10.0f64;
                    let pad_y = 4.0f64;
                    let rw = te.width() + 2.0 * pad_x;
                    let rh = te.height() + 2.0 * pad_y;
                    let rx = (px - rw / 2.0).clamp(MARGIN, (w - MARGIN - rw).max(MARGIN));
                    let ry = (bar_top - PLAYHEAD_TRIANGLE_H - rh - 14.0).max(MARGIN);
                    let radius = 6.0;
                    let pi = std::f64::consts::PI;
                    cr.set_source_rgb(palette::BG.0, palette::BG.1, palette::BG.2);
                    cr.new_path();
                    cr.move_to(rx + radius, ry);
                    cr.line_to(rx + rw - radius, ry);
                    cr.arc(rx + rw - radius, ry + radius, radius, 1.5 * pi, 0.0);
                    cr.line_to(rx + rw, ry + rh - radius);
                    cr.arc(rx + rw - radius, ry + rh - radius, radius, 0.0, 0.5 * pi);
                    cr.line_to(rx + radius, ry + rh);
                    cr.arc(rx + radius, ry + rh - radius, radius, 0.5 * pi, pi);
                    cr.line_to(rx, ry + radius);
                    cr.arc(rx + radius, ry + radius, radius, pi, 1.5 * pi);
                    cr.close_path();
                    cr.fill().ok();
                    cr.set_source_rgb(palette::TIME_LABEL_TEXT.0, palette::TIME_LABEL_TEXT.1, palette::TIME_LABEL_TEXT.2);
                    cr.move_to(rx + pad_x, ry + pad_y + te.height() - te.y_bearing());
                    cr.show_text(&time_str).ok();
                }
            }

            // Вертикальные линии шкалы поверх полосы: каждая отметка времени — линия через всю полосу
            if step_ms > 0 {
                cr.set_source_rgb(palette::TICK.0, palette::TICK.1, palette::TICK.2);
                cr.set_line_width(1.0);
                let mut t = first_tick_ms;
                while t <= view_end {
                    let x = timestamp_to_x(t, track_left, track_width, view_start, view_end);
                    if x >= track_left && x <= track_left + track_width {
                        cr.move_to(x, bar_top);
                        cr.line_to(x, bar_bottom);
                        cr.stroke().ok();
                    }
                    t = t.saturating_add(step_ms);
                }
            }

            // Иллюзорный маркер при наведении на полосу
            if let Ok(guard) = hover_x_draw.lock() {
                if let Some(x) = *guard {
                    let hx = x.clamp(track_left, track_left + track_width);
                    cr.set_source_rgba(
                        palette::HOVER_MARKER.0,
                        palette::HOVER_MARKER.1,
                        palette::HOVER_MARKER.2,
                        palette::HOVER_MARKER.3,
                    );
                    cr.set_line_width(1.5);
                    cr.move_to(hx, bar_top);
                    cr.line_to(hx, bar_bottom);
                    cr.stroke().ok();
                    // Небольшой треугольник над полосой (как у ползунка, но тоньше)
                    cr.move_to(hx, bar_top - TICK_HEIGHT_SHORT);
                    cr.line_to(hx - 4.0, bar_top);
                    cr.line_to(hx + 4.0, bar_top);
                    cr.close_path();
                    cr.fill().ok();
                }
            }
        }
    });

    // Клик по полосе — переход к воспроизведению с этого момента (в видимом окне)
    let state_click = Arc::clone(&state);
    let view_state_click = Arc::clone(&view_state);
    let area_click = area.clone();
    let click = GestureClick::new();
    click.connect_pressed(move |gesture, _n, x, y| {
        let ranges = state_click.get_ranges();
        if ranges.is_empty() {
            return;
        }
        let (t_min, t_max) = span(ranges.as_slice());
        if t_max <= t_min {
            return;
        }
        let w = area_click.allocated_width().max(1) as f64;
        let bar_top = MARGIN + LABEL_AREA_HEIGHT;
        let track_left = MARGIN;
        let track_width = (w - 2.0 * MARGIN).max(1.0);
        let x_clamped = x.clamp(0.0, w);
        if y >= bar_top && y <= bar_top + BAR_HEIGHT {
            let (view_start, view_end) = {
                let vs = view_state_click.lock().unwrap();
                (vs.view_start_ms, vs.view_end_ms)
            };
            if view_end > view_start {
                let local_x = (x_clamped - track_left).clamp(0.0, track_width);
                let timestamp_ms = x_to_timestamp(local_x, track_width, view_start, view_end);
                let _ = cmd_tx.try_send(ArchiveCommand::PlayFrom { timestamp_ms });
                log::info!("Timeline: клик -> воспроизведение с {}", format_tooltip(timestamp_ms));
            }
        }
        gesture.widget().queue_draw();
    });
    area.add_controller(click);

    // Наведение: подсказка с временем, курсор-указатель и иллюзорный маркер над полосой
    let state_motion = Arc::clone(&state);
    let view_state_motion = Arc::clone(&view_state);
    let last_mouse_x_motion = Arc::clone(&last_mouse_x);
    let hover_x_motion = Arc::clone(&hover_x);
    let area_motion = area.clone();
    let motion = EventControllerMotion::new();
    motion.connect_motion(move |controller, x, y| {
        let widget = controller.widget();
        if let Ok(mut last) = last_mouse_x_motion.lock() {
            *last = x;
        }
        let w = widget.allocated_width().max(1) as f64;
        let bar_top = MARGIN + LABEL_AREA_HEIGHT;
        let track_left = MARGIN;
        let track_width = (w - 2.0 * MARGIN).max(1.0);

        if y >= bar_top && y <= bar_top + BAR_HEIGHT && track_width > 0.0 {
            if let Ok(mut h) = hover_x_motion.lock() {
                *h = Some(x);
            }
            area_motion.queue_draw();
            let local_x = (x - track_left).clamp(0.0, track_width);
            let ranges = state_motion.get_ranges();
            if ranges.is_empty() {
                widget.set_tooltip_text(None);
                if let Some(ref c) = gtk4::gdk::Cursor::from_name("default", None) {
                    widget.set_cursor(Some(c));
                } else {
                    widget.set_cursor(None);
                }
                return;
            }
            let (view_start, view_end) = {
                let vs = view_state_motion.lock().unwrap();
                (vs.view_start_ms, vs.view_end_ms)
            };
            if view_end > view_start {
                let timestamp_ms = x_to_timestamp(local_x, track_width, view_start, view_end);
                widget.set_tooltip_text(Some(&format_tooltip(timestamp_ms)));
            }
            if let Some(ref c) = gtk4::gdk::Cursor::from_name("pointer", None) {
                widget.set_cursor(Some(c));
            } else {
                widget.set_cursor(None);
            }
        } else {
            if let Ok(mut h) = hover_x_motion.lock() {
                if h.is_some() {
                    *h = None;
                    area_motion.queue_draw();
                }
            }
            widget.set_tooltip_text(None);
            if let Some(ref c) = gtk4::gdk::Cursor::from_name("default", None) {
                widget.set_cursor(Some(c));
            } else {
                widget.set_cursor(None);
            }
        }
    });
    let hover_x_leave = Arc::clone(&hover_x);
    let area_leave = area.clone();
    motion.connect_leave(move |controller| {
        let widget = controller.widget();
        if let Ok(mut h) = hover_x_leave.lock() {
            if h.is_some() {
                *h = None;
                area_leave.queue_draw();
            }
        }
        widget.set_tooltip_text(None);
        if let Some(ref c) = gtk4::gdk::Cursor::from_name("default", None) {
            widget.set_cursor(Some(c));
        } else {
            widget.set_cursor(None);
        }
    });
    area.add_controller(motion);

    // Колёсико: вертикаль — без Shift зум (центр под курсором), с Shift панорама.
    // Горизонтальный скролл (наклон колёсика) — всегда панорама влево/вправо.
    let view_state_scroll = Arc::clone(&view_state);
    let state_scroll = Arc::clone(&state);
    let last_mouse_x_scroll = Arc::clone(&last_mouse_x);
    let area_scroll = area.clone();
    let scroll = EventControllerScroll::new(gtk4::EventControllerScrollFlags::BOTH_AXES);
    scroll.connect_scroll(move |controller, dx, dy| {
        let ranges = state_scroll.get_ranges();
        if ranges.is_empty() {
            return gtk4::glib::Propagation::Proceed;
        }
        let (t_min, t_max) = span(ranges.as_slice());
        let full_span = t_max.saturating_sub(t_min);
        if full_span == 0 {
            return gtk4::glib::Propagation::Proceed;
        }
        let w = area_scroll.allocated_width().max(1) as f64;
        let track_left = MARGIN;
        let track_width = (w - 2.0 * MARGIN).max(1.0);

        let shift = controller
            .current_event()
            .map(|e| e.modifier_state().contains(ModifierType::SHIFT_MASK))
            .unwrap_or(false);

        let mut vs = view_state_scroll.lock().unwrap();
        let view_span = vs.span_ms().max(1) as f64;
        let view_span_u = vs.span_ms();

        // Горизонтальный скролл (dx) или вертикальный с Shift — панорама
        let pan_dy = if shift { dy } else { 0.0 };
        let pan_dx = dx; // горизонтальный скролл: dx > 0 = вправо = сдвиг к более позднему времени
        let pan_total = pan_dy + pan_dx;
        let do_pan = pan_total.abs() > 1e-6;

        if do_pan {
            // Панорама: положительное значение = сдвиг вида к более позднему времени
            let delta_ms = (pan_total * view_span * PAN_FRACTION_PER_STEP) as i64;
            let new_start = (vs.view_start_ms as i64 + delta_ms)
                .clamp(t_min as i64, (t_max.saturating_sub(view_span_u)) as i64);
            let new_start = new_start.max(0) as u64;
            vs.view_start_ms = new_start.clamp(t_min, t_max.saturating_sub(1));
            vs.view_end_ms = (vs.view_start_ms + view_span_u).min(t_max);
        } else if dy.abs() > 1e-6 {
            // Зум: центр под курсором
            let mouse_x = *last_mouse_x_scroll.lock().unwrap();
            let local_x = (mouse_x - track_left).clamp(0.0, track_width);
            let t_center = x_to_timestamp(local_x, track_width, vs.view_start_ms, vs.view_end_ms);

            let factor = if dy < 0.0 {
                ZOOM_FACTOR_PER_STEP
            } else {
                1.0 / ZOOM_FACTOR_PER_STEP
            };
            let new_span_f = view_span * factor;
            let new_span_ms = (new_span_f as u64)
                .clamp(MIN_VIEW_SPAN_MS, full_span);
            let frac = if track_width > 0.0 { (local_x / track_width).clamp(0.0, 1.0) } else { 0.5 };
            let mut new_start = t_center.saturating_sub((frac * new_span_ms as f64) as u64);
            let mut new_end = new_start.saturating_add(new_span_ms);
            if new_end > t_max {
                new_end = t_max;
                new_start = new_end.saturating_sub(new_span_ms);
            }
            if new_start < t_min {
                new_start = t_min;
                new_end = (t_min + new_span_ms).min(t_max);
            }
            vs.view_start_ms = new_start;
            vs.view_end_ms = new_end;
        }
        drop(vs);
        area_scroll.queue_draw();
        gtk4::glib::Propagation::Proceed
    });
    area.add_controller(scroll);

    // Перетаскивание зажатой кнопкой мыши — панорама влево/вправо
    let view_state_drag = Arc::clone(&view_state);
    let state_drag = Arc::clone(&state);
    let drag = GestureDrag::new();
    let drag_start_view: Arc<Mutex<Option<(u64, u64)>>> = Arc::new(Mutex::new(None));
    let state_drag_begin = Arc::clone(&state_drag);
    let view_state_drag_begin = Arc::clone(&view_state_drag);
    let drag_start_begin = Arc::clone(&drag_start_view);
    drag.connect_drag_begin(move |_gesture, _x, _y| {
        let ranges = state_drag_begin.get_ranges();
        if ranges.is_empty() {
            return;
        }
        let (t_min, t_max) = span(ranges.as_slice());
        if t_max <= t_min {
            return;
        }
        let vs = view_state_drag_begin.lock().unwrap();
        *drag_start_begin.lock().unwrap() = Some((vs.view_start_ms, vs.view_end_ms));
    });
    let state_drag_update = Arc::clone(&state_drag);
    let view_state_drag_update = Arc::clone(&view_state_drag);
    let drag_start_update = Arc::clone(&drag_start_view);
    let area_drag_update = area.clone();
    drag.connect_drag_update(move |gesture, offset_x, _offset_y| {
        let start = match *drag_start_update.lock().unwrap() {
            Some(s) => s,
            None => return,
        };
        let ranges = state_drag_update.get_ranges();
        if ranges.is_empty() {
            return;
        }
        let (t_min, t_max) = span(ranges.as_slice());
        let w = area_drag_update.allocated_width().max(1) as f64;
        let track_width = (w - 2.0 * MARGIN).max(1.0);
        let view_span = start.1.saturating_sub(start.0);
        if view_span == 0 || track_width <= 0.0 {
            return;
        }
        let delta_t_ms = (offset_x / track_width * view_span as f64) as i64;
        let new_start = (start.0 as i64 - delta_t_ms)
            .max(t_min as i64)
            .min((t_max.saturating_sub(view_span)) as i64);
        let new_start = (new_start.max(0) as u64).clamp(t_min, t_max.saturating_sub(view_span));
        let new_end = (new_start + view_span).min(t_max);
        let new_start = new_end.saturating_sub(view_span).max(t_min);
        {
            let mut vs = view_state_drag_update.lock().unwrap();
            vs.view_start_ms = new_start;
            vs.view_end_ms = new_end;
        }
        gesture.widget().queue_draw();
    });
    let drag_start_end = Arc::clone(&drag_start_view);
    drag.connect_drag_end(move |_gesture, _x, _y| {
        *drag_start_end.lock().unwrap() = None;
    });
    area.add_controller(drag);

    // Перерисовка по таймеру; при обновлении ranges сбрасываем вид на полный диапазон
    let area_redraw = area.clone();
    let state_redraw = Arc::clone(&state);
    let view_state_redraw = Arc::clone(&view_state);
    gtk4::glib::timeout_add_local(std::time::Duration::from_millis(300), move || {
        if state_redraw.timeline_dirty.swap(false, std::sync::atomic::Ordering::Relaxed) {
            let ranges = state_redraw.get_ranges();
            if !ranges.is_empty() {
                let (t_min, t_max) = span(ranges.as_slice());
                if t_max > t_min {
                    let mut vs = view_state_redraw.lock().unwrap();
                    vs.view_start_ms = t_min;
                    vs.view_end_ms = t_max;
                }
            }
            area_redraw.queue_draw();
        }
        area_redraw.queue_draw();
        gtk4::glib::ControlFlow::Continue
    });

    area
}
