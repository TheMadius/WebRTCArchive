//! Таймлайн архива: отрисовка ranges в реальном времени, клик -> timestamp -> PlayFrom.

use crate::app_state::ArchiveCommand;
use crate::archive_protocol::TimeRange;
use gtk4::prelude::*;
use gtk4::{DrawingArea, GestureClick};
use std::sync::Arc;
use tokio::sync::mpsc;

/// Создаёт виджет таймлайна: рисует ranges из state, по клику отправляет PlayFrom(timestamp_ms).
pub fn new_timeline(
    state: Arc<crate::app_state::ArchiveState>,
    cmd_tx: mpsc::Sender<ArchiveCommand>,
) -> DrawingArea {
    let area = DrawingArea::new();
    area.set_content_height(60);

    let state_draw = Arc::clone(&state);
    area.set_draw_func(move |_, cr, width, height| {
        cr.set_source_rgb(0.12, 0.12, 0.14);
        cr.rectangle(0.0, 0.0, width as f64, height as f64);
        cr.fill().ok();

        let ranges = state_draw.get_ranges();
        if ranges.is_empty() {
            log::debug!("[timeline] draw: 0 ranges");
            return;
        }
        log::info!("[timeline] draw: {} range(s), width={} height={}", ranges.len(), width, height);

        let (t_min, t_max) = span(ranges.as_slice());
        let span_ms = t_max.saturating_sub(t_min);
        if span_ms == 0 {
            return;
        }

        let margin = 8.0;
        let bar_height = height as f64 - 2.0 * margin;
        let w = width as f64;

        for r in &ranges {
            let x0 = margin + (r.start_time.saturating_sub(t_min)) as f64 / span_ms as f64 * (w - 2.0 * margin);
            let x1 = margin + (r.end_time.saturating_sub(t_min)) as f64 / span_ms as f64 * (w - 2.0 * margin);
            let seg_w = (x1 - x0).max(1.0);
            cr.set_source_rgb(0.2, 0.65, 0.35);
            cr.rectangle(x0, margin, seg_w, bar_height);
            cr.fill().ok();
        }

        let start_ms = state_draw.playback_start_ms.load(std::sync::atomic::Ordering::Relaxed);
        let end_ms = state_draw.playback_end_ms.load(std::sync::atomic::Ordering::Relaxed);
        if end_ms > start_ms && start_ms >= t_min && end_ms <= t_max + 1 {
            let px = margin + (start_ms.saturating_sub(t_min)) as f64 / span_ms as f64 * (w - 2.0 * margin);
            cr.set_source_rgb(1.0, 0.85, 0.2);
            cr.rectangle(px, margin, 3.0, bar_height);
            cr.fill().ok();
        }
    });

    let state_click = Arc::clone(&state);
    let click = GestureClick::new();
    click.connect_pressed(move |gesture, _n, x, _y| {
        let ranges = state_click.get_ranges();
        if ranges.is_empty() {
            log::warn!("Timeline: no ranges yet, click ignored");
            return;
        }
        let (t_min, t_max) = span(ranges.as_slice());
        let span_ms = t_max.saturating_sub(t_min);
        if span_ms == 0 {
            return;
        }
        let w = gesture
            .widget()
            .downcast::<DrawingArea>()
            .ok()
            .map(|a| a.allocated_width().max(1) as f64)
            .unwrap_or(800.0);
        let margin = 8.0;
        let x = x.clamp(0.0, w);
        let t = t_min as f64 + (x - margin) / (w - 2.0 * margin).max(1.0) * span_ms as f64;
        let timestamp_ms = t.round() as u64;
        let _ = cmd_tx.try_send(ArchiveCommand::PlayFrom {
            timestamp_ms,
        });
        log::info!("Timeline click -> PlayFrom {} ms", timestamp_ms);
        gesture.widget().queue_draw();
    });
    area.add_controller(click);

    // Перерисовка по таймеру и при state.timeline_dirty (сразу после прихода ranges).
    let area_redraw = area.clone();
    let state_redraw = Arc::clone(&state);
    gtk4::glib::timeout_add_local(std::time::Duration::from_millis(300), move || {
        if state_redraw.timeline_dirty.swap(false, std::sync::atomic::Ordering::Relaxed) {
            log::info!("[timeline] dirty flag set, queue_draw");
            area_redraw.queue_draw();
        }
        area_redraw.queue_draw();
        gtk4::glib::ControlFlow::Continue
    });

    area
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
