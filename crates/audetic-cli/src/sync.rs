//! CLI consumer for local synchronization status.

use anyhow::{Context, Result};
use audetic_core::sync::SyncStatus;
use audetic_core::url::{api_url, paths};

use crate::args::{SyncCliArgs, SyncCommand};
use crate::client::{json_or_error, CONNECT_HINT};

pub async fn handle_sync_command(args: SyncCliArgs) -> Result<()> {
    match args.command {
        SyncCommand::Status => show_status().await,
    }
}

async fn show_status() -> Result<()> {
    let status = fetch_status().await?;
    println!("{}", render_status(&status));
    Ok(())
}

async fn fetch_status() -> Result<SyncStatus> {
    let response = reqwest::Client::new()
        .get(api_url(paths::SYNC_STATUS))
        .send()
        .await
        .context(CONNECT_HINT)?;
    let body = json_or_error(response, "get sync status").await?;
    serde_json::from_value(body).context("Failed to parse sync status")
}

fn render_status(status: &SyncStatus) -> String {
    format!(
        "Audetic Sync Status\n===================\nRole: {}\nNode ID: {}",
        status.role.as_str(),
        status.node_id
    )
}

#[cfg(test)]
mod tests {
    use audetic_core::config::SyncRole;

    use super::*;

    #[test]
    fn status_output_contains_role_and_node_identity() {
        let output = render_status(&SyncStatus {
            role: SyncRole::Standalone,
            node_id: "67e55044-10b1-426f-9247-bb680e5fe0c8".to_string(),
        });

        assert!(output.contains("Role: standalone"));
        assert!(output.contains("Node ID: 67e55044-10b1-426f-9247-bb680e5fe0c8"));
    }
}
