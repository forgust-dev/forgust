//! What a review job leaves behind (`artifacts:reports:ai_review`), and the
//! server reads. The job is not trusted: the server checks every comment
//! against the merge request before posting it.

use serde::{Deserialize, Serialize};

/// The file the job writes, uploaded as its report.
pub const FILE: &str = "review.json";
pub const VERSION: u32 = 1;
/// Largest report the server reads.
pub const MAX_BYTES: usize = 1 << 20;
/// Longest summary and comment the server posts (longer ones are cut, marked).
pub const MAX_SUMMARY_CHARS: usize = 4000;
pub const MAX_COMMENT_CHARS: usize = 4000;

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
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Skipped {
    pub path: String,
    pub reason: String,
}
