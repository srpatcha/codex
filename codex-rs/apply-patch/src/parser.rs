//! This module is responsible for parsing & validating a patch into a list of "hunks".
//! (It does not attempt to actually check that the patch can be applied to the filesystem.)
//!
//! The official Lark grammar for the apply-patch format is:
//!
//! start: begin_patch hunk+ end_patch
//! begin_patch: "*** Begin Patch" LF
//! end_patch: "*** End Patch" LF?
//!
//! hunk: add_hunk | delete_hunk | update_hunk
//! add_hunk: "*** Add File: " filename LF add_line+
//! delete_hunk: "*** Delete File: " filename LF
//! update_hunk: "*** Update File: " filename LF change_move? change?
//! filename: /(.+)/
//! add_line: "+" /(.+)/ LF -> line
//!
//! change_move: "*** Move to: " filename LF
//! change: (change_context | change_line)+ eof_line?
//! change_context: ("@@" | "@@ " /(.+)/) LF
//! change_line: ("+" | "-" | " ") /(.+)/ LF
//! eof_line: "*** End of File" LF
//!
//! The parser below is a little more lenient than the explicit spec and allows for
//! leading/trailing whitespace around patch markers.
use crate::ApplyPatchArgs;
use codex_utils_absolute_path::AbsolutePathBuf;
#[cfg(test)]
use codex_utils_absolute_path::test_support::PathBufExt;
use std::path::Path;
use std::path::PathBuf;

use thiserror::Error;

const BEGIN_PATCH_MARKER: &str = "*** Begin Patch";
const END_PATCH_MARKER: &str = "*** End Patch";
const ADD_FILE_MARKER: &str = "*** Add File: ";
const DELETE_FILE_MARKER: &str = "*** Delete File: ";
const UPDATE_FILE_MARKER: &str = "*** Update File: ";
const MOVE_TO_MARKER: &str = "*** Move to: ";
const EOF_MARKER: &str = "*** End of File";
const CHANGE_CONTEXT_MARKER: &str = "@@ ";
const EMPTY_CHANGE_CONTEXT_MARKER: &str = "@@";

/// Currently, the only OpenAI model that knowingly requires lenient parsing is
/// gpt-4.1. While we could try to require everyone to pass in a strictness
/// param when invoking apply_patch, it is a pain to thread it through all of
/// the call sites, so we resign ourselves allowing lenient parsing for all
/// models. See [`ParseMode::Lenient`] for details on the exceptions we make for
/// gpt-4.1.
const PARSE_IN_STRICT_MODE: bool = false;

#[derive(Debug, PartialEq, Error, Clone)]
pub enum ParseError {
    #[error("invalid patch: {0}")]
    InvalidPatchError(String),
    #[error("invalid hunk at line {line_number}, {message}")]
    InvalidHunkError { message: String, line_number: usize },
}
use ParseError::*;

#[derive(Debug, PartialEq, Clone)]
#[allow(clippy::enum_variant_names)]
pub enum Hunk {
    AddFile {
        path: PathBuf,
        contents: String,
    },
    DeleteFile {
        path: PathBuf,
    },
    UpdateFile {
        path: PathBuf,
        move_path: Option<PathBuf>,

        /// Chunks should be in order, i.e. the `change_context` of one chunk
        /// should occur later in the file than the previous chunk.
        chunks: Vec<UpdateFileChunk>,
    },
}

impl Hunk {
    pub fn resolve_path(&self, cwd: &AbsolutePathBuf) -> AbsolutePathBuf {
        let path = match self {
            Hunk::UpdateFile { path, .. } => path,
            Hunk::AddFile { .. } | Hunk::DeleteFile { .. } => self.path(),
        };
        AbsolutePathBuf::resolve_path_against_base(path, cwd)
    }

    /// Returns the path affected by this hunk, using the move destination for rename hunks.
    pub fn path(&self) -> &Path {
        match self {
            Hunk::AddFile { path, .. } => path,
            Hunk::DeleteFile { path } => path,
            Hunk::UpdateFile {
                move_path: Some(path),
                ..
            } => path,
            Hunk::UpdateFile {
                path,
                move_path: None,
                ..
            } => path,
        }
    }
}

use Hunk::*;

#[derive(Debug, PartialEq, Clone)]
pub struct UpdateFileChunk {
    /// A single line of context used to narrow down the position of the chunk
    /// (this is usually a class, method, or function definition.)
    pub change_context: Option<String>,

    /// A contiguous block of lines that should be replaced with `new_lines`.
    /// `old_lines` must occur strictly after `change_context`.
    pub old_lines: Vec<String>,
    pub new_lines: Vec<String>,

    /// If set to true, `old_lines` must occur at the end of the source file.
    /// (Tolerance around trailing newlines should be encouraged.)
    pub is_end_of_file: bool,
}

fn parse_move_to_line(line: &str) -> Option<&str> {
    line.trim().strip_prefix(MOVE_TO_MARKER)
}

fn update_chunk_from_header(line: &str) -> Option<UpdateFileChunk> {
    let line = line.trim();
    let change_context = if line == EMPTY_CHANGE_CONTEXT_MARKER {
        None
    } else {
        Some(line.strip_prefix(CHANGE_CONTEXT_MARKER)?.to_string())
    };
    Some(UpdateFileChunk {
        change_context,
        old_lines: Vec::new(),
        new_lines: Vec::new(),
        is_end_of_file: false,
    })
}

fn push_update_line(chunk: &mut UpdateFileChunk, line: &str) -> bool {
    match line.chars().next() {
        None => {
            chunk.old_lines.push(String::new());
            chunk.new_lines.push(String::new());
        }
        Some(' ') => {
            chunk.old_lines.push(line[1..].to_string());
            chunk.new_lines.push(line[1..].to_string());
        }
        Some('+') => {
            chunk.new_lines.push(line[1..].to_string());
        }
        Some('-') => {
            chunk.old_lines.push(line[1..].to_string());
        }
        _ => return false,
    }
    true
}

fn invalid_hunk_header_error(line: &str, line_number: usize) -> ParseError {
    InvalidHunkError {
        message: format!(
            "'{line}' is not a valid hunk header. Valid hunk headers: '*** Add File: {{path}}', '*** Delete File: {{path}}', '*** Update File: {{path}}'"
        ),
        line_number,
    }
}

fn empty_update_hunk_error(path: &Path, line_number: usize) -> ParseError {
    InvalidHunkError {
        message: format!("Update file hunk for path '{}' is empty", path.display()),
        line_number,
    }
}

fn missing_update_context_error(line: &str, line_number: usize) -> ParseError {
    InvalidHunkError {
        message: format!("Expected update hunk to start with a @@ context marker, got: '{line}'"),
        line_number,
    }
}

fn unexpected_update_line_error(line: &str, line_number: usize) -> ParseError {
    InvalidHunkError {
        message: format!(
            "Unexpected line found in update hunk: '{line}'. Every line should start with ' ' (context line), '+' (added line), or '-' (removed line)"
        ),
        line_number,
    }
}

pub fn parse_patch(patch: &str) -> Result<ApplyPatchArgs, ParseError> {
    let mode = if PARSE_IN_STRICT_MODE {
        ParseMode::Strict
    } else {
        ParseMode::Lenient
    };
    parse_patch_text(patch, mode)
}

#[derive(Debug, Default, Clone)]
pub struct StreamingPatchParser {
    line_buffer: String,
    partial_applied_len: usize,
    state: StreamingParserState,
    hunks: Vec<Hunk>,
    last_snapshot: Vec<Hunk>,
}

#[derive(Debug, Default, Clone)]
enum StreamingParserState {
    #[default]
    WaitingForBegin,
    BetweenHunks,
    AddFile {
        path: PathBuf,
        contents: String,
    },
    UpdateFile {
        path: PathBuf,
        header_line_number: usize,
        move_path: Option<PathBuf>,
        can_accept_move: bool,
        chunks: Vec<UpdateFileChunk>,
        current_chunk: Option<UpdateFileChunk>,
    },
    Finished,
    Invalid,
}

impl StreamingPatchParser {
    pub fn push_delta(&mut self, delta: &str) -> Option<Vec<Hunk>> {
        for ch in delta.chars() {
            if ch == '\n' {
                if self.partial_applied_len > 0 {
                    self.finish_partial_line();
                } else {
                    let line = std::mem::take(&mut self.line_buffer);
                    self.process_line(line.trim_end_matches('\r'));
                }
            } else {
                self.line_buffer.push(ch);
                self.apply_partial_line();
            }
        }
        self.snapshot_if_changed()
    }

    fn process_line(&mut self, line: &str) {
        if matches!(
            self.state,
            StreamingParserState::Finished | StreamingParserState::Invalid
        ) {
            return;
        }

        let trimmed = line.trim();
        if trimmed == BEGIN_PATCH_MARKER {
            self.state = StreamingParserState::BetweenHunks;
            return;
        }

        if matches!(self.state, StreamingParserState::WaitingForBegin) {
            self.state = StreamingParserState::Invalid;
            return;
        }

        if trimmed == END_PATCH_MARKER {
            self.finish_current_hunk();
            self.state = StreamingParserState::Finished;
            return;
        }

        if trimmed.starts_with(ADD_FILE_MARKER)
            || trimmed.starts_with(DELETE_FILE_MARKER)
            || trimmed.starts_with(UPDATE_FILE_MARKER)
        {
            self.finish_current_hunk();
            self.start_hunk(line, /*line_number*/ 0);
            return;
        }

        match &mut self.state {
            StreamingParserState::AddFile { contents, .. } => {
                if let Some(line_to_add) = line.strip_prefix('+') {
                    contents.push_str(line_to_add);
                    contents.push('\n');
                }
            }
            StreamingParserState::UpdateFile {
                move_path,
                can_accept_move,
                chunks,
                current_chunk,
                ..
            } => {
                if *can_accept_move
                    && move_path.is_none()
                    && let Some(path) = parse_move_to_line(line)
                {
                    *move_path = Some(PathBuf::from(path));
                    return;
                }
                *can_accept_move = false;

                if let Some(chunk) = update_chunk_from_header(line) {
                    finish_streaming_chunk(chunks, current_chunk);
                    *current_chunk = Some(chunk);
                    return;
                }

                if trimmed == EOF_MARKER {
                    if let Some(chunk) = current_chunk.as_mut() {
                        chunk.is_end_of_file = true;
                    }
                    finish_streaming_chunk(chunks, current_chunk);
                    return;
                }

                if current_chunk.is_none() {
                    *current_chunk = Some(UpdateFileChunk {
                        change_context: None,
                        old_lines: Vec::new(),
                        new_lines: Vec::new(),
                        is_end_of_file: false,
                    });
                }

                let Some(chunk) = current_chunk.as_mut() else {
                    return;
                };
                push_update_line(chunk, line);
            }
            StreamingParserState::WaitingForBegin
            | StreamingParserState::BetweenHunks
            | StreamingParserState::Finished
            | StreamingParserState::Invalid => {}
        }
    }

    fn start_hunk(&mut self, line: &str, line_number: usize) {
        let line = line.trim();
        if let Some(path) = line.strip_prefix(ADD_FILE_MARKER) {
            self.state = StreamingParserState::AddFile {
                path: PathBuf::from(path),
                contents: String::new(),
            };
        } else if let Some(path) = line.strip_prefix(DELETE_FILE_MARKER) {
            self.hunks.push(DeleteFile {
                path: PathBuf::from(path),
            });
            self.state = StreamingParserState::BetweenHunks;
        } else if let Some(path) = line.strip_prefix(UPDATE_FILE_MARKER) {
            self.state = StreamingParserState::UpdateFile {
                path: PathBuf::from(path),
                header_line_number: line_number,
                move_path: None,
                can_accept_move: true,
                chunks: Vec::new(),
                current_chunk: None,
            };
        }
    }

    fn apply_partial_line(&mut self) {
        if self.partial_applied_len == self.line_buffer.len() {
            return;
        }

        match &mut self.state {
            StreamingParserState::AddFile { contents, .. } => {
                let Some(line_to_add) = self.line_buffer.strip_prefix('+') else {
                    return;
                };
                let start = self.partial_applied_len.saturating_sub(1);
                contents.push_str(&line_to_add[start..]);
                self.partial_applied_len = self.line_buffer.len();
            }
            StreamingParserState::UpdateFile {
                can_accept_move,
                current_chunk,
                ..
            } => {
                let Some(prefix) = self.line_buffer.chars().next() else {
                    return;
                };
                if !matches!(prefix, ' ' | '+' | '-') {
                    return;
                }

                *can_accept_move = false;
                if current_chunk.is_none() {
                    *current_chunk = Some(UpdateFileChunk {
                        change_context: None,
                        old_lines: Vec::new(),
                        new_lines: Vec::new(),
                        is_end_of_file: false,
                    });
                }

                let Some(chunk) = current_chunk.as_mut() else {
                    return;
                };
                let start = self.partial_applied_len.saturating_sub(1);
                let line = &self.line_buffer[1..];
                let new_text = &line[start..];
                match prefix {
                    ' ' if self.partial_applied_len == 0 => {
                        chunk.old_lines.push(line.to_string());
                        chunk.new_lines.push(line.to_string());
                    }
                    ' ' => {
                        if let Some(old_line) = chunk.old_lines.last_mut() {
                            old_line.push_str(new_text);
                        }
                        if let Some(new_line) = chunk.new_lines.last_mut() {
                            new_line.push_str(new_text);
                        }
                    }
                    '+' if self.partial_applied_len == 0 => {
                        chunk.new_lines.push(line.to_string());
                    }
                    '+' => {
                        if let Some(new_line) = chunk.new_lines.last_mut() {
                            new_line.push_str(new_text);
                        }
                    }
                    '-' if self.partial_applied_len == 0 => {
                        chunk.old_lines.push(line.to_string());
                    }
                    '-' => {
                        if let Some(old_line) = chunk.old_lines.last_mut() {
                            old_line.push_str(new_text);
                        }
                    }
                    _ => {}
                }
                self.partial_applied_len = self.line_buffer.len();
            }
            StreamingParserState::WaitingForBegin
            | StreamingParserState::BetweenHunks
            | StreamingParserState::Finished
            | StreamingParserState::Invalid => {}
        }
    }

    fn finish_partial_line(&mut self) {
        let trim_cr = self.line_buffer.ends_with('\r');
        match &mut self.state {
            StreamingParserState::AddFile { contents, .. } => {
                if trim_cr {
                    contents.pop();
                }
                contents.push('\n');
            }
            StreamingParserState::UpdateFile {
                current_chunk: Some(chunk),
                ..
            } if trim_cr => match self.line_buffer.chars().next() {
                Some(' ') => {
                    if let Some(old_line) = chunk.old_lines.last_mut() {
                        old_line.pop();
                    }
                    if let Some(new_line) = chunk.new_lines.last_mut() {
                        new_line.pop();
                    }
                }
                Some('+') => {
                    if let Some(new_line) = chunk.new_lines.last_mut() {
                        new_line.pop();
                    }
                }
                Some('-') => {
                    if let Some(old_line) = chunk.old_lines.last_mut() {
                        old_line.pop();
                    }
                }
                None | Some(_) => {}
            },
            StreamingParserState::WaitingForBegin
            | StreamingParserState::BetweenHunks
            | StreamingParserState::UpdateFile { .. }
            | StreamingParserState::Finished
            | StreamingParserState::Invalid => {}
        }
        self.line_buffer.clear();
        self.partial_applied_len = 0;
    }

    fn parse_complete_lines(lines: &[&str]) -> Result<Vec<Hunk>, ParseError> {
        let mut parser = Self::default();
        for (index, line) in lines.iter().enumerate() {
            parser.process_complete_line(line.trim_end_matches('\r'), index + 1)?;
        }
        match parser.state {
            StreamingParserState::Finished => Ok(parser.hunks),
            StreamingParserState::WaitingForBegin => Err(InvalidPatchError(String::from(
                "The first line of the patch must be '*** Begin Patch'",
            ))),
            _ => Err(InvalidPatchError(String::from(
                "The last line of the patch must be '*** End Patch'",
            ))),
        }
    }

    #[cfg(test)]
    fn parse_incomplete_lines_for_test(lines: &[&str]) -> Result<Vec<Hunk>, ParseError> {
        let mut parser = Self::default();
        for line in lines {
            parser.process_line(line.trim_end_matches('\r'));
            if matches!(parser.state, StreamingParserState::Invalid) {
                return Err(InvalidPatchError(String::from(
                    "The first line of the patch must be '*** Begin Patch'",
                )));
            }
        }
        Ok(parser.current_hunks())
    }

    fn process_complete_line(&mut self, line: &str, line_number: usize) -> Result<(), ParseError> {
        let trimmed = line.trim();
        if matches!(self.state, StreamingParserState::Finished) {
            return Err(invalid_hunk_header_error(trimmed, line_number));
        }

        if trimmed == BEGIN_PATCH_MARKER {
            self.state = StreamingParserState::BetweenHunks;
            return Ok(());
        }

        if matches!(self.state, StreamingParserState::WaitingForBegin) {
            return Err(InvalidPatchError(String::from(
                "The first line of the patch must be '*** Begin Patch'",
            )));
        }

        if trimmed == END_PATCH_MARKER {
            self.finish_current_hunk_complete()?;
            self.state = StreamingParserState::Finished;
            return Ok(());
        }

        if trimmed.starts_with(ADD_FILE_MARKER)
            || trimmed.starts_with(DELETE_FILE_MARKER)
            || trimmed.starts_with(UPDATE_FILE_MARKER)
        {
            self.finish_current_hunk_complete()?;
            self.start_hunk(line, line_number);
            return Ok(());
        }

        match &mut self.state {
            StreamingParserState::AddFile { contents, .. } => {
                if let Some(line_to_add) = line.strip_prefix('+') {
                    contents.push_str(line_to_add);
                    contents.push('\n');
                    return Ok(());
                }
                self.finish_current_hunk_complete()?;
                Err(invalid_hunk_header_error(trimmed, line_number))
            }
            StreamingParserState::UpdateFile {
                path: _,
                move_path,
                can_accept_move,
                chunks,
                current_chunk,
                ..
            } => {
                if *can_accept_move
                    && move_path.is_none()
                    && let Some(path) = parse_move_to_line(line)
                {
                    *move_path = Some(PathBuf::from(path));
                    return Ok(());
                }
                *can_accept_move = false;

                if let Some(chunk) = update_chunk_from_header(line) {
                    finish_complete_chunk(chunks, current_chunk, line_number)?;
                    *current_chunk = Some(chunk);
                    return Ok(());
                }

                if trimmed == EOF_MARKER {
                    let Some(chunk) = current_chunk.as_mut() else {
                        return Err(missing_update_context_error(line, line_number));
                    };
                    if chunk.old_lines.is_empty() && chunk.new_lines.is_empty() {
                        return Err(InvalidHunkError {
                            message: "Update hunk does not contain any lines".to_string(),
                            line_number,
                        });
                    }
                    chunk.is_end_of_file = true;
                    finish_complete_chunk(chunks, current_chunk, line_number)?;
                    return Ok(());
                }

                if current_chunk.is_none() && line.trim().is_empty() {
                    return Ok(());
                }

                if current_chunk.is_none() {
                    if chunks.is_empty() {
                        *current_chunk = Some(UpdateFileChunk {
                            change_context: None,
                            old_lines: Vec::new(),
                            new_lines: Vec::new(),
                            is_end_of_file: false,
                        });
                    } else if line.starts_with('*') {
                        return Err(invalid_hunk_header_error(trimmed, line_number));
                    } else {
                        return Err(missing_update_context_error(line, line_number));
                    }
                }

                let Some(chunk) = current_chunk.as_mut() else {
                    return Ok(());
                };
                if push_update_line(chunk, line) {
                    return Ok(());
                }

                if chunk.old_lines.is_empty() && chunk.new_lines.is_empty() {
                    return Err(unexpected_update_line_error(line, line_number));
                }

                finish_complete_chunk(chunks, current_chunk, line_number)?;
                if line.starts_with('*') {
                    Err(invalid_hunk_header_error(trimmed, line_number))
                } else {
                    Err(missing_update_context_error(line, line_number))
                }
            }
            StreamingParserState::BetweenHunks => {
                Err(invalid_hunk_header_error(trimmed, line_number))
            }
            StreamingParserState::WaitingForBegin => Err(InvalidPatchError(String::from(
                "The first line of the patch must be '*** Begin Patch'",
            ))),
            StreamingParserState::Finished | StreamingParserState::Invalid => {
                Err(invalid_hunk_header_error(trimmed, line_number))
            }
        }
    }

    fn finish_current_hunk_complete(&mut self) -> Result<(), ParseError> {
        let state = std::mem::replace(&mut self.state, StreamingParserState::BetweenHunks);
        match state {
            StreamingParserState::AddFile { path, contents } => {
                self.hunks.push(AddFile { path, contents });
            }
            StreamingParserState::UpdateFile {
                path,
                header_line_number,
                move_path,
                mut chunks,
                mut current_chunk,
                ..
            } => {
                finish_complete_chunk(&mut chunks, &mut current_chunk, header_line_number)?;
                if chunks.is_empty() {
                    return Err(empty_update_hunk_error(&path, header_line_number));
                }
                self.hunks.push(UpdateFile {
                    path,
                    move_path,
                    chunks,
                });
            }
            other => {
                self.state = other;
            }
        }
        Ok(())
    }

    fn finish_current_hunk(&mut self) {
        let state = std::mem::replace(&mut self.state, StreamingParserState::BetweenHunks);
        match state {
            StreamingParserState::AddFile { path, contents } => {
                self.hunks.push(AddFile { path, contents });
            }
            StreamingParserState::UpdateFile {
                path,
                header_line_number: _,
                move_path,
                can_accept_move: _,
                mut chunks,
                mut current_chunk,
            } => {
                finish_streaming_chunk(&mut chunks, &mut current_chunk);
                if !chunks.is_empty() {
                    self.hunks.push(UpdateFile {
                        path,
                        move_path,
                        chunks,
                    });
                }
            }
            other => {
                self.state = other;
            }
        }
    }

    fn snapshot_if_changed(&mut self) -> Option<Vec<Hunk>> {
        let snapshot = self.current_hunks();
        if snapshot.is_empty() || snapshot == self.last_snapshot {
            return None;
        }
        self.last_snapshot = snapshot.clone();
        Some(snapshot)
    }

    fn current_hunks(&self) -> Vec<Hunk> {
        let mut hunks = self.hunks.clone();
        match &self.state {
            StreamingParserState::AddFile { path, contents } => {
                let mut contents = contents.clone();
                if self.partial_applied_len > 0 && self.line_buffer.starts_with('+') {
                    contents.push('\n');
                }
                hunks.push(AddFile {
                    path: path.clone(),
                    contents,
                });
            }
            StreamingParserState::UpdateFile {
                path,
                move_path,
                chunks,
                current_chunk,
                ..
            } => {
                let mut chunks = chunks.clone();
                if let Some(chunk) = current_chunk
                    && (!chunk.old_lines.is_empty()
                        || !chunk.new_lines.is_empty()
                        || chunk.is_end_of_file)
                {
                    chunks.push(chunk.clone());
                }
                if !chunks.is_empty() {
                    hunks.push(UpdateFile {
                        path: path.clone(),
                        move_path: move_path.clone(),
                        chunks,
                    });
                }
            }
            StreamingParserState::WaitingForBegin
            | StreamingParserState::BetweenHunks
            | StreamingParserState::Finished
            | StreamingParserState::Invalid => {}
        }
        hunks
    }
}

fn finish_streaming_chunk(
    chunks: &mut Vec<UpdateFileChunk>,
    current_chunk: &mut Option<UpdateFileChunk>,
) {
    if let Some(chunk) = current_chunk.take()
        && (!chunk.old_lines.is_empty() || !chunk.new_lines.is_empty() || chunk.is_end_of_file)
    {
        chunks.push(chunk);
    }
}

fn finish_complete_chunk(
    chunks: &mut Vec<UpdateFileChunk>,
    current_chunk: &mut Option<UpdateFileChunk>,
    line_number: usize,
) -> Result<(), ParseError> {
    if let Some(chunk) = current_chunk.take() {
        if chunk.old_lines.is_empty() && chunk.new_lines.is_empty() && !chunk.is_end_of_file {
            return Err(InvalidHunkError {
                message: "Update hunk does not contain any lines".to_string(),
                line_number,
            });
        }
        chunks.push(chunk);
    }
    Ok(())
}

enum ParseMode {
    /// Parse the patch text argument as is.
    Strict,

    /// GPT-4.1 is known to formulate the `command` array for the `local_shell`
    /// tool call for `apply_patch` call using something like the following:
    ///
    /// ```json
    /// [
    ///   "apply_patch",
    ///   "<<'EOF'\n*** Begin Patch\n*** Update File: README.md\n@@...\n*** End Patch\nEOF\n",
    /// ]
    /// ```
    ///
    /// This is a problem because `local_shell` is a bit of a misnomer: the
    /// `command` is not invoked by passing the arguments to a shell like Bash,
    /// but are invoked using something akin to `execvpe(3)`.
    ///
    /// This is significant in this case because where a shell would interpret
    /// `<<'EOF'...` as a heredoc and pass the contents via stdin (which is
    /// fine, as `apply_patch` is specified to read from stdin if no argument is
    /// passed), `execvpe(3)` interprets the heredoc as a literal string. To get
    /// the `local_shell` tool to run a command the way shell would, the
    /// `command` array must be something like:
    ///
    /// ```json
    /// [
    ///   "bash",
    ///   "-lc",
    ///   "apply_patch <<'EOF'\n*** Begin Patch\n*** Update File: README.md\n@@...\n*** End Patch\nEOF\n",
    /// ]
    /// ```
    ///
    /// In lenient mode, we check if the argument to `apply_patch` starts with
    /// `<<'EOF'` and ends with `EOF\n`. If so, we strip off these markers,
    /// trim() the result, and treat what is left as the patch text.
    Lenient,
}

fn parse_patch_text(patch: &str, mode: ParseMode) -> Result<ApplyPatchArgs, ParseError> {
    let lines: Vec<&str> = patch.trim().lines().collect();
    let (patch_lines, _) = match mode {
        ParseMode::Strict => check_patch_boundaries_strict(&lines)?,
        ParseMode::Lenient => check_patch_boundaries_lenient(&lines)?,
    };

    let hunks = StreamingPatchParser::parse_complete_lines(patch_lines)?;
    let patch = patch_lines.join("\n");
    Ok(ApplyPatchArgs {
        hunks,
        patch,
        workdir: None,
    })
}

/// Checks the start and end lines of the patch text for `apply_patch`,
/// returning an error if they do not match the expected markers.
fn check_patch_boundaries_strict<'a>(
    lines: &'a [&'a str],
) -> Result<(&'a [&'a str], &'a [&'a str]), ParseError> {
    let (first_line, last_line) = match lines {
        [] => (None, None),
        [first] => (Some(first), Some(first)),
        [first, .., last] => (Some(first), Some(last)),
    };
    check_start_and_end_lines_strict(first_line, last_line)?;
    Ok((lines, &lines[1..lines.len() - 1]))
}

/// If we are in lenient mode, we check if the first line starts with `<<EOF`
/// (possibly quoted) and the last line ends with `EOF`. There must be at least
/// 4 lines total because the heredoc markers take up 2 lines and the patch text
/// must have at least 2 lines.
///
/// If successful, returns the lines of the patch text that contain the patch
/// contents, excluding the heredoc markers.
fn check_patch_boundaries_lenient<'a>(
    original_lines: &'a [&'a str],
) -> Result<(&'a [&'a str], &'a [&'a str]), ParseError> {
    let original_parse_error = match check_patch_boundaries_strict(original_lines) {
        Ok(lines) => return Ok(lines),
        Err(e) => e,
    };

    match original_lines {
        [first, .., last] => {
            if (first == &"<<EOF" || first == &"<<'EOF'" || first == &"<<\"EOF\"")
                && last.ends_with("EOF")
                && original_lines.len() >= 4
            {
                let inner_lines = &original_lines[1..original_lines.len() - 1];
                check_patch_boundaries_strict(inner_lines)
            } else {
                Err(original_parse_error)
            }
        }
        _ => Err(original_parse_error),
    }
}

fn check_start_and_end_lines_strict(
    first_line: Option<&&str>,
    last_line: Option<&&str>,
) -> Result<(), ParseError> {
    let first_line = first_line.map(|line| line.trim());
    let last_line = last_line.map(|line| line.trim());

    match (first_line, last_line) {
        (Some(first), Some(last)) if first == BEGIN_PATCH_MARKER && last == END_PATCH_MARKER => {
            Ok(())
        }
        (Some(first), _) if first != BEGIN_PATCH_MARKER => Err(InvalidPatchError(String::from(
            "The first line of the patch must be '*** Begin Patch'",
        ))),
        _ => Err(InvalidPatchError(String::from(
            "The last line of the patch must be '*** End Patch'",
        ))),
    }
}

#[cfg(test)]
fn parse_patch_progress_for_test(patch: &str) -> Result<ApplyPatchArgs, ParseError> {
    let lines: Vec<&str> = patch.trim().lines().collect();
    let hunks = StreamingPatchParser::parse_incomplete_lines_for_test(&lines)?;
    Ok(ApplyPatchArgs {
        hunks,
        patch: lines.join("\n"),
        workdir: None,
    })
}

#[test]
fn test_streaming_patch_parser_parses_partial_patch_text() {
    assert_eq!(
        parse_patch_progress_for_test("*** Begin Patch\n*** Add File: src/hello.txt\n+hello\n+wor"),
        Ok(ApplyPatchArgs {
            hunks: vec![AddFile {
                path: PathBuf::from("src/hello.txt"),
                contents: "hello\nwor\n".to_string(),
            }],
            patch: "*** Begin Patch\n*** Add File: src/hello.txt\n+hello\n+wor".to_string(),
            workdir: None,
        })
    );

    assert_eq!(
        parse_patch_progress_for_test(
            "*** Begin Patch\n*** Update File: src/old.rs\n*** Move to: src/new.rs\n@@\n-old\n+new",
        ),
        Ok(ApplyPatchArgs {
            hunks: vec![UpdateFile {
                path: PathBuf::from("src/old.rs"),
                move_path: Some(PathBuf::from("src/new.rs")),
                chunks: vec![UpdateFileChunk {
                    change_context: None,
                    old_lines: vec!["old".to_string()],
                    new_lines: vec!["new".to_string()],
                    is_end_of_file: false,
                }],
            }],
            patch: "*** Begin Patch\n*** Update File: src/old.rs\n*** Move to: src/new.rs\n@@\n-old\n+new".to_string(),
            workdir: None,
        })
    );

    assert!(parse_patch_progress_for_test("*** Begin Patch\n*** Delete File: gone.txt").is_ok());
    assert!(
        parse_patch_text(
            "*** Begin Patch\n*** Delete File: gone.txt",
            ParseMode::Strict
        )
        .is_err()
    );

    assert_eq!(
        parse_patch_progress_for_test(
            "*** Begin Patch\n*** Add File: src/one.txt\n+one\n*** Delete File: src/two.txt\n",
        ),
        Ok(ApplyPatchArgs {
            hunks: vec![
                AddFile {
                    path: PathBuf::from("src/one.txt"),
                    contents: "one\n".to_string(),
                },
                DeleteFile {
                    path: PathBuf::from("src/two.txt"),
                },
            ],
            patch: "*** Begin Patch\n*** Add File: src/one.txt\n+one\n*** Delete File: src/two.txt"
                .to_string(),
            workdir: None,
        })
    );
}

#[test]
fn test_streaming_patch_parser_large_patch_by_character() {
    let patch = "\
*** Begin Patch
*** Add File: docs/release-notes.md
+# Release notes
+
+## CLI
+- Surface apply_patch progress while arguments stream.
+- Keep final patch application gated on the completed tool call.
+- Include file summaries in the progress event payload.
*** Update File: src/config.rs
@@ impl Config
-    pub apply_patch_progress: bool,
+    pub stream_apply_patch_progress: bool,
     pub include_diagnostics: bool,
@@ fn default_progress_interval()
-    Duration::from_millis(500)
+    Duration::from_millis(250)
*** Delete File: src/legacy_patch_progress.rs
*** Update File: crates/cli/src/main.rs
*** Move to: crates/cli/src/bin/codex.rs
@@ fn run()
-    let args = Args::parse();
-    dispatch(args)
+    let cli = Cli::parse();
+    dispatch(cli)
*** Add File: tests/fixtures/apply_patch_progress.json
+{
+  \"type\": \"apply_patch_progress\",
+  \"hunks\": [
+    { \"operation\": \"add\", \"path\": \"docs/release-notes.md\" },
+    { \"operation\": \"update\", \"path\": \"src/config.rs\" }
+  ]
+}
*** Update File: README.md
@@ Development workflow
 Build the Rust workspace before opening a pull request.
+When touching streamed tool calls, include parser coverage for partial input.
+Prefer tests that exercise the exact event payload shape.
*** Delete File: docs/old-apply-patch-progress.md
*** End Patch";

    let mut parser = StreamingPatchParser::default();
    let mut max_hunk_count = 0;
    let mut saw_hunk_counts = Vec::new();
    for ch in patch.chars() {
        if let Some(hunks) = parser.push_delta(&ch.to_string()) {
            let hunk_count = hunks.len();
            assert!(
                hunk_count >= max_hunk_count,
                "hunk count should never decrease while streaming: {hunk_count} < {max_hunk_count}",
            );
            if hunk_count > max_hunk_count {
                saw_hunk_counts.push(hunk_count);
                max_hunk_count = hunk_count;
            }
        }
    }

    assert_eq!(saw_hunk_counts, vec![1, 2, 3, 4, 5, 6, 7]);
    let hunks = parser.current_hunks();
    assert_eq!(hunks.len(), 7);
    assert_eq!(
        hunks
            .iter()
            .map(|hunk| match hunk {
                AddFile { .. } => "add",
                DeleteFile { .. } => "delete",
                UpdateFile {
                    move_path: Some(_), ..
                } => "move-update",
                UpdateFile {
                    move_path: None, ..
                } => "update",
            })
            .collect::<Vec<_>>(),
        vec![
            "add",
            "update",
            "delete",
            "move-update",
            "add",
            "update",
            "delete"
        ]
    );
}

#[test]
fn test_streaming_patch_parser_advances_by_character_in_current_line() {
    let mut parser = StreamingPatchParser::default();
    assert_eq!(parser.push_delta("*** Begin Patch\n"), None);
    assert_eq!(
        parser.push_delta("*** Add File: src/hello.txt\n"),
        Some(vec![AddFile {
            path: PathBuf::from("src/hello.txt"),
            contents: String::new(),
        }])
    );
    assert_eq!(
        parser.push_delta("+hel"),
        Some(vec![AddFile {
            path: PathBuf::from("src/hello.txt"),
            contents: "hel\n".to_string(),
        }])
    );
    assert_eq!(
        parser.push_delta("lo"),
        Some(vec![AddFile {
            path: PathBuf::from("src/hello.txt"),
            contents: "hello\n".to_string(),
        }])
    );
    assert_eq!(
        parser.push_delta("\n+world"),
        Some(vec![AddFile {
            path: PathBuf::from("src/hello.txt"),
            contents: "hello\nworld\n".to_string(),
        }])
    );
    assert_eq!(
        parser.push_delta("\n*** Delete File: src/gone.txt\n"),
        Some(vec![
            AddFile {
                path: PathBuf::from("src/hello.txt"),
                contents: "hello\nworld\n".to_string(),
            },
            DeleteFile {
                path: PathBuf::from("src/gone.txt"),
            },
        ])
    );
}

#[test]
fn test_parse_patch() {
    assert_eq!(
        parse_patch_text("bad", ParseMode::Strict),
        Err(InvalidPatchError(
            "The first line of the patch must be '*** Begin Patch'".to_string()
        ))
    );
    assert_eq!(
        parse_patch_text("*** Begin Patch\nbad", ParseMode::Strict),
        Err(InvalidPatchError(
            "The last line of the patch must be '*** End Patch'".to_string()
        ))
    );

    assert_eq!(
        parse_patch_text(
            concat!(
                "*** Begin Patch",
                " ",
                "\n*** Add File: foo\n+hi\n",
                " ",
                "*** End Patch"
            ),
            ParseMode::Strict
        )
        .unwrap()
        .hunks,
        vec![AddFile {
            path: PathBuf::from("foo"),
            contents: "hi\n".to_string()
        }]
    );
    assert_eq!(
        parse_patch_text(
            "*** Begin Patch\n\
             *** Update File: test.py\n\
             *** End Patch",
            ParseMode::Strict
        ),
        Err(InvalidHunkError {
            message: "Update file hunk for path 'test.py' is empty".to_string(),
            line_number: 2,
        })
    );
    assert_eq!(
        parse_patch_text(
            "*** Begin Patch\n\
             *** End Patch",
            ParseMode::Strict
        )
        .unwrap()
        .hunks,
        Vec::new()
    );
    assert_eq!(
        parse_patch_text(
            "*** Begin Patch\n\
             *** Add File: path/add.py\n\
             +abc\n\
             +def\n\
             *** Delete File: path/delete.py\n\
             *** Update File: path/update.py\n\
             *** Move to: path/update2.py\n\
             @@ def f():\n\
             -    pass\n\
             +    return 123\n\
             *** End Patch",
            ParseMode::Strict
        )
        .unwrap()
        .hunks,
        vec![
            AddFile {
                path: PathBuf::from("path/add.py"),
                contents: "abc\ndef\n".to_string()
            },
            DeleteFile {
                path: PathBuf::from("path/delete.py")
            },
            UpdateFile {
                path: PathBuf::from("path/update.py"),
                move_path: Some(PathBuf::from("path/update2.py")),
                chunks: vec![UpdateFileChunk {
                    change_context: Some("def f():".to_string()),
                    old_lines: vec!["    pass".to_string()],
                    new_lines: vec!["    return 123".to_string()],
                    is_end_of_file: false
                }]
            }
        ]
    );
    // Update hunk followed by another hunk (Add File).
    assert_eq!(
        parse_patch_text(
            "*** Begin Patch\n\
             *** Update File: file.py\n\
             @@\n\
             +line\n\
             *** Add File: other.py\n\
             +content\n\
             *** End Patch",
            ParseMode::Strict
        )
        .unwrap()
        .hunks,
        vec![
            UpdateFile {
                path: PathBuf::from("file.py"),
                move_path: None,
                chunks: vec![UpdateFileChunk {
                    change_context: None,
                    old_lines: vec![],
                    new_lines: vec!["line".to_string()],
                    is_end_of_file: false
                }],
            },
            AddFile {
                path: PathBuf::from("other.py"),
                contents: "content\n".to_string()
            }
        ]
    );

    // Update hunk without an explicit @@ header for the first chunk should parse.
    // Use a raw string to preserve the leading space diff marker on the context line.
    assert_eq!(
        parse_patch_text(
            r#"*** Begin Patch
*** Update File: file2.py
 import foo
+bar
*** End Patch"#,
            ParseMode::Strict
        )
        .unwrap()
        .hunks,
        vec![UpdateFile {
            path: PathBuf::from("file2.py"),
            move_path: None,
            chunks: vec![UpdateFileChunk {
                change_context: None,
                old_lines: vec!["import foo".to_string()],
                new_lines: vec!["import foo".to_string(), "bar".to_string()],
                is_end_of_file: false,
            }],
        }]
    );
}

#[test]
fn test_parse_patch_accepts_relative_and_absolute_hunk_paths() {
    let dir = tempfile::tempdir().unwrap();
    let absolute_delete = dir.path().join("absolute-delete.py").abs();
    let absolute_update = dir.path().join("absolute-update.py").abs();
    let patch_text = format!(
        r#"*** Begin Patch
*** Add File: relative-add.py
+content
*** Delete File: {}
*** Update File: {}
@@
-old
+new
*** End Patch"#,
        absolute_delete.display(),
        absolute_update.display()
    );

    assert_eq!(
        parse_patch_text(&patch_text, ParseMode::Strict)
            .unwrap()
            .hunks,
        vec![
            AddFile {
                path: PathBuf::from("relative-add.py"),
                contents: "content\n".to_string()
            },
            DeleteFile {
                path: absolute_delete.to_path_buf()
            },
            UpdateFile {
                path: absolute_update.to_path_buf(),
                move_path: None,
                chunks: vec![UpdateFileChunk {
                    change_context: None,
                    old_lines: vec!["old".to_string()],
                    new_lines: vec!["new".to_string()],
                    is_end_of_file: false
                }]
            },
        ]
    );
}

#[test]
fn test_hunk_resolve_path_accepts_relative_and_absolute_paths() {
    let cwd_dir = tempfile::tempdir().unwrap();
    let cwd = cwd_dir.path().to_path_buf().abs();
    let absolute_dir = tempfile::tempdir().unwrap();
    let absolute_add = absolute_dir.path().join("absolute-add.py").abs();
    let absolute_delete = absolute_dir.path().join("absolute-delete.py").abs();
    let absolute_update = absolute_dir.path().join("absolute-update.py").abs();

    for (hunk, expected_path) in [
        (
            AddFile {
                path: PathBuf::from("relative-add.py"),
                contents: String::new(),
            },
            cwd.join("relative-add.py"),
        ),
        (
            DeleteFile {
                path: PathBuf::from("relative-delete.py"),
            },
            cwd.join("relative-delete.py"),
        ),
        (
            UpdateFile {
                path: PathBuf::from("relative-update.py"),
                move_path: None,
                chunks: Vec::new(),
            },
            cwd.join("relative-update.py"),
        ),
        (
            AddFile {
                path: absolute_add.to_path_buf(),
                contents: String::new(),
            },
            absolute_add,
        ),
        (
            DeleteFile {
                path: absolute_delete.to_path_buf(),
            },
            absolute_delete,
        ),
        (
            UpdateFile {
                path: absolute_update.to_path_buf(),
                move_path: None,
                chunks: Vec::new(),
            },
            absolute_update,
        ),
    ] {
        assert_eq!(hunk.resolve_path(&cwd), expected_path);
    }
}

#[test]
fn test_parse_patch_lenient() {
    let patch_text = r#"*** Begin Patch
*** Update File: file2.py
 import foo
+bar
*** End Patch"#;
    let expected_patch = vec![UpdateFile {
        path: PathBuf::from("file2.py"),
        move_path: None,
        chunks: vec![UpdateFileChunk {
            change_context: None,
            old_lines: vec!["import foo".to_string()],
            new_lines: vec!["import foo".to_string(), "bar".to_string()],
            is_end_of_file: false,
        }],
    }];
    let expected_error =
        InvalidPatchError("The first line of the patch must be '*** Begin Patch'".to_string());

    let patch_text_in_heredoc = format!("<<EOF\n{patch_text}\nEOF\n");
    assert_eq!(
        parse_patch_text(&patch_text_in_heredoc, ParseMode::Strict),
        Err(expected_error.clone())
    );
    assert_eq!(
        parse_patch_text(&patch_text_in_heredoc, ParseMode::Lenient),
        Ok(ApplyPatchArgs {
            hunks: expected_patch.clone(),
            patch: patch_text.to_string(),
            workdir: None,
        })
    );

    let patch_text_in_single_quoted_heredoc = format!("<<'EOF'\n{patch_text}\nEOF\n");
    assert_eq!(
        parse_patch_text(&patch_text_in_single_quoted_heredoc, ParseMode::Strict),
        Err(expected_error.clone())
    );
    assert_eq!(
        parse_patch_text(&patch_text_in_single_quoted_heredoc, ParseMode::Lenient),
        Ok(ApplyPatchArgs {
            hunks: expected_patch.clone(),
            patch: patch_text.to_string(),
            workdir: None,
        })
    );

    let patch_text_in_double_quoted_heredoc = format!("<<\"EOF\"\n{patch_text}\nEOF\n");
    assert_eq!(
        parse_patch_text(&patch_text_in_double_quoted_heredoc, ParseMode::Strict),
        Err(expected_error.clone())
    );
    assert_eq!(
        parse_patch_text(&patch_text_in_double_quoted_heredoc, ParseMode::Lenient),
        Ok(ApplyPatchArgs {
            hunks: expected_patch,
            patch: patch_text.to_string(),
            workdir: None,
        })
    );

    let patch_text_in_mismatched_quotes_heredoc = format!("<<\"EOF'\n{patch_text}\nEOF\n");
    assert_eq!(
        parse_patch_text(&patch_text_in_mismatched_quotes_heredoc, ParseMode::Strict),
        Err(expected_error.clone())
    );
    assert_eq!(
        parse_patch_text(&patch_text_in_mismatched_quotes_heredoc, ParseMode::Lenient),
        Err(expected_error.clone())
    );

    let patch_text_with_missing_closing_heredoc =
        "<<EOF\n*** Begin Patch\n*** Update File: file2.py\nEOF\n".to_string();
    assert_eq!(
        parse_patch_text(&patch_text_with_missing_closing_heredoc, ParseMode::Strict),
        Err(expected_error)
    );
    assert_eq!(
        parse_patch_text(&patch_text_with_missing_closing_heredoc, ParseMode::Lenient),
        Err(InvalidPatchError(
            "The last line of the patch must be '*** End Patch'".to_string()
        ))
    );
}
