mod timeline;

use crate::app_state::ArchiveState;
use crate::config::AppConfig;
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
    ) -> Self {
        let window = ApplicationWindow::new(app);
        window.set_title(Some("WebRTC Archive Player"));
        window.set_default_size(1280, 720);

        let root = GtkBox::new(Orientation::Vertical, 4);

        let video_area = DrawingArea::new();
        video_area.set_content_width(1280);
        video_area.set_content_height(640);

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

