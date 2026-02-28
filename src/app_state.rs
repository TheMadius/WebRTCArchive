//! Общее состояние приложения: ranges архива и команды для WebRTC-потока.

use crate::archive_protocol::TimeRange;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

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
    /// Начало текущего воспроизводимого фрагмента (мс).
    pub playback_start_ms: AtomicU64,
    /// Конец текущего фрагмента (мс).
    pub playback_end_ms: AtomicU64,
    /// Рекомендательная позиция из RTP (опционально для коррекции дрейфа).
    pub playback_position_ms: AtomicU64,
    /// Момент в контенте (мс), с которого начали воспроизведение (при PlayFrom).
    pub playback_content_start_ms: AtomicU64,
    /// Момент по стенным часам (unix ms), когда начали воспроизведение.
    pub playback_started_at_unix_ms: AtomicU64,
    /// Поколение воспроизведения: увеличивается при каждом PlayFrom. RTP-читатель сбрасывает offset.
    pub playback_generation: AtomicU64,
    /// Timestamp, с которого запросили фрагмент при последнем PlayFrom (чтобы игнорировать устаревшие archive_fragment).
    pub last_play_from_requested_ms: AtomicU64,
    /// Флаг для UI: нужно перерисовать таймлайн (пришли новые ranges).
    pub timeline_dirty: AtomicBool,
}

impl Default for ArchiveState {
    fn default() -> Self {
        Self {
            ranges: std::sync::RwLock::new(Vec::new()),
            playback_start_ms: AtomicU64::new(0),
            playback_end_ms: AtomicU64::new(0),
            playback_position_ms: AtomicU64::new(0),
            playback_content_start_ms: AtomicU64::new(0),
            playback_started_at_unix_ms: AtomicU64::new(0),
            playback_generation: AtomicU64::new(0),
            last_play_from_requested_ms: AtomicU64::new(0),
            timeline_dirty: AtomicBool::new(false),
        }
    }
}

impl ArchiveState {
    /// Увеличивает поколение воспроизведения (вызывать при каждом PlayFrom). RTP-читатель сбрасывает старый offset.
    pub fn next_playback_generation(&self) -> u64 {
        self.playback_generation.fetch_add(1, Ordering::Relaxed) + 1
    }

    pub fn playback_generation(&self) -> u64 {
        self.playback_generation.load(Ordering::Relaxed)
    }

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

    /// Устанавливает рекомендательную позицию из RTP (опционально для отображения/коррекции дрейфа).
    pub fn set_playback_position(&self, position_ms: u64) {
        self.playback_position_ms.store(position_ms, Ordering::Relaxed);
    }

    /// Привязка воспроизведения к стенным часам (вызывать при PlayFrom).
    pub fn set_playback_wall_start(&self, content_start_ms: u64) {
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        self.playback_content_start_ms.store(content_start_ms, Ordering::Relaxed);
        self.playback_started_at_unix_ms.store(now_ms, Ordering::Relaxed);
    }

    /// Сбрасывает привязку к стенным часам (при Stop).
    pub fn clear_playback_wall_start(&self) {
        self.playback_started_at_unix_ms.store(0, Ordering::Relaxed);
    }

    /// Текущая позиция воспроизведения по стенным часам: content_start + (now - started_at), в границах [start, end].
    pub fn current_playback_position_ms(&self) -> u64 {
        let started_at = self.playback_started_at_unix_ms.load(Ordering::Relaxed);
        if started_at == 0 {
            return self.playback_position_ms.load(Ordering::Relaxed);
        }
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let content_start = self.playback_content_start_ms.load(Ordering::Relaxed);
        let start = self.playback_start_ms.load(Ordering::Relaxed);
        let end = self.playback_end_ms.load(Ordering::Relaxed);
        let elapsed = now_ms.saturating_sub(started_at);
        let pos = content_start.saturating_add(elapsed);
        if end > start {
            pos.min(end).max(start)
        } else {
            pos
        }
    }
}
