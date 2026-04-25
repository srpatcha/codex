//! # Codex Session History
//!
//! Provides session history tracking, persistence, search, export, and rotation
//! for Codex sessions. Designed to prevent unbounded memory growth by enforcing
//! configurable size limits and automatic rotation policies.

pub mod export;
pub mod rotation;
pub mod search;
pub mod storage;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Unique identifier for a session history entry.
pub type EntryId = u64;

/// Represents a single session history entry.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HistoryEntry {
    pub id: EntryId,
    pub session_id: String,
    pub timestamp: DateTime<Utc>,
    pub role: Role,
    pub content: String,
    pub token_count: Option<u32>,
    pub metadata: Option<serde_json::Value>,
}

/// The role of the message author.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
    System,
    Tool,
}

/// Configuration for session history management.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryConfig {
    /// Maximum number of entries to keep in memory per session.
    pub max_entries_in_memory: usize,
    /// Maximum total bytes of content to keep in memory.
    pub max_bytes_in_memory: usize,
    /// Maximum age of entries before rotation (in seconds).
    pub max_age_seconds: u64,
    /// Path for persistent storage (SQLite database).
    pub storage_path: Option<String>,
}

impl Default for HistoryConfig {
    fn default() -> Self {
        Self {
            max_entries_in_memory: 1000,
            max_bytes_in_memory: 10 * 1024 * 1024, // 10 MB
            max_age_seconds: 86400,                  // 24 hours
            storage_path: None,
        }
    }
}

/// Main session history manager.
///
/// Tracks entries with bounded memory usage and supports persistence,
/// search, export, and automatic rotation.
pub struct SessionHistory {
    config: HistoryConfig,
    entries: Vec<HistoryEntry>,
    next_id: EntryId,
    total_content_bytes: usize,
    storage: Option<storage::StorageBackend>,
}

impl SessionHistory {
    /// Creates a new session history with the given configuration.
    pub fn new(config: HistoryConfig) -> Self {
        let storage = config
            .storage_path
            .as_ref()
            .map(|path| storage::StorageBackend::new(path));

        Self {
            config,
            entries: Vec::new(),
            next_id: 1,
            total_content_bytes: 0,
            storage,
        }
    }

    /// Adds a new entry, enforcing memory bounds.
    /// Returns the assigned entry ID.
    pub fn add_entry(
        &mut self,
        session_id: String,
        role: Role,
        content: String,
        token_count: Option<u32>,
        metadata: Option<serde_json::Value>,
    ) -> EntryId {
        let entry = HistoryEntry {
            id: self.next_id,
            session_id,
            timestamp: Utc::now(),
            role,
            content,
            token_count,
            metadata,
        };
        let id = entry.id;
        self.next_id += 1;

        self.total_content_bytes += entry.content.len();
        self.entries.push(entry);

        // Enforce memory bounds
        self.enforce_limits();

        // Persist if storage is available
        if let Some(ref mut storage) = self.storage {
            if let Some(last) = self.entries.last() {
                storage.append_entry(last);
            }
        }

        id
    }

    /// Enforces memory limits by evicting oldest entries.
    fn enforce_limits(&mut self) {
        // Evict by entry count
        while self.entries.len() > self.config.max_entries_in_memory {
            if let Some(removed) = self.entries.first() {
                self.total_content_bytes =
                    self.total_content_bytes.saturating_sub(removed.content.len());
            }
            self.entries.remove(0);
        }

        // Evict by total bytes
        while self.total_content_bytes > self.config.max_bytes_in_memory && !self.entries.is_empty()
        {
            if let Some(removed) = self.entries.first() {
                self.total_content_bytes =
                    self.total_content_bytes.saturating_sub(removed.content.len());
            }
            self.entries.remove(0);
        }
    }

    /// Returns all entries currently in memory.
    pub fn entries(&self) -> &[HistoryEntry] {
        &self.entries
    }

    /// Returns the number of entries in memory.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Returns whether there are no entries.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Returns total content bytes tracked in memory.
    pub fn total_bytes(&self) -> usize {
        self.total_content_bytes
    }

    /// Gets a specific entry by ID.
    pub fn get_entry(&self, id: EntryId) -> Option<&HistoryEntry> {
        self.entries.iter().find(|e| e.id == id)
    }

    /// Gets all entries for a specific session.
    pub fn get_session_entries(&self, session_id: &str) -> Vec<&HistoryEntry> {
        self.entries
            .iter()
            .filter(|e| e.session_id == session_id)
            .collect()
    }

    /// Removes all entries for a session.
    pub fn clear_session(&mut self, session_id: &str) {
        self.entries.retain(|e| {
            if e.session_id == session_id {
                self.total_content_bytes =
                    self.total_content_bytes.saturating_sub(e.content.len());
                false
            } else {
                true
            }
        });
    }

    /// Clears all entries.
    pub fn clear_all(&mut self) {
        self.entries.clear();
        self.total_content_bytes = 0;
    }

    /// Performs search across history entries.
    pub fn search(&self, query: &search::SearchQuery) -> Vec<&HistoryEntry> {
        search::search_entries(&self.entries, query)
    }

    /// Exports history to the specified format.
    pub fn export(&self, format: export::ExportFormat) -> Result<String, String> {
        export::export_entries(&self.entries, format)
    }

    /// Runs rotation/cleanup based on configuration.
    pub fn rotate(&mut self) -> rotation::RotationResult {
        let result = rotation::rotate_entries(
            &mut self.entries,
            &mut self.total_content_bytes,
            &self.config,
        );

        if let Some(ref mut storage) = self.storage {
            storage.compact();
        }

        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_config(max_entries: usize, max_bytes: usize) -> HistoryConfig {
        HistoryConfig {
            max_entries_in_memory: max_entries,
            max_bytes_in_memory: max_bytes,
            max_age_seconds: 86400,
            storage_path: None,
        }
    }

    #[test]
    fn test_add_and_retrieve_entry() {
        let mut history = SessionHistory::new(make_config(100, 1_000_000));
        let id = history.add_entry(
            "session-1".to_string(),
            Role::User,
            "Hello, world!".to_string(),
            Some(3),
            None,
        );
        assert_eq!(id, 1);
        assert_eq!(history.len(), 1);

        let entry = history.get_entry(id).unwrap();
        assert_eq!(entry.content, "Hello, world!");
        assert_eq!(entry.role, Role::User);
        assert_eq!(entry.session_id, "session-1");
    }

    #[test]
    fn test_entry_count_limit_enforced() {
        let mut history = SessionHistory::new(make_config(3, 1_000_000));

        for i in 0..5 {
            history.add_entry(
                "s1".to_string(),
                Role::User,
                format!("message {}", i),
                None,
                None,
            );
        }

        assert_eq!(history.len(), 3);
        // Oldest entries should have been evicted
        assert!(history.get_entry(1).is_none());
        assert!(history.get_entry(2).is_none());
        assert!(history.get_entry(3).is_some());
    }

    #[test]
    fn test_byte_limit_enforced() {
        let mut history = SessionHistory::new(make_config(1000, 50));

        // Add entries with ~20 bytes each
        for i in 0..5 {
            history.add_entry(
                "s1".to_string(),
                Role::User,
                format!("message number {}", i),
                None,
                None,
            );
        }

        assert!(history.total_bytes() <= 50);
    }

    #[test]
    fn test_get_session_entries() {
        let mut history = SessionHistory::new(make_config(100, 1_000_000));
        history.add_entry("s1".to_string(), Role::User, "msg1".to_string(), None, None);
        history.add_entry("s2".to_string(), Role::User, "msg2".to_string(), None, None);
        history.add_entry("s1".to_string(), Role::Assistant, "reply".to_string(), None, None);

        let s1_entries = history.get_session_entries("s1");
        assert_eq!(s1_entries.len(), 2);

        let s2_entries = history.get_session_entries("s2");
        assert_eq!(s2_entries.len(), 1);
    }

    #[test]
    fn test_clear_session() {
        let mut history = SessionHistory::new(make_config(100, 1_000_000));
        history.add_entry("s1".to_string(), Role::User, "msg1".to_string(), None, None);
        history.add_entry("s2".to_string(), Role::User, "msg2".to_string(), None, None);

        history.clear_session("s1");
        assert_eq!(history.len(), 1);
        assert!(history.get_session_entries("s1").is_empty());
    }

    #[test]
    fn test_clear_all() {
        let mut history = SessionHistory::new(make_config(100, 1_000_000));
        history.add_entry("s1".to_string(), Role::User, "msg".to_string(), None, None);
        history.add_entry("s2".to_string(), Role::User, "msg".to_string(), None, None);

        history.clear_all();
        assert!(history.is_empty());
        assert_eq!(history.total_bytes(), 0);
    }

    #[test]
    fn test_ids_are_sequential() {
        let mut history = SessionHistory::new(make_config(100, 1_000_000));
        let id1 = history.add_entry("s".to_string(), Role::User, "a".to_string(), None, None);
        let id2 = history.add_entry("s".to_string(), Role::User, "b".to_string(), None, None);
        let id3 = history.add_entry("s".to_string(), Role::User, "c".to_string(), None, None);
        assert_eq!(id1, 1);
        assert_eq!(id2, 2);
        assert_eq!(id3, 3);
    }
}
