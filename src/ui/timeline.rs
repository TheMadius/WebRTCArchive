//! Таймлайн архива: шкала с засечками по минутам, подсказка времени при наведении, клик -> PlayFrom.

use crate::app_state::ArchiveCommand;
use crate::archive_protocol::TimeRange;
use chrono::{DateTime, Local, Utc};
use gtk4::prelude::*;
use gtk4::{DrawingArea, EventControllerMotion, GestureClick};
use std::sync::Arc;
use tokio::sync::mpsc;

const MARGIN: f64 = 10.0;
const BAR_HEIGHT: f64 = 28.0;
const LABEL_AREA_HEIGHT: f64 = 20.0;
const TICK_HEIGHT_SHORT: f64 = 6.0;
const PLAYHEAD_WIDTH: f64 = 2.5;
const PLAYHEAD_TRIANGLE_H: f64 = 8.0;

// Современная палитра (тёмная тема, комфортная для восприятия)
mod palette {
    // Фон таймлайна — мягкий тёмно-серый (zinc-900)
    pub const BG: (f64, f64, f64) = (0.095, 0.095, 0.106);
    // Канавка полосы — на тон светлее (zinc-800)
    pub const TRACK: (f64, f64, f64) = (0.161, 0.161, 0.165);
    // Доступные отрезки — приглушённый изумруд/teal, не режет глаз
    pub const RANGE: (f64, f64, f64) = (0.298, 0.612, 0.514);
    // Засечки и второстепенный текст — нейтральный серый (zinc-500)
    pub const TICK: (f64, f64, f64) = (0.447, 0.447, 0.478);
    // Подписи шкалы — читаемый светло-серый (zinc-400)
    pub const LABEL: (f64, f64, f64) = (0.631, 0.631, 0.667);
    // Ползунок воспроизведения — тёплый акцент (amber-500), хорошо заметен
    pub const PLAYHEAD: (f64, f64, f64) = (0.965, 0.620, 0.043);
}

fn ms_to_datetime_utc(ms: u64) -> Option<DateTime<Utc>> {
    DateTime::from_timestamp_millis(ms as i64)
}

/// Преобразование в локальное время для отображения (учёт timezone пользователя).
fn ms_to_local(ms: u64) -> Option<DateTime<Local>> {
    ms_to_datetime_utc(ms).map(|utc| utc.with_timezone(&Local))
}

/// Форматирование времени для шкалы (короткое), в локальной временной зоне.
fn format_scale(ms: u64) -> String {
    ms_to_local(ms)
        .map(|dt| dt.format("%H:%M").to_string())
        .unwrap_or_else(|| "--:--".to_string())
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

/// Создаёт виджет таймлайна с засечками по минутам, подсказкой времени и навигацией по клику.
pub fn new_timeline(
    state: Arc<crate::app_state::ArchiveState>,
    cmd_tx: mpsc::Sender<ArchiveCommand>,
) -> DrawingArea {
    let area = DrawingArea::new();
    area.set_content_height(88);
    area.set_draw_func({
        let state_draw = Arc::clone(&state);
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
            let span_ms = t_max.saturating_sub(t_min);
            if span_ms == 0 {
                return;
            }

            let track_left = MARGIN;
            let track_width = (w - 2.0 * MARGIN).max(1.0);
            let bar_top = MARGIN + LABEL_AREA_HEIGHT;
            let bar_bottom = bar_top + BAR_HEIGHT;

            // Подпись начала и конца диапазона над полосой
            cr.set_source_rgb(palette::LABEL.0, palette::LABEL.1, palette::LABEL.2);
            cr.select_font_face("Sans", cairo::FontSlant::Normal, cairo::FontWeight::Normal);
            cr.set_font_size(10.0);
            cr.move_to(track_left, MARGIN + 10.0);
            cr.show_text(&format_scale(t_min)).ok();
            let end_text = format_scale(t_max);
            let ext = cr.text_extents(&end_text).ok();
            let end_x = (track_left + track_width - ext.map(|e| e.width()).unwrap_or(0.0)).max(track_left);
            cr.move_to(end_x, MARGIN + 10.0);
            cr.show_text(&end_text).ok();

            // Фон полосы (тёмная канавка)
            cr.set_source_rgb(palette::TRACK.0, palette::TRACK.1, palette::TRACK.2);
            cr.rectangle(track_left, bar_top, track_width, BAR_HEIGHT);
            cr.fill().ok();

            // Засечки по минутам
            let step_min = tick_step_minutes(span_ms);
            let step_ms = step_min * 60 * 1000;
            let first_tick_ms = (t_min / step_ms).saturating_mul(step_ms);
            if step_ms > 0 {
                cr.set_source_rgb(palette::TICK.0, palette::TICK.1, palette::TICK.2);
                cr.set_line_width(1.0);
                cr.select_font_face("Sans", cairo::FontSlant::Normal, cairo::FontWeight::Normal);
                cr.set_font_size(9.0);
                let mut t = first_tick_ms;
                while t <= t_max {
                    let x = timestamp_to_x(t, track_left, track_width, t_min, t_max);
                    if x >= track_left && x <= track_left + track_width {
                        cr.move_to(x, bar_bottom - TICK_HEIGHT_SHORT);
                        cr.line_to(x, bar_bottom);
                        cr.stroke().ok();
                        let label = format_scale(t);
                        let ext = cr.text_extents(&label).ok();
                        let lw = ext.map(|e| e.width()).unwrap_or(0.0);
                        cr.set_source_rgb(palette::LABEL.0, palette::LABEL.1, palette::LABEL.2);
                        cr.move_to((x - lw / 2.0).max(track_left), bar_bottom + 12.0);
                        cr.show_text(&label).ok();
                        cr.set_source_rgb(palette::TICK.0, palette::TICK.1, palette::TICK.2);
                    }
                    t = t.saturating_add(step_ms);
                }
            }

            // Отрезки доступного видео
            for r in &ranges {
                let x0 = timestamp_to_x(r.start_time, track_left, track_width, t_min, t_max);
                let x1 = timestamp_to_x(r.end_time, track_left, track_width, t_min, t_max);
                let seg_w = (x1 - x0).max(1.0);
                cr.set_source_rgb(palette::RANGE.0, palette::RANGE.1, palette::RANGE.2);
                cr.rectangle(x0, bar_top, seg_w, BAR_HEIGHT);
                cr.fill().ok();
            }

            // Ползунок воспроизведения (вертикальная линия + треугольник сверху)
            let start_ms = state_draw.playback_start_ms.load(std::sync::atomic::Ordering::Relaxed);
            let end_ms = state_draw.playback_end_ms.load(std::sync::atomic::Ordering::Relaxed);
            if end_ms > start_ms && start_ms >= t_min && end_ms <= t_max + 1 {
                let px = timestamp_to_x(start_ms, track_left, track_width, t_min, t_max);
                cr.set_source_rgb(palette::PLAYHEAD.0, palette::PLAYHEAD.1, palette::PLAYHEAD.2);
                cr.rectangle(px - PLAYHEAD_WIDTH / 2.0, bar_top, PLAYHEAD_WIDTH, BAR_HEIGHT);
                cr.fill().ok();
                // Треугольник над полосой
                cr.move_to(px, bar_top - PLAYHEAD_TRIANGLE_H);
                cr.line_to(px - 6.0, bar_top);
                cr.line_to(px + 6.0, bar_top);
                cr.close_path();
                cr.fill().ok();
            }
        }
    });

    // Клик по полосе — переход к воспроизведению с этого момента
    let state_click = Arc::clone(&state);
    let area_click = area.clone();
    let click = GestureClick::new();
    click.connect_pressed(move |gesture, _n, x, y| {
        let ranges = state_click.get_ranges();
        if ranges.is_empty() {
            return;
        }
        let (t_min, t_max) = span(ranges.as_slice());
        let span_ms = t_max.saturating_sub(t_min);
        if span_ms == 0 {
            return;
        }
        let w = area_click
            .allocated_width()
            .max(1) as f64;
        let bar_top = MARGIN + LABEL_AREA_HEIGHT;
        let bar_bottom = bar_top + BAR_HEIGHT;
        let track_left = MARGIN;
        let track_width = (w - 2.0 * MARGIN).max(1.0);
        let x_clamped = x.clamp(0.0, w);
        let y = y;
        if y >= bar_top && y <= bar_bottom {
            let local_x = (x_clamped - track_left).clamp(0.0, track_width);
            let timestamp_ms = x_to_timestamp(local_x, track_width, t_min, t_max);
            let _ = cmd_tx.try_send(ArchiveCommand::PlayFrom { timestamp_ms });
            log::info!("Timeline: клик -> воспроизведение с {}", format_tooltip(timestamp_ms));
        }
        gesture.widget().queue_draw();
    });
    area.add_controller(click);

    // Наведение: подсказка с временем и курсор-указатель над полосой
    let state_motion = Arc::clone(&state);
    let motion = EventControllerMotion::new();
    motion.connect_motion(move |controller, x, y| {
        let widget = controller.widget();
        let w = widget.allocated_width().max(1) as f64;
        let bar_top = MARGIN + LABEL_AREA_HEIGHT;
        let bar_bottom = bar_top + BAR_HEIGHT;
        let track_left = MARGIN;
        let track_width = (w - 2.0 * MARGIN).max(1.0);

        if y >= bar_top && y <= bar_bottom && track_width > 0.0 {
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
            let (t_min, t_max) = span(ranges.as_slice());
            let timestamp_ms = x_to_timestamp(local_x, track_width, t_min, t_max);
            widget.set_tooltip_text(Some(&format_tooltip(timestamp_ms)));
            if let Some(ref c) = gtk4::gdk::Cursor::from_name("pointer", None) {
                widget.set_cursor(Some(c));
            } else {
                widget.set_cursor(None);
            }
        } else {
            widget.set_tooltip_text(None);
            if let Some(ref c) = gtk4::gdk::Cursor::from_name("default", None) {
                widget.set_cursor(Some(c));
            } else {
                widget.set_cursor(None);
            }
        }
    });
    motion.connect_leave(move |controller| {
        let widget = controller.widget();
        widget.set_tooltip_text(None);
        if let Some(ref c) = gtk4::gdk::Cursor::from_name("default", None) {
            widget.set_cursor(Some(c));
        } else {
            widget.set_cursor(None);
        }
    });
    area.add_controller(motion);

    // Перерисовка по таймеру и при обновлении ranges
    let area_redraw = area.clone();
    let state_redraw = Arc::clone(&state);
    gtk4::glib::timeout_add_local(std::time::Duration::from_millis(300), move || {
        if state_redraw.timeline_dirty.swap(false, std::sync::atomic::Ordering::Relaxed) {
            area_redraw.queue_draw();
        }
        area_redraw.queue_draw();
        gtk4::glib::ControlFlow::Continue
    });

    area
}
