//! Общее состояние приложения: ranges архива и команды для WebRTC-потока.

use crate::archive_protocol::TimeRange;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// Команды от UI к WebRTC-потоку.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub enum ArchiveCommand {
    /// Запросить ranges за период (мс; None = не задано).
    GetRanges { start_time: Option<u64>, end_time: Option<u64> },
    /// Начать воспроизведение с указанного timestamp (мс).
    PlayFrom { timestamp_ms: u64 },
    /// Остановить воспроизведение.
    Stop,
}

/// Состояние, доступное UI (ranges, текущая позиция для отрисовки).
pub struct ArchiveState {
    pub ranges: std::sync::RwLock<Vec<TimeRange>>,
    /// Текущий воспроизводимый фрагмент: начало (мс). Для отрисовки позиции на timeline.
    pub playback_start_ms: AtomicU64,
    /// Конец текущего фрагмента (мс).
    pub playback_end_ms: AtomicU64,
    /// Флаг для UI: нужно перерисовать таймлайн (пришли новые ranges).
    pub timeline_dirty: AtomicBool,
}

impl Default for ArchiveState {
    fn default() -> Self {
        Self {
            ranges: std::sync::RwLock::new(Vec::new()),
            playback_start_ms: AtomicU64::new(0),
            playback_end_ms: AtomicU64::new(0),
            timeline_dirty: AtomicBool::new(false),
        }
    }
}

impl ArchiveState {
    pub fn set_ranges(&self, ranges: Vec<TimeRange>) {
        let n = ranges.len();
        if let Ok(mut w) = self.ranges.write() {
            *w = ranges;
            log::info!("[state] set_ranges: {} range(s) written", n);
        } else {
            log::warn!("[state] set_ranges: failed to lock ranges for write");
        }
        self.timeline_dirty.store(true, Ordering::Relaxed);
    }

    pub fn get_ranges(&self) -> Vec<TimeRange> {
        self.ranges.read().map(|r| r.clone()).unwrap_or_default()
    }

    pub fn set_playback_span(&self, start_ms: u64, end_ms: u64) {
        self.playback_start_ms.store(start_ms, Ordering::Relaxed);
        self.playback_end_ms.store(end_ms, Ordering::Relaxed);
    }
}
