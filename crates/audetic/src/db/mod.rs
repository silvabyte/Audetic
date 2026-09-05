pub mod agent_profiles;
mod init;
pub mod meeting_artifacts;
pub mod meetings;
mod operations;
mod schemas;
pub mod shared_library;
pub mod sync_identity;
pub mod sync_outbox;
pub mod sync_serve;
pub mod sync_settings;

#[cfg(test)]
mod tests;

// Re-export public API
pub use init::{
    configure_connection, init_db, init_db_at, migrate, migrate_db, migrate_db_at, open_db,
    open_db_at,
};
pub use operations::{
    backfill_visible_dictations, backfill_visible_meetings, backfill_visible_records_batch,
    count_workflows, get_recent_workflows, get_workflow_by_sync_id, insert_workflow,
    insert_workflow_record, list_visible_workflows, prune_old_workflows, search_workflows,
};
pub(crate) use operations::{backfill_visible_records_batch_cancellable, BackfillCursor};
pub use schemas::{VoiceToTextData, Workflow, WorkflowData, WorkflowType};
