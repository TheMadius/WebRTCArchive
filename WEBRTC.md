# Документация WebRTC EVI RTP

# Документация WebRTC Live

### Назначение
**WebRTC Live** — это компонент для воспроизведения live трансляции через WebRTC протокол с поддержкой передачи аналитических метаданных через Data Channel.

### Основные возможности
- Потоковое воспроизведение видеоархива по WebRTC (RTP/RTCP)
- Поддержка кодеков H.264 и H.265
- Передача метаданных по SCTP Data Channel

## 1. Транспортный уровень и инициализация

### 1.1 Формат подключения

**POST запрос:**
```
https://<host>:<port>/webrtc?type=play&stream=<stream_id>&app=<stream_app>
```

**Параметры подключения:**
- `type=play`          — тип подключения (трансляция видео)
- `stream=<stream_id>` — uuid потока
- `app=<stream_app`    — тип потока (live, sub)

Для получения мета данных необходимо открыть datachannel

### 1.2 WebRTC инициализация

После POST запроса инициируется стандартный WebRTC handshake:

1. **Обмен SDP** (Session Description Protocol)
   - Клиент отправляет SDP offer
   - Сервер генерирует SDP answer с параметрами:
     - `RtpDirection::sendonly` — только передача видео со стороны сервера
     - `multi codec: true`      — поддержка нескольких кодеков одновременно
     - Предпочитаемые кодеки: H.264, H.265

2. **ICE сборка** (Interactive Connectivity Establishment)
  - Так общение между клиентом и SFU то ICE кандидаты для преодоления NAT не требуется

3. **DTLS handshake** (Datagram TLS)
   - Установление безопасного соединения для SRTP
   - Согласование криптографических параметров

---

## 2. Метаданные

### Аналитические метаданные

Передаются синхронно с видеокадрами:

```json
{
  "type":"meta",
  "data": {
    "faceModel": {
      "objects": [],
      "lines": [],
      "directionZones": [],
      "detectionZones": []
    },
    "plateModel": {
      "objects": [],
      "lines": [],
      "directionZones": [],
      "detectionZones": []
    },
    "objectModel": {
      "objects": [
        {
          "type": "objectModel",
          "detection": true,
          "classes": "humans_line",
          "id": null,
          "personType": null,
          "matchAccuracy": 0.49364590644836426,
          "triggered": true,
          "points": {
            "x": 0.00038580247201025486,
            "y": 0.19341564178466797,
            "w": 0.1454475373029709,
            "h": 0.16872428357601166
          }
        }
      ],
      "lines": [
        {
          "id": "ba05e762-a7ff-4ed7-b3e1-7bc62d03fffa",
          "name": "Линия 1",
          "crossDirection": "ABBA",
          "pointStart": {
            "x": 0.025730994152046785,
            "y": 0.9883177570093458
          },
          "pointEnd": {
            "x": 0.06315789473684211,
            "y": 0.004672897196261682
          }
        }
      ]
    }
  }
}
```

**Структура данных**:

| Поле | Описание |
|------|----------|
| `faceModel` | мета с аналитики распознавания лиц|
| `plateModel` | мета с аналитики распознавания государственых номеров|
| `objectModel` | мета с аналитики детекции объектов|
| `motionModel` | мета с детектора движения |
---


# Документация WebRTC Archive

## Общая информация

**WebRTC Archive** — это компонент для потокового воспроизведения архивированного видео через WebRTC протокол с поддержкой:
- Воспроизведения видеоархива по WebRTC (RTP/RTCP с SRTP шифрованием)
- Синхронной передачи аналитических метаданных через SCTP Data Channel
- Управления процессом воспроизведения (play, stop, pause, speed control)
- Кодеков H.264 и H.265
- Мультиплексирования нескольких кодеков в одной сессии

---

## 1. Транспортный уровень и инициализация

### 1.1 Формат подключения

**POST запрос:**
```
https://<host>:<port>/webrtc?type=archive&stream=<stream_id>&app=<stream_app>
```

**Параметры подключения:**
- `type=archive`          — тип подключения (архивное видео)
- `stream=<stream_id>` — uuid потока
- `app=<stream_app`    — тип потока (live, sub)

Для получения метаданных и отправки команд необходимо открыть datachannel

### 1.2 WebRTC инициализация

После POST запроса инициируется стандартный WebRTC handshake:

1. **Обмен SDP** (Session Description Protocol)
   - Клиент отправляет SDP offer
   - Сервер генерирует SDP answer с параметрами:
     - `RtpDirection::sendonly` — только передача видео со стороны сервера
     - `multi codec: true`      — поддержка нескольких кодеков одновременно
     - Предпочитаемые кодеки: H.264, H.265

2. **ICE сборка** (Interactive Connectivity Establishment)
  - Так общение между клиентом и SFU то ICE кандидаты для преодоления NAT не требуется

3. **DTLS handshake** (Datagram TLS)
   - Установление безопасного соединения для SRTP
   - Согласование криптографических параметров

---

## 2. Протокол управления

### 2.1 Общая структура сообщений

**Запросы от клиента (через Data Channel):**
```json
{
  "type": "command_type",
  "data": {
    "param1": "value1",
    "param2": "value2"
  }
}
```

**Ответы от сервера:**
```json
{
  "type": "response_type",
  "data": {
    "result": "..."
  }
}
```

**Ответ при ошибке:**
```json
{
  "error": "Error description"
}
```

### 2.2 Маппинг команд

| Внутреннее имя | Сокращённая форма | Полная команда | Обработчик |
|---|---|---|---|
| GetRanges | `r` | `get_ranges` | `GetRanges(const nlohmann::json &body)` |
| GetMetaRanges | `m` | `get_meta_ranges` | `GetMetaRanges(const nlohmann::json &body)` |
| PlayArchive | `p` | `play_stream` | `PlayArchive(const nlohmann::json &body)` |
| StopArchive | `s` | `stop_stream` | `StopArchive(const nlohmann::json &body)` |
| DropBuffer | `d` | `drop_buffer` | `DropBuffer(const nlohmann::json &body)` |
| SetSpeed | `S` | `set_speed` | `SetSpeed(const nlohmann::json &body)` |
| KeyFragment | `k` | `get_key` | `KeyFragment(const nlohmann::json &body)` |
| AddFragment | `a` | `get_archive_fragment` | `AddFragment(const nlohmann::json &body)` |
| GetUrl | — | `get_url` | `GetUrl(const nlohmann::json &body)` |
| Ping | — | `archive_connect_support` | (ignored) |

---

## 3. Все типы сигнальных сообщений

### 3.1 GetRanges — Получение доступных временных диапазонов видео

**Назначение:** Получить список периодов времени, когда в архиве есть видеозаписи.

**Запрос:**
```json
{
  "type": "get_ranges",
  "data": {
    "start_time": 1704067200000,
    "end_time": 1704153600000
  }
}
```

| Поле | Тип | Обязательное | Описание |
|------|-----|:---:|----------|
| `start_time` | uint64_t | ✗ | Начало периода поиска (мс с 1970-01-01, Unix timestamp × 1000) |
| `end_time` | uint64_t | ✗ | Конец периода поиска |

**Ответ (успех):**
```json
{
  "type": "ranges",
  "data": {
    "ranges": [
      {
        "start_time": 1704067200000,
        "end_time": 1704070800000,
        "duration": 3600000
      },
      {
        "start_time": 1704074400000,
        "end_time": 1704078000000,
        "duration": 3600000
      }
    ]
  }
}
```

**Ответ (ошибка):**
```json
{
  "error": "Not Found ranges"
}
```
---

### 3.2 GetMetaRanges — Получение диапазонов метаданных (устарел)

**Назначение:** Получить периоды доступности аналитических метаданных (распознавание лиц, номеров и т.п.).

**Запрос:**
```json
{
  "type": "get_meta_ranges",
  "data": {
    "start_time": 1704067200000,
    "end_time": 1704153600000,
    "codec": "md",
    "live": false
  }
}
```

| Поле | Тип | Обязательное | Описание |
|------|-----|:---:|----------|
| `start_time` | uint64_t | ✗ | Начало периода поиска |
| `end_time` | uint64_t | ✗ | Конец периода поиска |
| `codec` | string | ✗ | Тип метаданных (по умолчанию `"md"`) |
| `live` | bool | ✗ | Фильтр активных диапазонов |

**Ответ (успех):**
```json
{
  "type": "meta_ranges",
  "data": {
    "ranges": [
      {
        "start_time": 1704067200000,
        "end_time": 1704070800000,
        "duration": 3600000
      }
    ]
  }
}
```

**Ответ (ошибка):**
```json
{
  "error": "Not Found frame"
}
```

---

### 3.3 AddFragment — Загрузка видеофрагмента с аналитикой

**Назначение:** Загрузить видеофрагмент в буфер вместе со всеми связанными аналитическими метаданными.

**Запрос:**
```json
{
  "type": "get_archive_fragment",
  "data": {
    "start_time": 1704067200000,
    "duration": 60000,
    "key": true,
    "type_meta": ""
  }
}
```

| Поле | Тип | Обязательное | Описание |
|------|-----|:---:|----------|
| `start_time` | uint64_t | ✓ | Начало фрагмента (мс) |
| `duration` | int64_t | ✓ | Длительность фрагмента (мс) |
| `key` | bool | ✗ | Загрузить ключевой I-frame перед фрагментом (по умолчанию false) |
| `type_meta` | string | ✗ | Тип метаданных для фильтрации |

**Ответ (успех):**
```json
{
  "type": "archive_fragment",
  "data": {
    "start_time": 1704067200000,
    "end_time": 1704067260000,
    "duration": 60000
  }
}
```

**Ответ (ошибка):**
```json
{
  "error": "Not Found frame"
}
```

или

```json
{
  "error": "duration <= 0"
}
```

**Временная синхронизация:**

Архивные кадры хранятся с временем, отсчитываемым от начала дня UTC. Для воспроизведения через WebRTC требуется коррекция:

---

### 3.4 KeyFragment — Получение ключевого кадра

**Назначение:** Загрузить одиночный ключевой I-frame для быстрого переключения или получения контекста декодера.

**Запрос:**
```json
{
  "type": "get_key",
  "data": {
    "start_time": 1704067200000
  }
}
```

| Поле | Тип | Обязательное | Описание |
|------|-----|:---:|----------|
| `start_time` | int64_t | ✓ | Время, к которому найти ближайший ключевой кадр |

**Ответ (успех):**
```json
{
  "type": "key_fragment"
}
```

**Ответ (ошибка):**
```json
{
  "error": "Not Found frame"
}
```

---

### 3.5 PlayArchive — Начать воспроизведение

**Назначение:** Включить отправку ранее загруженных видеокадров в WebRTC.

**Запрос:**
```json
{
  "type": "play_stream",
  "data": {}
}
```

**Ответ:**
```json
{
  "type": "play"
}
```

---

### 3.6 StopArchive — Остановка воспроизведения

**Назначение:** Остановить отправку видеокадров, но оставить буфер нетронутым.

**Запрос:**
```json
{
  "type": "stop_stream",
  "data": {}
}
```

**Ответ:**
```json
{
  "type": "stop"
}
```

---

### 3.7 DropBuffer — Очистка буфера

**Назначение:** Остановить воспроизведение и полностью очистить кэш видеокадров для загрузки нового фрагмента.

**Запрос:**
```json
{
  "type": "drop_buffer",
  "data": {}
}
```

**Ответ:**
```json
{
  "type": "drop"
}
```

---

### 3.8 SetSpeed — Управление скоростью воспроизведения

**Назначение:** Установить множитель скорости воспроизведения.

**Запрос:**
```json
{
  "type": "set_speed",
  "data": {
    "speed": 2.0
  }
}
```

| Поле | Тип | Обязательное | Описание |
|------|-----|:---:|----------|
| `speed` | double | ✓ | Множитель скорости (0.5, 1.0, 2.0, 4.0, и т.д.) |

**Ответ:**
```json
{
  "type": "speed"
}
```

---

### 3.9 GetUrl — Получение URL для загрузки

**Назначение:** Получить HTTP ссылку для прямого скачивания видеофрагмента (без WebRTC).

**Запрос:**
```json
{
  "type": "get_url",
  "data": {
    "start_time": 1704067200000,
    "duration": 60000
  }
}
```

| Поле | Тип | Обязательное | Описание |
|------|-----|:---:|----------|
| `start_time` | int64_t | ✓ | Начало фрагмента (мс) |
| `duration` | int64_t | ✓ | Длительность (мс) |

**Ответ:**
```json
{
  "type": "url",
  "data": {
    "url": "https://host:8443/archive/camera_1/download?timestamp=1704067200000&duration=60000"
  }
}
```

---

### 3.10 Ping — Проверка доступности

**Назначение:** Проверить, что архив доступен и готов к работе.

**Запрос:**
```json
{
  "type": "archive_connect_support"
}
```

**Ответ:** Ответ не отправляется (команда игнорируется). Нужен для поддержки соединения, так как при отсутствии пакетов сервер отключает клиента

---

## 4. Сценарии использования

### 4.1 Подключение к архиву и просмотр

**Последовательность действий:**

```
1. WebRTC подключение
   POST /webrtc?type=archive
   → SDP offer/answer
   → DTLS handshake
   → Готово к командам

2. Получить доступное видео
   get_ranges { start_time, end_time }
   ← ranges { data: { ranges: [...] } }

3. Загрузить видеофрагмент в буфер
3.1 Загрузить видеофрагмента с ключевым кадром
   get_archive_fragment { start_time, duration, key }
   ← archive_fragment { data: { start_time, end_time, duration } }

4. Начать воспроизведение
   play_stream {}
   ← play {}
   [Начинают приходить видеокадры через RTP]
   [Начинают приходить метаданные через Data Channel]

5. Управление во время воспроизведения
   set_speed { speed: 2.0 }  // Ускорение
   ← speed {}

   stop_stream {}             // Пауза
   ← stop {}

   play_stream {}             // Возобновление
   ← play {}

6. Загрузить другой фрагмент
   drop_buffer {}             // Очистить буфер
   ← drop {}

   get_archive_fragment { ... } // Загрузить новый
   ← archive_fragment { ... }

   play_stream {}             // Воспроизвести
   ← play {}
```

### 4.2 Быстрая перемотка по архиву

```
1. Остановить текущее воспроизведение
   stop_stream {}

2. Очистить буфер
   drop_buffer {}

3. Загрузить фрагмент начиная с целевого времени
   get_key { start_time: target_time }
   ← key_fragment {}

   get_archive_fragment {
       start_time: target_time,
       duration: 300000  // 5 минут
   }
   ← archive_fragment {}

4. Возобновить воспроизведение
   play_stream {}
```

---

## 5. Описание особенностей реализации

### 5.1 Поддерживаемые кодеки и форматы

**Видеокодеки:**
- H.264 (AVC) — наиболее совместимый
- H.265 (HEVC) — более эффективный

**Типы метаданных (кодеки):**
- `CodecMD`        — детектор движения
- `CodecAnalytic`  — базовая аналитика
- `CodecFace`      — распознавание лиц
- `CodecPlate`     — распознавание номеров
- `CodecLine`      — триггеры на линии
- `CodecZoneEnter` — события входа в зоны
- `CodecZoneExit`  — события выхода из зон

### 5.4 Синхронизация времени между клиентом и сервером

**Проблема:** Архивные кадры хранятся с временем от начала дня, а WebRTC использует RTP timestamp в мс от начала сессии.

**Алгоритм синхронизации:**

```
Архивный timestamp = 1704067200000 (2024-01-01 00:00:00)

1. Получить дату из timestamp
   date = 2024-01-01

2. Вычислить начало дня (UTC)
   day_start = timestamp("2024-01-01", "00:00:00") = 1704067200000

3. Вычислить relative PTS (смещение от начала дня)
   pts = 1704067200000 - 1704067200000 = 0

4. При воспроизведении все кадры синхронизированы относительно дня
   Это позволяет плавное воспроизведение нескольких дней подряд
```

---

## 6. Обработка ошибок и восстановление соединения

### 6.1 Обработка ошибок в Data Channel

Все ошибки отправляются в единообразном формате:

```json
{
  "error": "Описание ошибки"
}
```

### 6.2 Частые ошибки

| Ошибка | Причина | Решение |
|--------|---------|---------|
| `"Not Found ranges"` | Нет видео в архиве за заданный период | Проверить start_time/end_time |
| `"Not Found frame"` | Видеофрагмент не найден или повреждён | Попробовать соседнее время |
| `"duration <= 0"` | Неверная длительность фрагмента | duration должна быть > 0 |
| `"Not Found type"` | Неизвестная команда | Проверить тип команды |
| RTCP BYE | Удалённый сервер закрыл соединение | Переподключиться |

### 6.3 Восстановление соединения

**Стратегия:**

```javascript
const reconnect = async (maxRetries = 3) => {
    for (let i = 0; i < maxRetries; i++) {
        try {
            // 1. Закрыть старое соединение
            if (pc.connectionState !== 'closed') {
                pc.close();
            }

            // 2. Создать новое
            pc = new RTCPeerConnection();
            await initializePeerConnection();

            // 3. Переподключиться к архиву
            const response = await fetch('https://host:8443/webrtc?type=archive', {
                method: 'POST',
                body: JSON.stringify({ sdp: pc.localDescription })
            });

            if (response.ok) {
                const answer = await response.json();
                pc.setRemoteDescription(new RTCSessionDescription(answer));
                return true;
            }
        } catch (error) {
            console.error(`Reconnection attempt ${i + 1} failed:`, error);
            await new Promise(r => setTimeout(r, 1000 * (i + 1)));  // Exponential backoff
        }
    }
    return false;
};

pc.onconnectionstatechange = () => {
    if (pc.connectionState === 'disconnected' ||
        pc.connectionState === 'failed' ||
        pc.connectionState === 'closed') {
        reconnect();
    }
};
```

### 6.4 Контроль тайм-ауты Data Channel

```cpp
// Клиентская сторона может отправлять Ping для проверки живости
setInterval(() => {
    dataChannel.send(JSON.stringify({
        type: "archive_connect_support"
    }));
}, 30000);  // Каждые 30 секунд
```

---

## 7. Состояния и переходы

```
┌─────────────────────┐
│  Подключение        │
│  WebRTC/DTLS        │
└──────────┬──────────┘
           │
           ▼
┌─────────────────────┐
│  Ожидание команд    │◄─────────┐
│  (Idle)             │          │
└──────────┬──────────┘          │
           │ get_ranges          │ drop_buffer
           │ get_meta_ranges     │ or stop_stream
           │ get_key             │ or stall
           │ get_archive_fragment│
           ▼
┌─────────────────────┐
│  Загрузка в буфер   │
│  (Loading)          │
└──────────┬──────────┘
           │ play_stream
           ▼
┌─────────────────────┐
│  Воспроизведение    │
│  (Playing)          │──┐
└──────────┬──────────┘  │
           │ stop_stream │ set_speed
           │ drop_buffer │
           ▼             │
┌─────────────────────┐  │
│  Пауза              │◄─┘
│  (Paused)           │
└──────────┬──────────┘
           │ play_stream
           └─────────────┘
```

---

## 8. Примеры HTTP запросов/ответов

### Пример 1: Полный сценарий просмотра

```bash
# 1. Подключение к WebRTC (WebSocket upgrade -> DTLS -> SRTP)
POST https://localhost:8443/webrtc?type=archive
Content-Type: application/json

"v=0\no=- ..."

HTTP/1.1 200 OK
Content-Type: application/json

{
  "sdp": "v=0\no=- ..."
}

# 2-10. Все остальные команды идут через SCTP Data Channel (не HTTP)

# Клиент отправляет через DataChannel:
{
  "type": "get_ranges",
  "data": {
    "start_time": 1704067200000,
    "end_time": 1704153600000
  }
}

# Сервер отвечает через DataChannel:
{
  "type": "ranges",
  "data": {
    "ranges": [...]
  }
}
```

### Пример 2: GetUrl для скачивания

```bash
# Через DataChannel:
{
  "type": "get_url",
  "data": {
    "start_time": 1704067200000,
    "duration": 60000
  }
}

# Ответ:
{
  "type": "url",
  "data": {
    "url": "https://localhost:8443/archive/camera_1/download?timestamp=1704067200000&duration=60000"
  }
}

# Затем клиент может скачать видеофайл обычным HTTP:
GET https://localhost:8443/archive/camera_1/download?timestamp=1704067200000&duration=60000
HTTP/1.1 200 OK
Content-Type: video/mp4
Content-Disposition: attachment; filename="camera_1_2024-01-01_000000.mp4"

[бинарные данные видеофайла]
```

---