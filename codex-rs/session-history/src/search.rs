//! Search functionality for session history.
//!
//! Supports filtering by text content, role, session ID, and time range.

use crate::{HistoryEntry, Role};
use chrono::{DateTime, Utc};

/// A query for searching session history.
#[derive(Debug, Clone, Default)]
pub struct SearchQuery {
    /// Text substring to search for in entry content (case-insensitive).
    pub text: Option<String>,
    /// Filter by specific role.
    pub role: Option<Role>,
    /// Filter by session ID.
    pub session_id: Option<String>,
    /// Only include entries after this timestamp.
    pub after: Option<DateTime<Utc>>,
    /// Only include entries before this timestamp.
    pub before: Option<DateTime<Utc>>,
    /// Maximum number of results to return.
    pub limit: Option<usize>,
}

impl SearchQuery {
    /// Creates a new empty query.
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the text search term.
    pub fn with_text(mut self, text: &str) -> Self {
        self.text = Some(text.to_string());
        self
    }

    /// Sets the role filter.
    pub fn with_role(mut self, role: Role) -> Self {
        self.role = Some(role);
        self
    }

    /// Sets the session ID filter.
    pub fn with_session_id(mut self, session_id: &str) -> Self {
        self.session_id = Some(session_id.to_string());
        self
    }

    /// Sets the result limit.
    pub fn with_limit(mut self, limit: usize) -> Self {
        self.limit = Some(limit);
        self
    }
}

/// Searches entries matching the given query.
pub fn search_entries<'a>(entries: &'a [HistoryEntry], query: &SearchQuery) -> Vec<&'a HistoryEntry> {
    let mut results: Vec<&HistoryEntry> = entries
        .iter()
        .filter(|entry| {
            // Text filter (case-insensitive)
            if let Some(ref text) = query.text {
                if !entry.content.to_lowercase().contains(&text.to_lowercase()) {
                    return false;
                }
            }

            // Role filter
            if let Some(ref role) = query.role {
                if &entry.role != role {
                    return false;
                }
            }

            // Session filter
            if let Some(ref session_id) = query.session_id {
                if &entry.session_id != session_id {
                    return false;
                }
            }

            // Time range filters
            if let Some(ref after) = query.after {
                if &entry.timestamp < after {
                    return false;
                }
            }
            if let Some(ref before) = query.before {
                if &entry.timestamp > before {
                    return false;
                }
            }

            true
        })
        .collect();

    // Apply limit
    if let Some(limit) = query.limit {
        results.truncate(limit);
    }

    results
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn make_entries() -> Vec<HistoryEntry> {
        vec![
            HistoryEntry {
                id: 1,
                session_id: "s1".to_string(),
                timestamp: Utc::now(),
                role: Role::User,
                content: "Hello world".to_string(),
                token_count: Some(2),
                metadata: None,
            },
            HistoryEntry {
                id: 2,
                session_id: "s1".to_string(),
                timestamp: Utc::now(),
                role: Role::Assistant,
                content: "Hi there, how can I help?".to_string(),
                token_count: Some(6),
                metadata: None,
            },
            HistoryEntry {
                id: 3,
                session_id: "s2".to_string(),
                timestamp: Utc::now(),
                role: Role::User,
                content: "Fix the bug in main.rs".to_string(),
                token_count: Some(6),
                metadata: None,
            },
        ]
    }

    #[test]
    fn test_search_by_text() {
        let entries = make_entries();
        let query = SearchQuery::new().with_text("hello");
        let results = search_entries(&entries, &query);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, 1);
    }

    #[test]
    fn test_search_by_role() {
        let entries = make_entries();
        let query = SearchQuery::new().with_role(Role::Assistant);
        let results = search_entries(&entries, &query);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, 2);
    }

    #[test]
    fn test_search_by_session_id() {
        let entries = make_entries();
        let query = SearchQuery::new().with_session_id("s2");
        let results = search_entries(&entries, &query);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, 3);
    }

    #[test]
    fn test_search_with_limit() {
        let entries = make_entries();
        let query = SearchQuery::new().with_limit(1);
        let results = search_entries(&entries, &query);
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn test_search_case_insensitive() {
        let entries = make_entries();
        let query = SearchQuery::new().with_text("HELLO");
        let results = search_entries(&entries, &query);
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn test_search_no_match() {
        let entries = make_entries();
        let query = SearchQuery::new().with_text("nonexistent");
        let results = search_entries(&entries, &query);
        assert!(results.is_empty());
    }

    #[test]
    fn test_empty_query_returns_all() {
        let entries = make_entries();
        let query = SearchQuery::new();
        let results = search_entries(&entries, &query);
        assert_eq!(results.len(), 3);
    }
}
