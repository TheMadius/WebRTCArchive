mod timeline;

use crate::config::AppConfig;
use gtk4::prelude::*;
use gtk4::{Application, ApplicationWindow, Box as GtkBox, DrawingArea, Orientation};

pub struct MainWindow {
    window: ApplicationWindow,
}

impl MainWindow {
    pub fn new(app: &Application, _config: AppConfig) -> Self {
        let window = ApplicationWindow::new(app);
        window.set_title(Some("WebRTC Archive Player"));
        window.set_default_size(1280, 720);

        let root = GtkBox::new(Orientation::Vertical, 4);

        let video_area = DrawingArea::new();
        video_area.set_content_width(1280);
        video_area.set_content_height(640);

        let timeline = timeline::TimelineWidget::new();

        root.append(&video_area);
        root.append(&timeline);

        window.set_child(Some(&root));

        Self { window }
    }

    pub fn present(&self) {
        self.window.present();
    }
}

