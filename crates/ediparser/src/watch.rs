//! Poll-based file watcher for the EDI dropzone. Ported from `ediparser/watch/watcher.py`.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// File extensions the dropzone accepts. Without a filter the poller feeds
/// the parser anything that lands in the directory — editor swap files,
/// half-copied uploads, .DS_Store — and logs a parse failure for each.
const SUFFIXES: &[&str] = &["835", "837", "edi", "txt"];

pub struct Watcher {
    pub directory: String,
    processed: HashSet<String>,
    /// Size seen on the previous poll. A file still being written is left
    /// alone until it stops growing — parsing a half-copied 835 gives a
    /// silently truncated claim set, which is worse than a late one.
    sizes: HashMap<String, u64>,
}

impl Watcher {
    pub fn new(directory: impl Into<String>) -> Self {
        Self {
            directory: directory.into(),
            processed: HashSet::new(),
            sizes: HashMap::new(),
        }
    }

    /// Files that are stable (same size on consecutive polls) and not yet
    /// processed. Updates `self.sizes` as a side effect.
    pub fn ready_files(&mut self) -> Vec<PathBuf> {
        let dir = Path::new(&self.directory);
        let mut ready = Vec::new();

        let entries = match dir.read_dir() {
            Ok(e) => e,
            Err(_) => return ready,
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let fname = path
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            if self.processed.contains(&fname) {
                continue;
            }

            let ext = path
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("")
                .to_lowercase();
            if !SUFFIXES.contains(&ext.as_str()) {
                continue;
            }

            let size = match entry.metadata() {
                Ok(m) => m.len(),
                Err(_) => continue,
            };

            if self.sizes.get(&fname) != Some(&size) {
                // Still arriving (or brand new). Note the size and look
                // again next poll.
                self.sizes.insert(fname, size);
                continue;
            }

            ready.push(path);
        }
        ready
    }

    pub fn mark_processed(&mut self, fname: &str) {
        self.processed.insert(fname.to_string());
        self.sizes.remove(fname);
    }

    /// Treat files that already have parsed output as done.
    ///
    /// `_processed` lives in memory, so without this every restart re-parses
    /// and re-stores the entire dropzone history.
    pub fn seed_from_outputs(&mut self, output_dir: &str) {
        let dir = Path::new(output_dir);
        let entries = match dir.read_dir() {
            Ok(e) => e,
            Err(_) => return,
        };
        let mut count = 0;
        for entry in entries.flatten() {
            let fname = entry.file_name().to_string_lossy().to_string();
            if fname.ends_with(".json") {
                let base = &fname[..fname.len() - 5];
                self.processed.insert(base.to_string());
                count += 1;
            }
        }
        if count > 0 {
            tracing::info!("Seeded watcher with {count} already-parsed files");
        }
    }
}
