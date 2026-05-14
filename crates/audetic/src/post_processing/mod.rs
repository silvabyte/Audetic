//! Post-processing jobs: user-defined actions fired on daemon events.
//!
//! A `Job` ties an [`Event`] name to an [`Action`] (today: a shell command).
//! The [`PostProcessingService`] is the public entry point used by the rest
//! of the daemon — call [`PostProcessingService::dispatch`] from inside a
//! state machine (e.g. after a meeting is fully transcribed) and it
//! fans out to every enabled job for that event, fire-and-forget.
//!
//! Jobs are persisted in the `post_processing_jobs` SQLite table. The
//! REST API (`/api/post-processing/*`), CLI (`audetic post-processing …`),
//! and web UI all manage jobs through [`JobRepository`].

pub mod action;
pub mod event;
pub mod executors;
pub mod job;
pub mod repository;
pub mod runner;

pub use action::Action;
pub use event::{
    DictationCompletedPayload, Event, EventKind, MeetingCompletedPayload, ALL_EVENT_KINDS,
};
pub use job::{Job, NewJob, UpdateJob};
pub use repository::JobRepository;
pub use runner::PostProcessingService;
