use serde::{Deserialize, Serialize};

const DEFAULT_URL: &str = "https://10.24.88.31:7200/webrtc?app=live&stream=dca117f0-95d4-47e0-bb93-0681e85dbd0b&type=archive";
const CONFIG_FILE: &str = "config.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    pub webrtc_url: String,
    /// Если сервер использует self-signed сертификат, можно отключить проверку TLS.
    /// В продакшене лучше добавить свой CA и включить валидацию.
    #[serde(default)]
    pub tls_insecure_skip_verify: bool,
    /// ICE-сервера (STUN/TURN) для проверки/проброса соединения.
    /// Примеры:
    /// - "stun:stun.l.google.com:19302"
    /// - "turn:turn.example.com:3478?transport=udp"
    #[serde(default)]
    pub ice_servers: Vec<String>,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            webrtc_url: DEFAULT_URL.to_string(),
            tls_insecure_skip_verify: true,
            // По умолчанию добавляем public STUN для отладки; при прямом подключении к SFU он не мешает.
            ice_servers: vec!["stun:stun.l.google.com:19302".to_string()],
        }
    }
}

impl AppConfig {
    pub fn load() -> anyhow::Result<Self> {
        let path = std::path::Path::new(CONFIG_FILE);
        if !path.exists() {
            return Ok(Self::default());
        }
        let data = std::fs::read_to_string(path)?;
        let cfg: AppConfig = serde_json::from_str(&data)?;
        Ok(cfg)
    }
}

