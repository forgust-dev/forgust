//! What a review job leaves behind (`artifacts:reports:ai_review`): the
//! report of the contract with the reviewer image (forgust-dev/forgust-reviewer,
//! its README), kept here as the server reads it. The job is not trusted: the
//! server checks every comment against the merge request before posting it.
//! A field the image adds later is ignored; `version` changes only when the
//! report does in a way this server cannot read.

use serde::{Deserialize, Serialize};

/// The file the job writes, uploaded as its report.
pub const FILE: &str = "review.json";
pub const VERSION: u32 = 1;
/// Largest report the server reads.
pub const MAX_BYTES: usize = 1 << 20;
/// Longest summary and comment the server posts (longer ones are cut, marked).
pub const MAX_SUMMARY_CHARS: usize = 4000;
pub const MAX_COMMENT_CHARS: usize = 4000;
/// A suggested change replaces at most this many lines, with at most this much code.
pub const MAX_SUGGESTION_LINES: u32 = 20;
pub const MAX_SUGGESTION_CHARS: usize = 4000;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Report {
    pub version: u32,
    /// What was reviewed: `base_sha..head_sha`.
    pub base_sha: String,
    pub head_sha: String,
    pub model: String,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub comments: Vec<Comment>,
    /// Files the model was not shown, and why.
    #[serde(default)]
    pub skipped: Vec<Skipped>,
    /// The review could not be made (the endpoint failed, nothing usable came back).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// A comment on one line: `new_line` (a line of the new file, added or kept)
/// or `old_line` (a removed line of the old file).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Comment {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new_line: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_line: Option<u32>,
    pub body: String,
    /// A change ready to apply, on lines of the new file (`new_line` comments only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suggestion: Option<Suggestion>,
}

/// Replace lines `from_line..=to_line` of the new file, which read `original`
/// (checked by the server against the file), with `code`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Suggestion {
    pub from_line: u32,
    pub to_line: u32,
    pub original: String,
    /// The replacement; empty deletes the lines.
    pub code: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Skipped {
    pub path: String,
    pub reason: String,
}
