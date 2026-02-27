use gtk4::prelude::*;
use gtk4::{DrawingArea, GestureClick};

#[derive(Clone)]
pub struct TimelineWidget {
    area: DrawingArea,
}

impl TimelineWidget {
    pub fn new() -> DrawingArea {
        let area = DrawingArea::new();
        area.set_content_height(60);

        area.set_draw_func(|_, cr, width, height| {
            // фон
            cr.set_source_rgb(0.1, 0.1, 0.1);
            cr.rectangle(0.0, 0.0, width as f64, height as f64);
            cr.fill().ok();

            // пример: закрашенный сегмент архива в середине
            cr.set_source_rgb(0.2, 0.7, 0.2);
            let margin = 10.0;
            let bar_height = height as f64 - 2.0 * margin;
            cr.rectangle(
                width as f64 * 0.25,
                margin,
                width as f64 * 0.5,
                bar_height,
            );
            cr.fill().ok();
        });

        let click = GestureClick::new();
        click.connect_pressed(move |_gesture, _n, x, _y| {
            log::info!("Timeline clicked at x={}", x);
            // здесь позже будет отправка команд get_key/get_archive_fragment/play_stream
        });
        area.add_controller(click);

        area
    }
}

