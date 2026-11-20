//! Service settings in %ProgramData%\Kestrel (admin-writable only).

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Settings {
    /// Firmware prefixes whose experimental (untested) map the owner opted into.
    #[serde(default)]
    pub allow_experimental: Vec<String>,
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
