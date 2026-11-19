//! The AI review job of Forgust (`forgust-review`, in its own image). It
//! holds no credential of the forge: it reads the clone the runner made, asks
//! the model endpoint the administrator set, and writes [`report::FILE`],
//! which the server checks and posts as notes of the project's bot. The
//! variables it reads and the report it writes are the contract with the
//! server: see the README. Without the `job` feature, only [`report`] (what
//! the server reads).

pub mod report;

#[cfg(feature = "job")]
mod job;
#[cfg(feature = "job")]
pub use job::*;
