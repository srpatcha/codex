//! History rotation and cleanup.
//!
//! Provides time-based and size-based rotation of history entries to
//! prevent unbounded memory and disk growth.

use crate::{HistoryConfig, HistoryEntry};
use chrono::Utc;

/// Result of a rotation operation.
#[derive(Debug, Clone)]
pub struct RotationResult {
    /// Number of entries removed during rotation.
    pub entries_removed: usize,
    /// Bytes freed during rotation.
    pub bytes_freed: usize,
    /// Number of entries remaining after rotation.
    pub entries_remaining: usize,
}

/// Rotates (evicts) entries based on the configuration policy.
///
/// Removes entries that exceed age limits or when total count/bytes exceed limits.
pub fn rotate_entries(
    entries: &mut Vec<HistoryEntry>,
    total_bytes: &mut usize,
    config: &HistoryConfig,
) -> RotationResult {
    let initial_count = entries.len();
    let initial_bytes = *total_bytes;

    let now = Utc::now();
    let max_age = chrono::Duration::seconds(config.max_age_seconds as i64);

    // Remove entries older than max_age
    entries.retain(|entry| {
        let age = now.signed_duration_since(entry.timestamp);
        if age > max_age {
            *total_bytes = total_bytes.saturating_sub(entry.content.len());
            false
        } else {
            true
        }
    });

    // Enforce entry count limit
    while entries.len() > config.max_entries_in_memory {
        if let Some(removed) = entries.first() {
            *total_bytes = total_bytes.saturating_sub(removed.content.len());
        }
        entries.remove(0);
    }

    // Enforce byte limit
    while *total_bytes > config.max_bytes_in_memory && !entries.is_empty() {
        if let Some(removed) = entries.first() {
            *total_bytes = total_bytes.saturating_sub(removed.content.len());
        }
        entries.remove(0);
    }

    let entries_removed = initial_count - entries.len();
    let bytes_freed = initial_bytes.saturating_sub(*total_bytes);

    RotationResult {
        entries_removed,
        bytes_freed,
        entries_remaining: entries.len(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Role;
    use chrono::{Duration, Utc};

    fn make_entry_with_age(id: u64, content: &str, age_secs: i64) -> HistoryEntry {
        HistoryEntry {
            id,
            session_id: "s1".to_string(),
            timestamp: Utc::now() - Duration::seconds(age_secs),
            role: Role::User,
            content: content.to_string(),
            token_count: None,
            metadata: None,
        }
    }

    #[test]
    fn test_rotation_removes_old_entries() {
        let mut entries = vec![
            make_entry_with_age(1, "old message", 7200),   // 2 hours old
            make_entry_with_age(2, "new message", 100),    // recent
        ];
        let mut total_bytes: usize = entries.iter().map(|e| e.content.len()).sum();
        let config = HistoryConfig {
            max_entries_in_memory: 1000,
            max_bytes_in_memory: 1_000_000,
            max_age_seconds: 3600, // 1 hour
            storage_path: None,
        };

        let result = rotate_entries(&mut entries, &mut total_bytes, &config);
        assert_eq!(result.entries_removed, 1);
        assert_eq!(result.entries_remaining, 1);
        assert_eq!(entries[0].id, 2);
    }

    #[test]
    fn test_rotation_enforces_count_limit() {
        let mut entries: Vec<HistoryEntry> = (0..10)
            .map(|i| make_entry_with_age(i, &format!("msg {}", i), 10))
            .collect();
        let mut total_bytes: usize = entries.iter().map(|e| e.content.len()).sum();
        let config = HistoryConfig {
            max_entries_in_memory: 5,
            max_bytes_in_memory: 1_000_000,
            max_age_seconds: 86400,
            storage_path: None,
        };

        let result = rotate_entries(&mut entries, &mut total_bytes, &config);
        assert_eq!(result.entries_remaining, 5);
        assert_eq!(result.entries_removed, 5);
    }

    #[test]
    fn test_rotation_no_op_when_within_limits() {
        let mut entries = vec![
            make_entry_with_age(1, "msg", 10),
        ];
        let mut total_bytes: usize = entries.iter().map(|e| e.content.len()).sum();
        let config = HistoryConfig {
            max_entries_in_memory: 100,
            max_bytes_in_memory: 1_000_000,
            max_age_seconds: 86400,
            storage_path: None,
        };

        let result = rotate_entries(&mut entries, &mut total_bytes, &config);
        assert_eq!(result.entries_removed, 0);
        assert_eq!(result.entries_remaining, 1);
    }

    #[test]
    fn test_rotation_empty_entries() {
        let mut entries: Vec<HistoryEntry> = Vec::new();
        let mut total_bytes: usize = 0;
        let config = HistoryConfig::default();

        let result = rotate_entries(&mut entries, &mut total_bytes, &config);
        assert_eq!(result.entries_removed, 0);
        assert_eq!(result.entries_remaining, 0);
        assert_eq!(result.bytes_freed, 0);
    }
}
