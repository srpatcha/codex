//! Storage backend for session history persistence.
//!
//! Provides a file-backed storage layer using JSON lines format for
//! append-only persistence of history entries.

use crate::HistoryEntry;
use serde_json;
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

/// File-based storage backend for session history.
///
/// Stores entries in a JSON Lines (.jsonl) file for efficient
/// append-only writes and line-by-line reads.
pub struct StorageBackend {
    path: PathBuf,
}

impl StorageBackend {
    /// Creates a new storage backend at the given path.
    pub fn new(path: &str) -> Self {
        Self {
            path: PathBuf::from(path),
        }
    }

    /// Appends a single entry to the storage file.
    pub fn append_entry(&mut self, entry: &HistoryEntry) {
        if let Ok(json) = serde_json::to_string(entry) {
            if let Some(parent) = self.path.parent() {
                let _ = fs::create_dir_all(parent);
            }
            if let Ok(mut file) = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.path)
            {
                let _ = writeln!(file, "{}", json);
            }
        }
    }

    /// Loads all entries from the storage file.
    pub fn load_all(&self) -> Vec<HistoryEntry> {
        let mut entries = Vec::new();
        if let Ok(file) = fs::File::open(&self.path) {
            let reader = BufReader::new(file);
            for line in reader.lines() {
                if let Ok(line) = line {
                    if let Ok(entry) = serde_json::from_str::<HistoryEntry>(&line) {
                        entries.push(entry);
                    }
                }
            }
        }
        entries
    }

    /// Returns the storage file path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Returns the size of the storage file in bytes.
    pub fn file_size(&self) -> u64 {
        fs::metadata(&self.path)
            .map(|m| m.len())
            .unwrap_or(0)
    }

    /// Compacts the storage by rewriting the file with only the given entries.
    pub fn compact(&mut self) {
        // In a full implementation, this would rewrite the file with only
        // non-rotated entries. For now, we truncate if the file is empty.
        if self.file_size() == 0 {
            let _ = fs::remove_file(&self.path);
        }
    }

    /// Writes a complete set of entries, replacing the file contents.
    pub fn write_all(&mut self, entries: &[HistoryEntry]) {
        if let Some(parent) = self.path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if let Ok(mut file) = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&self.path)
        {
            for entry in entries {
                if let Ok(json) = serde_json::to_string(entry) {
                    let _ = writeln!(file, "{}", json);
                }
            }
        }
    }

    /// Deletes the storage file.
    pub fn delete(&self) {
        let _ = fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Role;
    use chrono::Utc;
    use tempfile::TempDir;

    fn make_entry(id: u64, content: &str) -> HistoryEntry {
        HistoryEntry {
            id,
            session_id: "test-session".to_string(),
            timestamp: Utc::now(),
            role: Role::User,
            content: content.to_string(),
            token_count: None,
            metadata: None,
        }
    }

    #[test]
    fn test_append_and_load() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("history.jsonl");
        let mut storage = StorageBackend::new(db_path.to_str().unwrap());

        storage.append_entry(&make_entry(1, "hello"));
        storage.append_entry(&make_entry(2, "world"));

        let entries = storage.load_all();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].content, "hello");
        assert_eq!(entries[1].content, "world");
    }

    #[test]
    fn test_load_nonexistent_file() {
        let storage = StorageBackend::new("/tmp/nonexistent_codex_test.jsonl");
        let entries = storage.load_all();
        assert!(entries.is_empty());
    }

    #[test]
    fn test_write_all_replaces_content() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("history.jsonl");
        let mut storage = StorageBackend::new(db_path.to_str().unwrap());

        storage.append_entry(&make_entry(1, "old"));
        storage.append_entry(&make_entry(2, "data"));

        let new_entries = vec![make_entry(3, "fresh")];
        storage.write_all(&new_entries);

        let entries = storage.load_all();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].content, "fresh");
    }

    #[test]
    fn test_file_size() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("history.jsonl");
        let mut storage = StorageBackend::new(db_path.to_str().unwrap());

        assert_eq!(storage.file_size(), 0);
        storage.append_entry(&make_entry(1, "data"));
        assert!(storage.file_size() > 0);
    }

    #[test]
    fn test_delete() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("history.jsonl");
        let mut storage = StorageBackend::new(db_path.to_str().unwrap());

        storage.append_entry(&make_entry(1, "data"));
        assert!(db_path.exists());

        storage.delete();
        assert!(!db_path.exists());
    }
}
