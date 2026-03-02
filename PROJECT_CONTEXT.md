# webrtc_archive_player — контекст для нового чата

Скопируй блок ниже в новый чат, чтобы продолжить работу над проектом.

---

## Промпт для нового чата

```
Проект: webrtc_archive_player — Rust-приложение с GTK4 для воспроизведения архивного WebRTC-видеопотока (H.264) с SFU-сервера (например ZLMediaKit).

Стек: Rust, gtk4, tokio, webrtc (crate 0.17, локальные форки webrtc-ice и webrtc-data в third_party/), ffmpeg-next (декодирование H.264 → RGB24), serde/json, reqwest, cairo.

Конфиг: config.json — webrtc_url, tls_insecure_skip_verify, ice_servers.

Структура кода:
- main.rs — загрузка конфига, SharedFrame и каналы, запуск WebRTC в отдельном потоке (tokio), GTK Application и MainWindow.
- webrtc_offer.rs — построение SDP offer (H264/H265), on_track для видео: RTP reader → sync_channel(8) → decoder, запись кадра в SharedFrame, try_send в frame_updated (ёмкость 1, без блокировки декодера).
- webrtc_client.rs — POST offer на webrtc_url, разбор answer SDP, добавление ICE-кандидата.
- video_decoder.rs — H264 RTP depacketize → FFmpeg decode (LOW_DELAY, многопоточность) → масштабирование в RGB24. SharedFrame = Arc<Mutex<Option<DecodedFrame>>>.
- ui/mod.rs — MainWindow, DrawingArea для видео (draw_func: копирование кадра под замком, отрисовка вне замка; кадр растягивается под размер области). Таймер перерисовки ~33 ms. video_view (zoom, pan, rotation), timeline.
- ui/timeline.rs — таймлайн архива, клик → PlayFrom.
- archive_loop.rs — при открытии Data Channel: get_ranges; по ответу ranges — обновление состояния; по командам PlayFrom (с key), Stop — drop_buffer, get_archive_fragment, play_stream/stop_stream; следующий фрагмент за 2 сек до конца (фрагмент 10 сек). Сообщения "meta" по DC не логируются и не парсятся (снята нагрузка).
- archive_protocol.rs — типы и JSON для get_ranges, get_archive_fragment, play_stream, stop_stream, drop_buffer и ответов ranges, archive_fragment.
- app_state.rs — ArchiveState (ranges, session_id, playback span, video_view zoom/pan/rotation), ArchiveCommand (PlayFrom, Stop).

Важно: декодирование без привязки к pts/dts; конвейер reader→channel(8)→decoder сохраняет порядок; уведомление UI через try_send(1) чтобы декодер не блокировался. Видео на экране растягивается под размер области (scale_x, scale_y независимо).
```

---

## Краткое описание проекта

**webrtc_archive_player** — десктопный плеер архивного видео по WebRTC.

- Подключается к SFU по URL из `config.json`, отправляет SDP offer, получает answer и по Data Channel запрашивает диапазоны архива (`get_ranges`) и фрагменты (`get_archive_fragment`).
- Видео приходит по RTP (H.264), декодируется в отдельном потоке (FFmpeg → RGB24), последний кадр хранится в `SharedFrame` и рисуется в GTK4 DrawingArea с растягиванием под размер окна (zoom/pan/rotation в `video_view`).
- Таймлайн показывает доступные диапазоны; по клику отправляется PlayFrom → сервер начинает отдавать фрагмент, плеер запрашивает следующий за 2 сек до конца текущего.

Локальные форки в `third_party/`: webrtc-ice (логирование STUN), webrtc-data (приём первого сообщения по DC не-DCEP).
