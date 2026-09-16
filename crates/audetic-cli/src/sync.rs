//! CLI consumer for local synchronization administration.

use anyhow::{bail, Context, Result};
use audetic_core::sync::{
    DeviceAddRequest, DeviceAddResponse, DeviceListResponse, DeviceRevokeResponse,
    HubEnableResponse, LocalPairRequest, LocalPairResponse, LocalUnpairResponse, SyncDevice,
    SyncStatusResponse,
};
use audetic_core::url::{api_url, paths, sync_device_path};
use dialoguer::Password;

use std::io::{self, BufRead};

use crate::args::{
    SyncCliArgs, SyncCommand, SyncDevicesCliArgs, SyncDevicesCommand, SyncHubCliArgs,
    SyncHubCommand,
};
use crate::client::{json_or_error, CONNECT_HINT};

pub async fn handle_sync_command(args: SyncCliArgs) -> Result<()> {
    match args.command {
        SyncCommand::Status => show_status().await,
        SyncCommand::Hub(args) => handle_hub_command(args).await,
        SyncCommand::Pair { hub, token_stdin } => pair(hub, token_stdin).await,
        SyncCommand::Unpair => unpair().await,
        SyncCommand::Devices(args) => handle_devices_command(args).await,
    }
}

async fn handle_hub_command(args: SyncHubCliArgs) -> Result<()> {
    match args.command {
        SyncHubCommand::Enable => enable_hub().await,
    }
}

async fn handle_devices_command(args: SyncDevicesCliArgs) -> Result<()> {
    match args.command {
        SyncDevicesCommand::List => list_devices().await,
        SyncDevicesCommand::Add { name } => add_device(name).await,
        SyncDevicesCommand::Revoke { device_id } => revoke_device(device_id).await,
    }
}

async fn show_status() -> Result<()> {
    let status = fetch_status().await?;
    println!("{}", render_status(&status));
    Ok(())
}

async fn fetch_status() -> Result<SyncStatusResponse> {
    let response = reqwest::Client::new()
        .get(api_url(paths::SYNC_STATUS))
        .send()
        .await
        .context(CONNECT_HINT)?;
    let body = json_or_error(response, "get sync status").await?;
    serde_json::from_value(body).context("Failed to parse sync status")
}

async fn enable_hub() -> Result<()> {
    let response = reqwest::Client::new()
        .post(api_url(paths::SYNC_HUB_ENABLE))
        .send()
        .await
        .context(CONNECT_HINT)?;
    let body = json_or_error(response, "enable sync Hub").await?;
    let enabled: HubEnableResponse =
        serde_json::from_value(body).context("Failed to parse Hub enable response")?;
    println!("{}", render_hub_enable(&enabled));
    Ok(())
}

async fn pair(hub_url: String, token_stdin: bool) -> Result<()> {
    let credential = if token_stdin {
        read_stdin_credential(io::stdin().lock())?
    } else {
        prompt_credential()?
    };
    let request = LocalPairRequest {
        hub_url,
        credential,
    };
    let response = reqwest::Client::new()
        .post(api_url(paths::SYNC_PAIR))
        .json(&request)
        .send()
        .await
        .context(CONNECT_HINT)?;
    let body = json_or_error(response, "pair with sync Hub").await?;
    let paired: LocalPairResponse =
        serde_json::from_value(body).context("Failed to parse sync pairing response")?;
    println!("{}", render_pair(&paired));
    Ok(())
}

async fn unpair() -> Result<()> {
    let response = reqwest::Client::new()
        .delete(api_url(paths::SYNC_PAIR))
        .send()
        .await
        .context(CONNECT_HINT)?;
    let body = json_or_error(response, "unpair from sync Hub").await?;
    let unpaired: LocalUnpairResponse =
        serde_json::from_value(body).context("Failed to parse sync unpair response")?;
    println!("{}", render_unpair(&unpaired));
    Ok(())
}

async fn list_devices() -> Result<()> {
    let response = reqwest::Client::new()
        .get(api_url(paths::SYNC_DEVICES))
        .send()
        .await
        .context(CONNECT_HINT)?;
    let body = json_or_error(response, "list sync devices").await?;
    let devices: DeviceListResponse =
        serde_json::from_value(body).context("Failed to parse sync device list")?;
    println!("{}", render_device_list(&devices));
    Ok(())
}

async fn add_device(name: String) -> Result<()> {
    let response = reqwest::Client::new()
        .post(api_url(paths::SYNC_DEVICES))
        .json(&DeviceAddRequest { name })
        .send()
        .await
        .context(CONNECT_HINT)?;
    let body = json_or_error(response, "add sync device").await?;
    let added: DeviceAddResponse =
        serde_json::from_value(body).context("Failed to parse added sync device")?;
    println!("{}", render_device_add(&added));
    Ok(())
}

async fn revoke_device(device_id: String) -> Result<()> {
    let response = reqwest::Client::new()
        .delete(api_url(&sync_device_path(&device_id)))
        .send()
        .await
        .context(CONNECT_HINT)?;
    let body = json_or_error(response, "revoke sync device").await?;
    let revoked: DeviceRevokeResponse =
        serde_json::from_value(body).context("Failed to parse revoked sync device")?;
    println!("{}", render_device_revoke(&revoked));
    Ok(())
}

fn prompt_credential() -> Result<String> {
    let credential = Password::new()
        .with_prompt("Hub credential")
        .interact()
        .context("Failed to read Hub credential")?;
    if credential.is_empty() {
        bail!("Hub credential must not be empty");
    }
    Ok(credential)
}

fn read_stdin_credential(mut input: impl BufRead) -> Result<String> {
    let mut credential = String::new();
    input
        .read_line(&mut credential)
        .context("Failed to read Hub credential from stdin")?;
    credential.truncate(credential.trim_end_matches(['\r', '\n']).len());
    if credential.is_empty() {
        bail!("Hub credential from stdin must not be empty");
    }
    Ok(credential)
}

fn render_status(status: &SyncStatusResponse) -> String {
    let mut lines = vec![
        "Audetic Sync Status".to_string(),
        "===================".to_string(),
        format!("Role: {}", status.role.as_str()),
        format!("Node ID: {}", status.node_id),
    ];
    if let Some(hub) = &status.paired_hub {
        lines.extend([
            format!("Hub: {}", hub.hub_url),
            format!("Hub node ID: {}", hub.hub_node_id),
            format!("Device ID: {}", hub.device_id),
            format!("Protocol version: {}", hub.protocol_version),
            format!("Paired at: {}", hub.paired_at),
        ]);
    } else {
        lines.push("Hub: not paired".to_string());
    }
    lines.join("\n")
}

fn render_hub_enable(response: &HubEnableResponse) -> String {
    format!(
        "Sync Hub enabled.\nRole: {}\n{}",
        response.role.as_str(),
        render_restart_notice(response.restart_required)
    )
}

fn render_pair(response: &LocalPairResponse) -> String {
    format!(
        "Paired with Hub.\nRole: {}\nHub: {}\nHub node ID: {}\nDevice ID: {}\nProtocol version: {}\n{}",
        response.role.as_str(),
        response.paired_hub.hub_url,
        response.paired_hub.hub_node_id,
        response.paired_hub.device_id,
        response.paired_hub.protocol_version,
        render_restart_notice(response.restart_required)
    )
}

fn render_unpair(response: &LocalUnpairResponse) -> String {
    let credential_notice = if response.hub_credential_revoked {
        "The Hub credential was revoked."
    } else {
        "The Hub credential was not revoked. Revoke the device on the Hub to invalidate it."
    };
    format!(
        "Unpaired from Hub.\nRole: {}\n{credential_notice}\n{}",
        response.role.as_str(),
        render_restart_notice(response.restart_required)
    )
}

fn render_restart_notice(restart_required: bool) -> &'static str {
    if restart_required {
        "Restart required: restart audeticd to activate the new synchronization role."
    } else {
        "Restart required: no; the synchronization role is already active."
    }
}

fn render_device_list(response: &DeviceListResponse) -> String {
    if response.devices.is_empty() {
        return "No sync devices configured.".to_string();
    }
    response
        .devices
        .iter()
        .map(render_device)
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn render_device(device: &SyncDevice) -> String {
    let state = if device.revoked {
        "revoked"
    } else if device.paired {
        "paired"
    } else {
        "pending"
    };
    let mut lines = vec![
        format!("{} ({})", device.name, device.device_id),
        format!("  State: {state}"),
        format!("  Created at: {}", device.created_at),
    ];
    if let Some(client_node_id) = &device.client_node_id {
        lines.push(format!("  Client node ID: {client_node_id}"));
    }
    if let Some(paired_at) = &device.paired_at {
        lines.push(format!("  Paired at: {paired_at}"));
    }
    if let Some(revoked_at) = &device.revoked_at {
        lines.push(format!("  Revoked at: {revoked_at}"));
    }
    lines.join("\n")
}

fn render_device_add(response: &DeviceAddResponse) -> String {
    format!(
        "Device added: {}\nDevice ID: {}\nCredential: {}\nSave this credential now; it will not be shown again.",
        response.device.name, response.device.device_id, response.credential
    )
}

fn render_device_revoke(response: &DeviceRevokeResponse) -> String {
    format!(
        "Device revoked: {}\nDevice ID: {}",
        response.device.name, response.device.device_id
    )
}

#[cfg(test)]
mod tests {
    use audetic_core::config::SyncRole;
    use audetic_core::sync::{PairedHub, SYNC_PROTOCOL_VERSION};

    use super::*;

    const SECRET: &str = "audetic_sync_secret-value";

    fn device() -> SyncDevice {
        SyncDevice {
            device_id: "257dc5a6-d8d7-463b-8f2f-f95f616c3a15".to_string(),
            name: "Work laptop".to_string(),
            client_node_id: Some("67e55044-10b1-426f-9247-bb680e5fe0c8".to_string()),
            paired: true,
            revoked: false,
            created_at: "2026-09-16T18:00:00Z".to_string(),
            paired_at: Some("2026-09-16T18:01:00Z".to_string()),
            revoked_at: None,
        }
    }

    fn paired_hub() -> PairedHub {
        PairedHub {
            hub_url: "https://sync.example.com".to_string(),
            hub_node_id: "350e8400-e29b-41d4-a716-446655440000".to_string(),
            device_id: "257dc5a6-d8d7-463b-8f2f-f95f616c3a15".to_string(),
            protocol_version: SYNC_PROTOCOL_VERSION,
            paired_at: "2026-09-16T18:01:00Z".to_string(),
        }
    }

    #[test]
    fn status_output_contains_role_and_node_identity() {
        let output = render_status(&SyncStatusResponse {
            role: SyncRole::Client,
            node_id: "67e55044-10b1-426f-9247-bb680e5fe0c8".to_string(),
            paired_hub: Some(paired_hub()),
        });

        assert!(output.contains("Role: client"));
        assert!(output.contains("Node ID: 67e55044-10b1-426f-9247-bb680e5fe0c8"));
        assert!(output.contains("Hub: https://sync.example.com"));
    }

    #[test]
    fn stdin_credential_trims_only_line_endings() {
        assert_eq!(
            read_stdin_credential("  secret value  \r\nignored".as_bytes()).unwrap(),
            "  secret value  "
        );
        assert_eq!(
            read_stdin_credential("secret-without-newline".as_bytes()).unwrap(),
            "secret-without-newline"
        );
    }

    #[test]
    fn stdin_credential_rejects_empty_input() {
        assert!(read_stdin_credential("\r\n".as_bytes()).is_err());
        assert!(read_stdin_credential("".as_bytes()).is_err());
    }

    #[test]
    fn sync_commands_use_only_local_daemon_urls() {
        assert_eq!(
            api_url(paths::SYNC_STATUS),
            "http://127.0.0.1:3737/api/sync/status"
        );
        assert_eq!(
            api_url(paths::SYNC_HUB_ENABLE),
            "http://127.0.0.1:3737/api/sync/hub/enable"
        );
        assert_eq!(
            api_url(paths::SYNC_PAIR),
            "http://127.0.0.1:3737/api/sync/pair"
        );
        assert_eq!(
            api_url(paths::SYNC_DEVICES),
            "http://127.0.0.1:3737/api/sync/devices"
        );
        assert_eq!(
            api_url(&sync_device_path("device-id")),
            "http://127.0.0.1:3737/api/sync/devices/device-id"
        );
    }

    #[test]
    fn local_pair_request_contains_hub_url_and_credential() {
        let request = LocalPairRequest {
            hub_url: "https://sync.example.com".to_string(),
            credential: SECRET.to_string(),
        };

        assert_eq!(
            serde_json::to_value(request).unwrap(),
            serde_json::json!({
                "hub_url": "https://sync.example.com",
                "credential": SECRET,
            })
        );
    }

    #[test]
    fn device_add_prints_credential_exactly_once() {
        let output = render_device_add(&DeviceAddResponse {
            device: device(),
            credential: SECRET.to_string(),
        });

        assert_eq!(output.matches(SECRET).count(), 1);
        assert!(output.contains("it will not be shown again"));
    }

    #[test]
    fn non_add_rendering_does_not_expose_secrets_or_hashes() {
        let outputs = [
            render_status(&SyncStatusResponse {
                role: SyncRole::Client,
                node_id: "67e55044-10b1-426f-9247-bb680e5fe0c8".to_string(),
                paired_hub: Some(paired_hub()),
            }),
            render_hub_enable(&HubEnableResponse {
                role: SyncRole::Hub,
                restart_required: true,
            }),
            render_pair(&LocalPairResponse {
                role: SyncRole::Client,
                paired_hub: paired_hub(),
                restart_required: true,
            }),
            render_unpair(&LocalUnpairResponse {
                role: SyncRole::Standalone,
                restart_required: true,
                hub_credential_revoked: false,
            }),
            render_device_list(&DeviceListResponse {
                devices: vec![device()],
            }),
            render_device_revoke(&DeviceRevokeResponse { device: device() }),
        ];

        for output in outputs {
            assert!(!output.contains(SECRET));
            assert!(!output.contains("credential_hash"));
            assert!(!output.contains("bearer_credential"));
        }
    }

    #[test]
    fn role_changes_render_clear_restart_guidance() {
        assert_eq!(
            render_restart_notice(true),
            "Restart required: restart audeticd to activate the new synchronization role."
        );
        assert!(render_restart_notice(false).starts_with("Restart required: no"));
    }

    #[test]
    fn unpair_explains_that_hub_credential_is_not_revoked() {
        let output = render_unpair(&LocalUnpairResponse {
            role: SyncRole::Standalone,
            restart_required: true,
            hub_credential_revoked: false,
        });

        assert!(output.contains("Hub credential was not revoked"));
        assert!(output.contains("Revoke the device on the Hub"));
    }
}
