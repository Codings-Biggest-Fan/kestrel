//! Restore journal: the original value of every setting Kestrel has written,
//! persisted so a crash or uninstall can put the laptop back. EC registers are
//! keyed `0xNN`; WMI-method backends use the getter name as the key.

use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Debug, Default)]
pub struct Journal {
    path: Option<PathBuf>,
    entries: BTreeMap<String, u32>,
}

fn ec_key(addr: u8) -> String {
    format!("0x{addr:02x}")
}

impl Journal {
    pub fn in_memory() -> Self {
        Self::default()
    }

    /// Load the journal at `path`, or start empty if it does not exist.
    pub fn open(path: PathBuf) -> std::io::Result<Self> {
        let entries = match std::fs::read_to_string(&path) {
            Ok(s) => serde_json::from_str::<BTreeMap<String, u32>>(&s).unwrap_or_default(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
            Err(e) => return Err(e),
        };
        Ok(Self {
            path: Some(path),
            entries,
        })
    }

    pub fn contains(&self, addr: u8) -> bool {
        self.entries.contains_key(&ec_key(addr))
    }

    pub fn record(&mut self, addr: u8, original: u8) {
        self.record_named(&ec_key(addr), original.into());
    }

    /// EC register entries as (address, original value).
    pub fn entries(&self) -> impl Iterator<Item = (u8, u8)> + '_ {
        self.entries.iter().filter_map(|(k, &v)| {
            let addr = u8::from_str_radix(k.strip_prefix("0x")?, 16).ok()?;
            Some((addr, v as u8))
        })
    }

    pub fn contains_named(&self, key: &str) -> bool {
        self.entries.contains_key(key)
    }

    /// Keep the first original only: later writes must not overwrite it.
    pub fn record_named(&mut self, key: &str, original: u32) {
        if !self.entries.contains_key(key) {
            self.entries.insert(key.to_string(), original);
            self.flush();
        }
    }

    pub fn named_entries(&self) -> impl Iterator<Item = (&str, u32)> + '_ {
        self.entries.iter().map(|(k, &v)| (k.as_str(), v))
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.flush();
    }

    fn flush(&self) {
        let Some(path) = &self.path else { return };
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let tmp = path.with_extension("tmp");
        if std::fs::write(
            &tmp,
            serde_json::to_vec_pretty(&self.entries).unwrap_or_default(),
        )
        .is_ok()
        {
            let _ = std::fs::rename(tmp, path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persists_first_original_only() {
        let dir = std::env::temp_dir().join(format!("kestrel-journal-{}", std::process::id()));
        let path = dir.join("journal.json");
        let mut j = Journal::open(path.clone()).unwrap();
        j.record(0x98, 0x02);
        j.record(0x98, 0x82);
        j.record_named("GetChargeStop", 100);
        j.record_named("GetChargeStop", 80);
        let j2 = Journal::open(path).unwrap();
        assert_eq!(j2.entries().collect::<Vec<_>>(), vec![(0x98, 0x02)]);
        assert!(j2.named_entries().any(|e| e == ("GetChargeStop", 100)));
        let _ = std::fs::remove_dir_all(dir);
    }
}
