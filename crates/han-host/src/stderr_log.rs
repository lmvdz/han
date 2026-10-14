//! The agent's stderr, kept for diagnosis. It is never published (it can hold anything): each line is masked
//! ([`Masker`]) and appended to a log in the session directory, outside the sandbox's writable paths, up to
//! [`MAX_LOG_BYTES`]. The last [`TAIL_LINES`] lines are kept in memory so a session that fails to start can
//! say why.

use std::collections::VecDeque;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::task::JoinHandle;

use crate::mask::Masker;

/// The log's name in the session directory.
pub const LOG_NAME: &str = "adapter-stderr.log";
/// Lines kept in memory for an error message.
pub const TAIL_LINES: usize = 20;
/// The log stops growing at this size; later lines still reach the tail.
pub const MAX_LOG_BYTES: u64 = 1 << 20;
/// A longer line is cut to this many bytes.
pub const MAX_LINE_BYTES: usize = 4096;

/// A running capture of one process's stderr.
pub struct StderrLog {
    path: PathBuf,
    tail: Arc<Mutex<VecDeque<String>>>,
    task: Option<JoinHandle<()>>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// Starts copying `r` into the log at `path`, masked.
pub fn capture<R>(mut r: R, path: PathBuf, masker: Arc<Masker>) -> StderrLog
where
    R: AsyncRead + Send + Unpin + 'static,
{
    let tail = Arc::new(Mutex::new(VecDeque::new()));
    let t2 = Arc::clone(&tail);
    let p2 = path.clone();
    let task = tokio::spawn(async move {
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&p2)
            .ok();
        let mut written = file
            .as_ref()
            .and_then(|f| f.metadata().ok())
            .map_or(0, |m| m.len());
        let started = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let mut emit = |line: &[u8], file: &mut Option<std::fs::File>| {
            let text = String::from_utf8_lossy(line);
            let text = masker.mask(text.trim_end_matches('\r')).into_owned();
            if let Some(f) = file.as_mut()
                && written < MAX_LOG_BYTES
            {
                let _ = writeln!(f, "{text}");
                written += text.len() as u64 + 1;
                if written >= MAX_LOG_BYTES {
                    let _ = writeln!(f, "[han: stderr log full; later lines are not kept]");
                }
            }
            let mut t = lock(&t2);
            t.push_back(text);
            while t.len() > TAIL_LINES {
                t.pop_front();
            }
        };
        if let Some(f) = file.as_mut() {
            let _ = writeln!(f, "[han: agent started, unix time {started}]");
        }
        let mut buf = vec![0u8; 8192];
        let mut line: Vec<u8> = Vec::new();
        let mut cut = false;
        loop {
            let n = match r.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            for &b in &buf[..n] {
                if b == b'\n' {
                    emit(&line, &mut file);
                    line.clear();
                    cut = false;
                } else if line.len() < MAX_LINE_BYTES {
                    line.push(b);
                } else if !cut {
                    line.extend_from_slice(" [cut]".as_bytes());
                    cut = true;
                }
            }
        }
        if !line.is_empty() {
            emit(&line, &mut file);
        }
    });
    StderrLog {
        path,
        tail,
        task: Some(task),
    }
}

impl StderrLog {
    /// Where the log is.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The last lines seen so far (masked).
    pub fn tail(&self) -> Vec<String> {
        lock(&self.tail).iter().cloned().collect()
    }

    /// Waits up to `wait` for the stream to end (the process exited), then returns the last lines.
    pub async fn finish(&mut self, wait: Duration) -> Vec<String> {
        if let Some(t) = self.task.take() {
            let _ = tokio::time::timeout(wait, t).await;
        }
        self.tail()
    }

    /// What to add to an error about the agent: the last few lines and where the rest is.
    pub fn describe(lines: &[String], path: &Path, n: usize) -> String {
        const SHOWN: usize = 300;
        let last: Vec<String> = lines
            .iter()
            .filter(|l| !l.trim().is_empty())
            .map(|l| match l.char_indices().nth(SHOWN) {
                Some((i, _)) => format!("{}…", &l[..i]),
                None => l.clone(),
            })
            .collect();
        let last = &last[last.len().saturating_sub(n)..];
        if last.is_empty() {
            format!(
                "the agent wrote nothing to stderr (log: {})",
                path.display()
            )
        } else {
            format!(
                "the agent's stderr ended with: {} (log: {})",
                last.join(" | "),
                path.display()
            )
        }
    }
}

impl Drop for StderrLog {
    fn drop(&mut self) {
        // The copy runs until the stream ends; dropping the handle detaches it.
        self.task.take();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn masks_caps_and_keeps_the_tail() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(LOG_NAME);
        let secret = "proxytoken-1234567890abcdef";
        let masker = Arc::new(Masker::new(vec![secret.into()]));
        let mut input = String::new();
        input.push_str(&format!("HTTPS_PROXY=http://han:{secret}@127.0.0.1:1\n"));
        for i in 0..30 {
            input.push_str(&format!("line {i}\r\n"));
        }
        input.push_str(&"x".repeat(MAX_LINE_BYTES * 2));
        input.push('\n');
        input.push_str("error: unrecognized subcommand 'listen'");
        let (mut w, r) = tokio::io::duplex(1024);
        let mut log = capture(r, path.clone(), masker);
        tokio::io::AsyncWriteExt::write_all(&mut w, input.as_bytes())
            .await
            .unwrap();
        drop(w);
        let tail = log.finish(Duration::from_secs(5)).await;
        assert_eq!(tail.len(), TAIL_LINES);
        assert_eq!(
            tail.last().unwrap(),
            "error: unrecognized subcommand 'listen'"
        );
        assert!(tail.iter().any(|l| l == "line 29"), "{tail:?}");
        assert!(
            tail.iter()
                .any(|l| l.ends_with(" [cut]") && l.len() < MAX_LINE_BYTES + 10)
        );
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains(secret), "the token never reaches the log");
        assert!(text.contains("[masked:"));
        assert!(text.contains("line 0\n"), "carriage returns are dropped");
        let d = StderrLog::describe(&tail, &path, 2);
        assert!(
            d.contains("xxx… | error: unrecognized subcommand 'listen'"),
            "{d}"
        );
        assert!(d.len() < 600, "{d}");
        let d = StderrLog::describe(&tail[..tail.len() - 2], &path, 1);
        assert!(
            d.ends_with(&format!("line 29 (log: {})", path.display())),
            "{d}"
        );
        assert!(StderrLog::describe(&[], &path, 3).contains("wrote nothing"));
        assert!(d.contains(LOG_NAME));
    }

    #[tokio::test]
    async fn stops_growing_at_the_cap() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(LOG_NAME);
        let (mut w, r) = tokio::io::duplex(64 * 1024);
        let mut log = capture(r, path.clone(), Arc::new(Masker::new(vec![])));
        let line = format!("{}\n", "y".repeat(1000));
        for _ in 0..(MAX_LOG_BYTES as usize / 1000 + 200) {
            tokio::io::AsyncWriteExt::write_all(&mut w, line.as_bytes())
                .await
                .unwrap();
        }
        tokio::io::AsyncWriteExt::write_all(&mut w, b"last\n")
            .await
            .unwrap();
        drop(w);
        let tail = log.finish(Duration::from_secs(10)).await;
        assert_eq!(tail.last().unwrap(), "last");
        let len = std::fs::metadata(&path).unwrap().len();
        assert!(len < MAX_LOG_BYTES + 4096, "{len}");
        assert!(std::fs::read_to_string(&path).unwrap().contains("log full"));
    }
}
