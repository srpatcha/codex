//! Export functionality for session history.
//!
//! Supports exporting history entries to JSON and Markdown formats.

use crate::HistoryEntry;
use serde_json;

/// Supported export formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportFormat {
    /// Export as a JSON array.
    Json,
    /// Export as a Markdown document.
    Markdown,
}

/// Exports history entries to the specified format.
pub fn export_entries(entries: &[HistoryEntry], format: ExportFormat) -> Result<String, String> {
    match format {
        ExportFormat::Json => export_json(entries),
        ExportFormat::Markdown => export_markdown(entries),
    }
}

fn export_json(entries: &[HistoryEntry]) -> Result<String, String> {
    serde_json::to_string_pretty(entries).map_err(|e| format!("JSON serialization failed: {}", e))
}

fn export_markdown(entries: &[HistoryEntry]) -> Result<String, String> {
    let mut output = String::new();
    output.push_str("# Session History\n\n");

    let mut current_session: Option<&str> = None;

    for entry in entries {
        // Add session header when session changes
        if current_session != Some(&entry.session_id) {
            output.push_str(&format!("## Session: {}\n\n", entry.session_id));
            current_session = Some(&entry.session_id);
        }

        let role_label = match entry.role {
            crate::Role::User => "**User**",
            crate::Role::Assistant => "**Assistant**",
            crate::Role::System => "**System**",
            crate::Role::Tool => "**Tool**",
        };

        let timestamp = entry.timestamp.format("%Y-%m-%d %H:%M:%S UTC");
        output.push_str(&format!("### {} ({})\n\n", role_label, timestamp));
        output.push_str(&entry.content);
        output.push_str("\n\n---\n\n");
    }

    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Role;
    use chrono::Utc;

    fn make_test_entries() -> Vec<HistoryEntry> {
        vec![
            HistoryEntry {
                id: 1,
                session_id: "s1".to_string(),
                timestamp: Utc::now(),
                role: Role::User,
                content: "What is Rust?".to_string(),
                token_count: Some(3),
                metadata: None,
            },
            HistoryEntry {
                id: 2,
                session_id: "s1".to_string(),
                timestamp: Utc::now(),
                role: Role::Assistant,
                content: "Rust is a systems programming language.".to_string(),
                token_count: Some(6),
                metadata: None,
            },
        ]
    }

    #[test]
    fn test_export_json() {
        let entries = make_test_entries();
        let result = export_entries(&entries, ExportFormat::Json);
        assert!(result.is_ok());
        let json = result.unwrap();
        assert!(json.contains("What is Rust?"));
        assert!(json.contains("systems programming"));
    }

    #[test]
    fn test_export_markdown() {
        let entries = make_test_entries();
        let result = export_entries(&entries, ExportFormat::Markdown);
        assert!(result.is_ok());
        let md = result.unwrap();
        assert!(md.contains("# Session History"));
        assert!(md.contains("## Session: s1"));
        assert!(md.contains("**User**"));
        assert!(md.contains("**Assistant**"));
        assert!(md.contains("What is Rust?"));
    }

    #[test]
    fn test_export_empty() {
        let result = export_entries(&[], ExportFormat::Json);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "[]");
    }

    #[test]
    fn test_export_markdown_empty() {
        let result = export_entries(&[], ExportFormat::Markdown);
        assert!(result.is_ok());
        assert!(result.unwrap().contains("# Session History"));
    }
}
