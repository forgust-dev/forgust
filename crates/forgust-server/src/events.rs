//! Event bus (ADR-0016): in-process broadcast by default. Readers must always be
//! able to rebuild state from the database; events are hints, not the record.

use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    /// Refs changed in a repo (push, branch deletion, maintenance).
    Push {
        project_id: i64,
        refs: Vec<String>,
    },
    ProjectDeleted {
        project_id: i64,
    },
    /// Repository content was replaced (restore): drop every cached result.
    RepoReset {
        project_id: i64,
    },
    /// Anything about a merge request changed (version, note, approval, state).
    MergeRequest {
        project_id: i64,
        iid: i64,
    },
    /// A pipeline's status changed (or one was created).
    Pipeline {
        project_id: i64,
        pipeline_id: i64,
        status: String,
    },
    /// A CI job changed state.
    Job {
        project_id: i64,
        pipeline_id: i64,
        job_id: i64,
        status: String,
        trace_size: i64,
    },
    /// A running job's log grew to `trace_size` bytes (only log viewers care).
    JobTrace {
        project_id: i64,
        job_id: i64,
        trace_size: i64,
    },
    /// Jobs became pending: wake runners waiting in `jobs/request`.
    JobsQueued {
        project_id: i64,
    },
}

impl Event {
    /// A job's state change.
    pub fn job(b: &forgust_db::ci::Build, status: &str) -> Event {
        Event::Job { project_id: b.project_id, pipeline_id: b.pipeline_id, job_id: b.id, status: status.into(), trace_size: b.trace_size }
    }

    pub fn project_id(&self) -> i64 {
        match self {
            Event::Push { project_id, .. }
            | Event::ProjectDeleted { project_id }
            | Event::RepoReset { project_id }
            | Event::MergeRequest { project_id, .. }
            | Event::Pipeline { project_id, .. }
            | Event::Job { project_id, .. }
            | Event::JobTrace { project_id, .. }
            | Event::JobsQueued { project_id } => *project_id,
        }
    }
}

pub trait EventBus: Send + Sync + 'static {
    fn publish(&self, ev: Event);
    fn subscribe(&self) -> broadcast::Receiver<Event>;
}

pub struct InProcessBus {
    tx: broadcast::Sender<Event>,
}

impl InProcessBus {
    pub fn new(capacity: usize) -> Self {
        let (tx, _) = broadcast::channel(capacity);
        InProcessBus { tx }
    }
}

impl EventBus for InProcessBus {
    fn publish(&self, ev: Event) {
        // no subscribers is fine
        let _ = self.tx.send(ev);
    }

    fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.tx.subscribe()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn delivers_to_subscribers() {
        let bus = InProcessBus::new(16);
        let mut rx = bus.subscribe();
        bus.publish(Event::Push { project_id: 1, refs: vec!["refs/heads/main".into()] });
        assert_eq!(rx.recv().await.unwrap().project_id(), 1);
    }
}
