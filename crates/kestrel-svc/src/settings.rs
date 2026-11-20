//! Service settings in %ProgramData%\Kestrel (admin-writable only).

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Settings {
    /// Firmware prefixes whose experimental (untested) map the owner opted into.
    #[serde(default)]
    pub allow_experimental: Vec<String>,
    /// Full EC firmware string the verified map was last confirmed on.
    #[serde(default)]
    pub known_firmware: Option<String>,
}

pub fn data_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("KESTREL_DATA_DIR") {
        return dir.into();
    }
    #[cfg(windows)]
    {
        let base = std::env::var_os("ProgramData").map(PathBuf::from).unwrap_or_else(|| r"C:\ProgramData".into());
        base.join("Kestrel")
    }
    #[cfg(not(windows))]
    std::env::temp_dir().join("kestrel")
}

impl Settings {
    pub fn save(&self) -> std::io::Result<()> {
        let dir = data_dir();
        std::fs::create_dir_all(&dir)?;
        let tmp = dir.join("settings.json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(self).map_err(std::io::Error::other)?)?;
        std::fs::rename(tmp, dir.join("settings.json"))
    }

    pub fn load() -> Self {
        std::fs::read_to_string(data_dir().join("settings.json"))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn allows_experimental(&self, prefix: &str) -> bool {
        self.allow_experimental.iter().any(|p| p == prefix)
    }
}
