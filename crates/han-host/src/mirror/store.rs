//! A durable han-timeline [`Store`] on the file system (TL-120…TL-125): one directory per history holding
//! `format`, an append-only `log` of length-prefixed, checksummed records (a torn append is cut whole on
//! open), and `high-water`, replaced atomically.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use han_timeline::Store;
use sha2::{Digest, Sha256};

/// A file-backed store.
#[derive(Debug)]
pub struct FileStore {
    dir: PathBuf,
}

impl FileStore {
    /// Opens (or creates) the store in `dir`, cutting a torn record a crash left at the end of the log.
    pub fn open(dir: &Path) -> std::io::Result<Self> {
        fs::create_dir_all(dir)?;
        let store = FileStore {
            dir: dir.to_path_buf(),
        };
        let (_, good) = store.read_log();
        if let Ok(f) = OpenOptions::new().write(true).open(store.path("log"))
            && f.metadata()?.len() != good as u64
        {
            f.set_len(good as u64)?;
            f.sync_all()?;
        }
        Ok(store)
    }

    /// The directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }

    fn replace(&self, name: &str, bytes: &[u8]) -> Result<(), String> {
        let tmp = self.path(&format!("{name}.tmp"));
        let mut f = File::create(&tmp).map_err(|e| e.to_string())?;
        f.write_all(bytes).map_err(|e| e.to_string())?;
        f.sync_all().map_err(|e| e.to_string())?;
        fs::rename(&tmp, self.path(name)).map_err(|e| e.to_string())
    }

    /// The whole records of the log, and the byte length they span.
    fn read_log(&self) -> (Vec<Vec<u8>>, usize) {
        let Ok(bytes) = fs::read(self.path("log")) else {
            return (Vec::new(), 0);
        };
        let mut out = Vec::new();
        let mut at = 0;
        while at + 4 <= bytes.len() {
            let len = u32::from_be_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
                as usize;
            let end = at + 4 + len + 32;
            if end > bytes.len() {
                break;
            }
            let body = &bytes[at + 4..at + 4 + len];
            if Sha256::digest(body).as_slice() != &bytes[at + 4 + len..end] {
                break;
            }
            out.push(body.to_vec());
            at = end;
        }
        (out, at)
    }

    fn high_waters(&self) -> Result<Vec<(String, String, u64)>, String> {
        let Ok(text) = fs::read_to_string(self.path("high-water")) else {
            return Ok(Vec::new());
        };
        text.lines()
            .filter(|l| !l.is_empty())
            .map(|l| {
                let mut p = l.split(' ');
                match (p.next(), p.next(), p.next().and_then(|c| c.parse().ok())) {
                    (Some(a), Some(k), Some(c)) => Ok((a.to_string(), k.to_string(), c)),
                    _ => Err(format!("bad high-water line {l:?}")),
                }
            })
            .collect()
    }

    /// Reads a small side file (for example the history id), if present.
    pub fn read_meta(&self, name: &str) -> Option<String> {
        fs::read_to_string(self.path(&format!("meta-{name}")))
            .ok()
            .map(|s| s.trim().to_owned())
    }

    /// Writes a small side file atomically.
    pub fn write_meta(&self, name: &str, value: &str) -> Result<(), String> {
        self.replace(&format!("meta-{name}"), value.as_bytes())
    }
}

impl Store for FileStore {
    fn format_version(&self) -> Result<Option<u32>, String> {
        match fs::read_to_string(self.path("format")) {
            Ok(s) => s.trim().parse().map(Some).map_err(|e| format!("{e}")),
            Err(_) => Ok(None),
        }
    }

    fn set_format_version(&mut self, version: u32) -> Result<(), String> {
        self.replace("format", version.to_string().as_bytes())
    }

    fn envelopes(&self) -> Result<Vec<Vec<u8>>, String> {
        Ok(self.read_log().0)
    }

    fn append(&mut self, envelope: &[u8]) -> Result<(), String> {
        let len = u32::try_from(envelope.len()).map_err(|_| "envelope too large".to_string())?;
        let mut rec = Vec::with_capacity(envelope.len() + 36);
        rec.extend_from_slice(&len.to_be_bytes());
        rec.extend_from_slice(envelope);
        rec.extend_from_slice(&Sha256::digest(envelope));
        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.path("log"))
            .map_err(|e| e.to_string())?;
        f.write_all(&rec).map_err(|e| e.to_string())?;
        f.sync_all().map_err(|e| e.to_string())
    }

    fn high_water(&self, actor: &str, key: &str) -> Result<u64, String> {
        Ok(self
            .high_waters()?
            .into_iter()
            .find(|(a, k, _)| a == actor && k == key)
            .map_or(0, |(_, _, c)| c))
    }

    fn raise_high_water(&mut self, actor: &str, key: &str, counter: u64) -> Result<(), String> {
        let mut all = self.high_waters()?;
        match all.iter_mut().find(|(a, k, _)| a == actor && k == key) {
            Some(e) if e.2 >= counter => return Ok(()),
            Some(e) => e.2 = counter,
            None => all.push((actor.to_string(), key.to_string(), counter)),
        }
        let text: String = all
            .iter()
            .map(|(a, k, c)| format!("{a} {k} {c}\n"))
            .collect();
        self.replace("high-water", text.as_bytes())
    }
}
