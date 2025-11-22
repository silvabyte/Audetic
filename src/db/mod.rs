mod init;
mod operations;
mod schemas;

#[cfg(test)]
mod tests;

// Re-export public API
pub use init::{init_db, migrate};
pub use operations::{
    count_workflows, get_recent_workflows, insert_workflow, prune_old_workflows, search_workflows,
};
pub use schemas::{VoiceToTextData, Workflow, WorkflowData, WorkflowType};
