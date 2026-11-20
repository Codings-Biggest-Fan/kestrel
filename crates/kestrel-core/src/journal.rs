//! Restore journal: the original value of every EC register Kestrel has
//! written, persisted so a crash or uninstall can put the laptop back.

use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Debug, Default)]
pub struct Journal {
    path: Option<PathBuf>,
    entries: BTreeMap<u8, u8>,
}

impl Journal {
    pub fn in_memory() -> Self {
        Self::default()
    }

    /// Load the journal at `path`, or start empty if it does not exist.
    pub fn open(path: PathBuf) -> std::io::Result<Self> {
        let entries = match std::fs::read_to_string(&path) {
            Ok(s) => serde_json::from_str::<BTreeMap<String, u8>>(&s)
                .unwrap_or_default()
                .into_iter()
                .filter_map(|(k, v)| u8::from_str_radix(k.trim_start_matches("0x"), 16).ok().map(|k| (k, v)))
                .collect(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
            Err(e) => return Err(e),
        };
        Ok(Self { path: Some(path), entries })
    }

    pub fn contains(&self, addr: u8) -> bool {
        self.entries.contains_key(&addr)
    }

    pub fn record(&mut self, addr: u8, original: u8) {
        self.entries.entry(addr).or_insert(original);
        self.flush();
    }

    pub fn entries(&self) -> impl Iterator<Item = (u8, u8)> + '_ {
        self.entries.iter().map(|(&a, &v)| (a, v))
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
        let map: BTreeMap<String, u8> = self.entries.iter().map(|(a, v)| (format!("0x{a:02x}"), *v)).collect();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let tmp = path.with_extension("tmp");
        if std::fs::write(&tmp, serde_json::to_vec_pretty(&map).unwrap_or_default()).is_ok() {
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
        let j2 = Journal::open(path).unwrap();
        assert_eq!(j2.entries().collect::<Vec<_>>(), vec![(0x98, 0x02)]);
        let _ = std::fs::remove_dir_all(dir);
    }
}
