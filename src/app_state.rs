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
    /// Остановить воспроизведение (полная остановка).
    Stop,
    /// Пауза: stop_stream, позиция сохраняется.
    Pause,
    /// Возобновление: play_stream.
    Play,
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
    /// Если != 0: пауза, unix ms момента постановки на паузу (позиция замораживается).
    pub playback_paused_at_unix_ms: AtomicU64,
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
            playback_paused_at_unix_ms: AtomicU64::new(0),
            last_play_from_requested_ms: AtomicU64::new(0),
            timeline_dirty: AtomicBool::new(false),
        }
    }
}

/// Хранит f64 как биты в AtomicU64 для атомарного доступа без потери точности панорамы.
fn f64_to_bits(v: f64) -> u64 {
    v.to_bits()
}
fn f64_from_bits(b: u64) -> f64 {
    f64::from_bits(b)
}

/// Параметры отображения видео: зум, панорама, поворот (управление мышью над видео).
pub struct VideoViewState {
    zoom_percent: std::sync::atomic::AtomicU32,
    /// Поворот в градусах, шаг 5 (0, 5, 10, ... 355).
    rotation_deg: std::sync::atomic::AtomicU32,
    /// Смещение отрисовки в пикселях (перетаскивание), хранится в f64 для точного края.
    pan_x: AtomicU64,
    pan_y: AtomicU64,
}

impl VideoViewState {
    pub fn new() -> Self {
        Self {
            zoom_percent: std::sync::atomic::AtomicU32::new(100),
            rotation_deg: std::sync::atomic::AtomicU32::new(0),
            pan_x: AtomicU64::new(f64_to_bits(0.0)),
            pan_y: AtomicU64::new(f64_to_bits(0.0)),
        }
    }

    pub fn zoom_percent(&self) -> u32 {
        self.zoom_percent.load(Ordering::Relaxed)
    }

    /// Минимальный зум = 100% (максимальное отдаление = изначальный вид), шаг 5%.
    pub fn set_zoom_percent(&self, p: u32) {
        self.zoom_percent.store(p.clamp(100, 400), Ordering::Relaxed);
    }

    const ZOOM_STEP_PERCENT: u32 = 5;

    /// Зум колёсиком вверх (только когда курсор над видео).
    pub fn zoom_in(&self) {
        let p = self.zoom_percent.load(Ordering::Relaxed);
        self.set_zoom_percent(p.saturating_add(Self::ZOOM_STEP_PERCENT));
    }

    /// Зум колёсиком вниз; не отдаляем больше 100% (исходный кадр по размеру области).
    pub fn zoom_out(&self) {
        let p = self.zoom_percent.load(Ordering::Relaxed);
        self.set_zoom_percent(p.saturating_sub(Self::ZOOM_STEP_PERCENT));
    }

    pub fn rotation_deg(&self) -> u32 {
        self.rotation_deg.load(Ordering::Relaxed)
    }

    /// Поворот на 5° по часовой (Shift + колёсико вверх).
    pub fn rotate_5_cw(&self) {
        let r = self.rotation_deg.load(Ordering::Relaxed);
        self.rotation_deg.store((r + 5) % 360, Ordering::Relaxed);
    }

    /// Поворот на 5° против часовой (Shift + колёсико вниз).
    pub fn rotate_5_ccw(&self) {
        let r = self.rotation_deg.load(Ordering::Relaxed);
        self.rotation_deg.store((r + 355) % 360, Ordering::Relaxed);
    }

    /// Текущее смещение по X (пиксели), для отрисовки.
    pub fn pan_x(&self) -> f64 {
        f64_from_bits(self.pan_x.load(Ordering::Relaxed))
    }

    /// Текущее смещение по Y (пиксели), для отрисовки.
    pub fn pan_y(&self) -> f64 {
        f64_from_bits(self.pan_y.load(Ordering::Relaxed))
    }

    /// Смещение при перетаскивании (только когда курсор над видео).
    pub fn add_pan(&self, dx: f64, dy: f64) {
        let px = f64_from_bits(self.pan_x.load(Ordering::Relaxed));
        let py = f64_from_bits(self.pan_y.load(Ordering::Relaxed));
        self.pan_x.store(f64_to_bits(px + dx), Ordering::Relaxed);
        self.pan_y.store(f64_to_bits(py + dy), Ordering::Relaxed);
    }

    /// Ограничивает панораму так, чтобы не показывать пустоту за пределами кадра.
    /// Вызывать из отрисовки после вычисления drawn_w, drawn_h. Хранит pan в f64 — до края можно довести точно.
    pub fn clamp_pan_to_frame(&self, area_w: f64, area_h: f64, drawn_w: f64, drawn_h: f64) {
        let pan_x_max = if drawn_w >= area_w {
            (drawn_w - area_w) / 2.0
        } else {
            0.0
        };
        let pan_x_min = -pan_x_max;
        let pan_y_max = if drawn_h >= area_h {
            (drawn_h - area_h) / 2.0
        } else {
            0.0
        };
        let pan_y_min = -pan_y_max;
        let px = f64_from_bits(self.pan_x.load(Ordering::Relaxed));
        let py = f64_from_bits(self.pan_y.load(Ordering::Relaxed));
        let px = px.clamp(pan_x_min, pan_x_max);
        let py = py.clamp(pan_y_min, pan_y_max);
        self.pan_x.store(f64_to_bits(px), Ordering::Relaxed);
        self.pan_y.store(f64_to_bits(py), Ordering::Relaxed);
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
        self.playback_paused_at_unix_ms.store(0, Ordering::Relaxed);
    }

    /// Ставит воспроизведение на паузу (останавливает таймлайн, позиция сохраняется).
    pub fn set_playback_paused(&self) {
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        self.playback_paused_at_unix_ms.store(now_ms, Ordering::Relaxed);
    }

    /// Снимает с паузы (возобновляет таймлайн с той же позиции).
    /// Если воспроизведение было остановлено (started_at == 0), заново привязывает позицию к стенным часам.
    pub fn set_playback_resumed(&self) {
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let started_at = self.playback_started_at_unix_ms.load(Ordering::Relaxed);
        let paused_at = self.playback_paused_at_unix_ms.load(Ordering::Relaxed);

        if paused_at != 0 {
            let elapsed_at_pause = paused_at.saturating_sub(started_at);
            self.playback_started_at_unix_ms
                .store(now_ms.saturating_sub(elapsed_at_pause), Ordering::Relaxed);
            self.playback_paused_at_unix_ms.store(0, Ordering::Relaxed);
            return;
        }
        // После Stop воспроизведение не в паузе, но started_at сброшен — заново привязываем к часам.
        if started_at == 0 {
            let pos = self.current_playback_position_ms();
            self.playback_content_start_ms.store(pos, Ordering::Relaxed);
            self.playback_started_at_unix_ms.store(now_ms, Ordering::Relaxed);
        }
    }

    pub fn is_paused(&self) -> bool {
        self.playback_paused_at_unix_ms.load(Ordering::Relaxed) != 0
    }

    /// Текущая позиция воспроизведения по стенным часам (или замороженная при паузе).
    pub fn current_playback_position_ms(&self) -> u64 {
        let started_at = self.playback_started_at_unix_ms.load(Ordering::Relaxed);
        if started_at == 0 {
            return self.playback_position_ms.load(Ordering::Relaxed);
        }
        let content_start = self.playback_content_start_ms.load(Ordering::Relaxed);
        let start = self.playback_start_ms.load(Ordering::Relaxed);
        let end = self.playback_end_ms.load(Ordering::Relaxed);
        let paused_at = self.playback_paused_at_unix_ms.load(Ordering::Relaxed);
        let elapsed = if paused_at != 0 {
            paused_at.saturating_sub(started_at)
        } else {
            let now_ms = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);
            now_ms.saturating_sub(started_at)
        };
        let pos = content_start.saturating_add(elapsed);
        if end > start {
            pos.min(end).max(start)
        } else {
            pos
        }
    }
}
