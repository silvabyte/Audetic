//! CLI consumer for daemon-owned setup and Library Sync state.

use anyhow::{bail, Context, Result};
use audetic_core::keybind::KeybindTarget;
use audetic_core::setup::{SetupAssessment, SetupCapabilityId, SetupState};
use audetic_core::sync::{
    CacheLevel, HubCandidate, HubConnection, HubId, SyncRole, SyncSetupRequest, SyncSetupResult,
    SyncStatus,
};
use audetic_core::url::{api_url, app_url, paths};
use dialoguer::{theme::ColorfulTheme, Confirm, Input, Select};

use std::io::{self, IsTerminal};
use std::process::Command;

use crate::args::{KeybindCliArgs, KeybindCommand, ProviderCliArgs, SetupCliArgs, SyncRoleArg};
use crate::client::{json_or_error, CONNECT_HINT};

pub async fn handle_setup_command(args: SetupCliArgs) -> Result<()> {
    let interactive = io::stdin().is_terminal() && io::stdout().is_terminal();

    if let Some(role) = args.sync_role {
        configure_from_args(args, role).await?;
        return Ok(());
    }

    loop {
        let assessment = fetch_assessment().await?;
        let sync_status = fetch_sync_status().await?;
        println!("{}", render_summary(&assessment, &sync_status));

        if !interactive {
            return Ok(());
        }

        match choose_action(&assessment)? {
            SetupAction::Provider => {
                println!("Opening `audetic provider`...");
                crate::provider::handle_provider_command(ProviderCliArgs { command: None }).await?;
            }
            SetupAction::Keybind(target) => {
                println!("Running `audetic keybind install --target {target}`...");
                crate::keybind::handle_keybind_command(KeybindCliArgs {
                    command: Some(KeybindCommand::Install {
                        target,
                        key: None,
                        dry_run: false,
                    }),
                })
                .await?;
            }
            SetupAction::LibrarySync => configure_library_sync(&sync_status).await?,
            SetupAction::Recheck => continue,
            SetupAction::OpenBrowser => open_setup_page()?,
            SetupAction::Exit => return Ok(()),
        }
    }
}

async fn configure_from_args(args: SetupCliArgs, role: SyncRoleArg) -> Result<()> {
    let current = fetch_sync_status().await?;
    if role != SyncRoleArg::ConnectedDevice {
        reject_hub_arguments(&args)?;
    }
    let device_name = args.device_name.or(current.device_name.clone());
    let (role, hub) = match role {
        SyncRoleArg::Standalone => (SyncRole::Standalone, None),
        SyncRoleArg::HomeHub => (SyncRole::HomeHub, None),
        SyncRoleArg::ConnectedDevice => {
            let base_url = args.hub_url.context(
                "--sync-role connected-device requires --hub-url from the Home Hub command",
            )?;
            let hub_id = args.hub_id.context(
                "--sync-role connected-device requires --hub-id from the Home Hub command",
            )?;
            let owner_login = current.network.owner_login.clone().context(
                "The daemon could not determine the local Tailscale login; fix Tailscale and retry",
            )?;
            (
                SyncRole::ConnectedDevice,
                Some(HubConnection {
                    base_url,
                    hub_id,
                    owner_login,
                }),
            )
        }
    };

    let result = configure_sync(sync_request(&current, role, device_name, hub, false)).await?;
    println!("{}", render_sync_result(&result));
    if role == SyncRole::HomeHub && result.status.role != SyncRole::HomeHub {
        println!(
            "Home Hub activation was previewed only. Run `audetic setup` interactively to review and explicitly confirm the Tailscale Serve change."
        );
    }
    Ok(())
}

fn reject_hub_arguments(args: &SetupCliArgs) -> Result<()> {
    if args.hub_url.is_some() || args.hub_id.is_some() {
        bail!("--hub-url and --hub-id are valid only with --sync-role connected-device");
    }
    Ok(())
}

async fn fetch_assessment() -> Result<SetupAssessment> {
    get_json(paths::SETUP, "get setup assessment").await
}

async fn fetch_sync_status() -> Result<SyncStatus> {
    get_json(paths::SYNC_STATUS, "get Library Sync status").await
}

async fn discover_hubs() -> Result<SyncSetupResult> {
    post_empty_json(paths::SYNC_DISCOVER, "discover Home Hubs").await
}

async fn configure_sync(request: SyncSetupRequest) -> Result<SyncSetupResult> {
    let response = reqwest::Client::new()
        .post(api_url(paths::SYNC_CONFIGURE))
        .json(&request)
        .send()
        .await
        .context(CONNECT_HINT)?;
    let body = json_or_error(response, "configure Library Sync").await?;
    serde_json::from_value(body).context("Failed to parse Library Sync setup result")
}

async fn get_json<T: serde::de::DeserializeOwned>(path: &str, operation: &str) -> Result<T> {
    let response = reqwest::Client::new()
        .get(api_url(path))
        .send()
        .await
        .context(CONNECT_HINT)?;
    let body = json_or_error(response, operation).await?;
    serde_json::from_value(body).with_context(|| format!("Failed to parse {operation}"))
}

async fn post_empty_json<T: serde::de::DeserializeOwned>(path: &str, operation: &str) -> Result<T> {
    let response = reqwest::Client::new()
        .post(api_url(path))
        .send()
        .await
        .context(CONNECT_HINT)?;
    let body = json_or_error(response, operation).await?;
    serde_json::from_value(body).with_context(|| format!("Failed to parse {operation}"))
}

fn render_summary(assessment: &SetupAssessment, sync_status: &SyncStatus) -> String {
    let mut lines = vec![
        "Audetic Setup".to_string(),
        "=============".to_string(),
        format!(
            "Platform: {} {}{}",
            assessment.platform.os,
            assessment.platform.architecture,
            assessment
                .platform
                .distribution
                .as_ref()
                .map(|distribution| format!(" ({distribution})"))
                .unwrap_or_default()
        ),
        format!(
            "Readiness: dictation {} | meetings {}",
            state_label(assessment.workflows.dictation),
            state_label(assessment.workflows.meetings)
        ),
        String::new(),
    ];

    if assessment.restart_required {
        lines.push(
            "[!!] provider_restart         Saved provider differs from the active daemon; restart audeticd"
                .to_string(),
        );
    }

    for capability in &assessment.capabilities {
        let marker = match capability.state {
            SetupState::Ready => "ok",
            SetupState::NotApplicable => "--",
            SetupState::NeedsAction => "!!",
            SetupState::Unavailable => "??",
        };
        let mut line = format!(
            "[{marker}] {:<24} {}",
            capability.id.as_str(),
            capability.summary
        );
        if let Some(detail) = &capability.detail {
            line.push_str(&format!(" ({detail})"));
        }
        lines.push(line);

        if !capability.state.is_ready() {
            if let Some(action) = &capability.action {
                lines.push(format!("     -> {action}"));
            }
        }
    }

    if let Some(command) = &assessment.arch_package_command {
        lines.extend([
            String::new(),
            "Missing Arch packages (copy and run with appropriate privileges):".to_string(),
            format!("  {command}"),
        ]);
    }

    lines.extend([String::new(), render_sync_status(sync_status)]);
    lines.join("\n")
}

fn render_sync_status(status: &SyncStatus) -> String {
    let role = role_label(status.role);
    let device_name = status.device_name.as_deref().unwrap_or("not set");
    let network_state = if status.network.ready {
        "READY"
    } else {
        "NEEDS ACTION"
    };
    let mut lines = vec![
        "Library Sync".to_string(),
        "------------".to_string(),
        format!("Role: {role} | Device: {device_name}"),
        format!(
            "Tailscale: {network_state} | Login: {}",
            status
                .network
                .owner_login
                .as_deref()
                .unwrap_or("unavailable")
        ),
        format!(
            "Pending: {} item(s), {} byte(s)",
            status.pending_items, status.pending_bytes
        ),
        format!(
            "Recording Payload uploads: {}",
            enabled_label(status.upload_recording_payloads)
        ),
        format!("Library Cache: {}", cache_label(status.cache_level)),
        format!(
            "Shared Configuration: {}{}",
            enabled_label(status.shared_config_enabled),
            status
                .applied_shared_config_version
                .map(|version| format!(" (version {version})"))
                .unwrap_or_default()
        ),
    ];

    if let Some(hub) = &status.hub {
        lines.push(format!(
            "Home Hub: {} ({})",
            if status.hub_reachable {
                "REACHABLE"
            } else {
                "UNREACHABLE"
            },
            hub.base_url
        ));
    } else if status.role == SyncRole::HomeHub {
        lines.push(format!(
            "Home Hub: {}",
            if status.hub_reachable {
                "SERVING"
            } else {
                "NOT REACHABLE"
            }
        ));
    }
    if let Some(last_contact) = &status.last_contact_at {
        lines.push(format!("Last contact: {last_contact}"));
    }
    if let Some(error) = status.last_error.as_ref().or(status.network.error.as_ref()) {
        lines.push(format!("Last error: {error}"));
        lines.push("  -> Fix the reported network or hub issue, then recheck setup.".to_string());
    }
    lines.join("\n")
}

fn render_sync_result(result: &SyncSetupResult) -> String {
    let mut output = render_sync_status(&result.status);
    if let Some(preview) = &result.serve_preview {
        output.push_str("\nTailscale Serve preview:\n  ");
        output.push_str(preview);
    }
    if let Some(command) = &result.setup_command {
        output.push_str("\nConnected Device setup command:\n  ");
        output.push_str(command);
    }
    output
}

fn state_label(state: SetupState) -> &'static str {
    match state {
        SetupState::Ready => "READY",
        SetupState::NeedsAction => "NEEDS ACTION",
        SetupState::Unavailable => "UNAVAILABLE",
        SetupState::NotApplicable => "N/A",
    }
}

fn role_label(role: SyncRole) -> &'static str {
    match role {
        SyncRole::Standalone => "Standalone",
        SyncRole::HomeHub => "Home Hub",
        SyncRole::ConnectedDevice => "Connected Device",
    }
}

fn cache_label(cache_level: CacheLevel) -> &'static str {
    match cache_level {
        CacheLevel::LiveOnly => "Live Only",
        CacheLevel::TextForOfflineUse => "Text for Offline Use",
        CacheLevel::TextAndAvailableAudio => "Text + Available Audio",
    }
}

fn enabled_label(enabled: bool) -> &'static str {
    if enabled {
        "enabled"
    } else {
        "disabled"
    }
}

#[derive(Debug, Clone, Copy)]
enum SetupAction {
    Provider,
    Keybind(KeybindTarget),
    LibrarySync,
    Recheck,
    OpenBrowser,
    Exit,
}

fn choose_action(assessment: &SetupAssessment) -> Result<SetupAction> {
    let mut actions = Vec::new();
    let mut labels = Vec::new();

    if assessment
        .capability(SetupCapabilityId::TranscriptionProvider)
        .is_some_and(|capability| !capability.state.is_ready())
    {
        actions.push(SetupAction::Provider);
        labels.push("Configure provider (`audetic provider`)".to_string());
    }
    if assessment
        .capability(SetupCapabilityId::DictationKeybind)
        .is_some_and(|capability| capability.state == SetupState::NeedsAction)
    {
        actions.push(SetupAction::Keybind(KeybindTarget::Dictation));
        labels.push("Install dictation keybind (`audetic keybind install`)".to_string());
    }
    if assessment
        .capability(SetupCapabilityId::MeetingKeybind)
        .is_some_and(|capability| capability.state == SetupState::NeedsAction)
    {
        actions.push(SetupAction::Keybind(KeybindTarget::Meeting));
        labels.push(
            "Install meeting keybind (`audetic keybind install --target meeting`)".to_string(),
        );
    }

    actions.extend([
        SetupAction::LibrarySync,
        SetupAction::Recheck,
        SetupAction::OpenBrowser,
        SetupAction::Exit,
    ]);
    labels.extend([
        "Configure Library Sync".to_string(),
        "Recheck capabilities".to_string(),
        "Open setup page in browser".to_string(),
        "Exit".to_string(),
    ]);

    println!();
    let selection = Select::with_theme(&ColorfulTheme::default())
        .with_prompt("Setup action")
        .items(&labels)
        .default(0)
        .interact()?;
    Ok(actions[selection])
}

async fn configure_library_sync(current: &SyncStatus) -> Result<()> {
    let labels = [
        "Discover and connect to a Home Hub",
        "Make this device the Home Hub",
        "Use Standalone mode",
        "Back",
    ];
    println!();
    let selection = Select::with_theme(&ColorfulTheme::default())
        .with_prompt("Library Sync action")
        .items(&labels)
        .default(0)
        .interact()?;

    match selection {
        0 => discover_and_connect(current).await,
        1 => preview_and_enable_home_hub(current).await,
        2 => configure_standalone(current).await,
        _ => Ok(()),
    }
}

async fn discover_and_connect(current: &SyncStatus) -> Result<()> {
    println!("Discovering compatible Home Hubs through the local daemon...");
    let result = discover_hubs().await?;
    print_discovery(&result);

    let candidate = match result.discovered_hubs.as_slice() {
        [candidate] => {
            println!(
                "Exactly one compatible Home Hub found; selecting {}.",
                candidate_label(candidate)
            );
            Some(candidate.clone())
        }
        [] => manual_hub_candidate(current, "No compatible Home Hub was discovered.")?,
        candidates => choose_discovered_hub_or_manual(current, candidates)?,
    };

    let Some(candidate) = candidate else {
        return Ok(());
    };
    let request = sync_request(
        current,
        SyncRole::ConnectedDevice,
        current.device_name.clone(),
        Some(candidate.connection),
        false,
    );
    let configured = configure_sync(request).await?;
    println!("{}", render_sync_result(&configured));
    Ok(())
}

fn print_discovery(result: &SyncSetupResult) {
    for candidate in &result.discovered_hubs {
        println!("  ✓ {}", candidate_label(candidate));
    }
    for failure in &result.discovery_failures {
        println!("  · {}: {}", failure.candidate, failure.reason);
    }
}

fn choose_discovered_hub_or_manual(
    current: &SyncStatus,
    candidates: &[HubCandidate],
) -> Result<Option<HubCandidate>> {
    let mut labels = candidates
        .iter()
        .map(candidate_label)
        .collect::<Vec<String>>();
    labels.push("Enter details from a generated setup command".to_string());
    labels.push("Back".to_string());
    println!(
        "Multiple compatible Home Hubs were found. Select one explicitly; Audetic will not guess."
    );
    let selection = Select::with_theme(&ColorfulTheme::default())
        .with_prompt("Home Hub")
        .items(&labels)
        .default(0)
        .interact()?;
    if selection < candidates.len() {
        Ok(Some(candidates[selection].clone()))
    } else if selection == candidates.len() {
        prompt_for_hub(current).map(Some)
    } else {
        Ok(None)
    }
}

fn manual_hub_candidate(current: &SyncStatus, reason: &str) -> Result<Option<HubCandidate>> {
    println!("{reason}");
    println!("On the Home Hub, copy the `audetic setup --sync-role connected-device ...` command.");
    let labels = ["Enter the generated command details", "Back"];
    let selection = Select::with_theme(&ColorfulTheme::default())
        .with_prompt("Connection fallback")
        .items(&labels)
        .default(0)
        .interact()?;
    if selection == 0 {
        prompt_for_hub(current).map(Some)
    } else {
        Ok(None)
    }
}

fn prompt_for_hub(current: &SyncStatus) -> Result<HubCandidate> {
    let owner_login = current.network.owner_login.clone().context(
        "The daemon could not determine the local Tailscale login; fix Tailscale and retry",
    )?;
    let theme = ColorfulTheme::default();
    let base_url: String = Input::with_theme(&theme)
        .with_prompt("Home Hub URL (--hub-url)")
        .interact_text()?;
    let hub_id_text: String = Input::with_theme(&theme)
        .with_prompt("Home Hub ID (--hub-id)")
        .validate_with(|value: &String| -> std::result::Result<(), String> {
            value.parse::<HubId>().map(|_| ())
        })
        .interact_text()?;
    let hub_id = hub_id_text.parse::<HubId>().map_err(anyhow::Error::msg)?;
    Ok(HubCandidate {
        connection: HubConnection {
            base_url,
            hub_id,
            owner_login,
        },
        device_name: None,
        protocol_version: 0,
    })
}

async fn preview_and_enable_home_hub(current: &SyncStatus) -> Result<()> {
    let preview = configure_sync(sync_request(
        current,
        SyncRole::HomeHub,
        current.device_name.clone(),
        None,
        false,
    ))
    .await?;
    println!("{}", render_sync_result(&preview));

    if preview.status.role == SyncRole::HomeHub {
        return Ok(());
    }
    println!("Audetic will add only the mapping shown above and will never enable Funnel.");
    let confirmed = Confirm::with_theme(&ColorfulTheme::default())
        .with_prompt("Apply this exact Tailscale Serve change and enable Home Hub?")
        .default(false)
        .interact()?;
    if !confirmed {
        println!("Home Hub setup cancelled; no Serve change was made.");
        return Ok(());
    }

    let configured = configure_sync(sync_request(
        current,
        SyncRole::HomeHub,
        current.device_name.clone(),
        None,
        true,
    ))
    .await?;
    println!("{}", render_sync_result(&configured));
    Ok(())
}

async fn configure_standalone(current: &SyncStatus) -> Result<()> {
    if current.role != SyncRole::Standalone {
        let confirmed = Confirm::with_theme(&ColorfulTheme::default())
            .with_prompt("Leave the current Library Sync role and use Standalone mode?")
            .default(false)
            .interact()?;
        if !confirmed {
            return Ok(());
        }
    }
    let configured = configure_sync(sync_request(
        current,
        SyncRole::Standalone,
        current.device_name.clone(),
        None,
        false,
    ))
    .await?;
    println!("{}", render_sync_result(&configured));
    Ok(())
}

fn sync_request(
    current: &SyncStatus,
    role: SyncRole,
    device_name: Option<String>,
    hub: Option<HubConnection>,
    confirm_serve_change: bool,
) -> SyncSetupRequest {
    SyncSetupRequest {
        role,
        device_name,
        hub,
        upload_recording_payloads: current.upload_recording_payloads,
        cache_level: current.cache_level,
        shared_config_enabled: current.shared_config_enabled,
        confirm_serve_change,
    }
}

fn candidate_label(candidate: &HubCandidate) -> String {
    format!(
        "{} — {} ({})",
        candidate.device_name.as_deref().unwrap_or("Home Hub"),
        candidate.connection.base_url,
        candidate.connection.hub_id
    )
}

fn open_setup_page() -> Result<()> {
    let url = format!("{}settings/setup", app_url());
    let opener = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };

    Command::new(opener)
        .arg(&url)
        .spawn()
        .with_context(|| format!("Could not open a browser. Open {url} manually"))?;
    println!("Opened {url}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use audetic_core::setup::{PlatformInfo, SetupCapability, ToolReadiness, WorkflowReadiness};
    use audetic_core::sync::{DeviceId, ServeMappingState, SyncNetworkAssessment};

    use super::*;

    fn assessment() -> SetupAssessment {
        SetupAssessment {
            state: SetupState::Ready,
            restart_required: false,
            platform: PlatformInfo {
                os: "linux".to_string(),
                architecture: "x86_64".to_string(),
                distribution: Some("Arch Linux".to_string()),
                arch_linux: true,
            },
            workflows: WorkflowReadiness {
                dictation: SetupState::Ready,
                meetings: SetupState::NeedsAction,
            },
            capabilities: vec![SetupCapability {
                id: SetupCapabilityId::MeetingAudio,
                state: SetupState::NeedsAction,
                required_for_dictation: false,
                required_for_meetings: true,
                summary: "Meeting audio tools incomplete".to_string(),
                detail: None,
                action: Some("Install the missing tools".to_string()),
                tools: vec![ToolReadiness {
                    id: "pw-cat".to_string(),
                    available: false,
                    path: None,
                    arch_package: Some("pipewire-audio".to_string()),
                }],
            }],
            missing_arch_packages: vec!["pipewire-audio".to_string()],
            arch_package_command: Some("pacman -S --needed pipewire-audio".to_string()),
        }
    }

    fn sync_status(role: SyncRole) -> SyncStatus {
        SyncStatus {
            device_id: DeviceId::new(),
            role,
            device_name: Some("Travel Laptop".into()),
            local_hub_id: None,
            hub: None,
            hub_reachable: false,
            last_contact_at: None,
            pending_items: 2,
            pending_bytes: 4096,
            last_error: None,
            upload_recording_payloads: false,
            cache_level: CacheLevel::LiveOnly,
            shared_config_enabled: false,
            applied_shared_config_version: None,
            network: SyncNetworkAssessment {
                ready: true,
                backend_state: Some("Running".into()),
                dns_name: Some("laptop.example.ts.net".into()),
                owner_login: Some("owner@example.com".into()),
                serve_mapping: Some(ServeMappingState::Vacant),
                funnel_enabled: Some(false),
                serve_preview: "tailscale serve preview".into(),
                error: None,
            },
        }
    }

    #[test]
    fn summary_is_concise_and_includes_actionable_command_and_sync_status() {
        let summary = render_summary(&assessment(), &sync_status(SyncRole::Standalone));

        assert!(summary.contains("dictation READY | meetings NEEDS ACTION"));
        assert!(summary.contains("[!!] meeting_audio"));
        assert!(summary.contains("-> Install the missing tools"));
        assert!(summary.contains("pacman -S --needed pipewire-audio"));
        assert!(summary.contains("Library Sync"));
        assert!(summary.contains("Role: Standalone | Device: Travel Laptop"));
        assert!(summary.contains("Tailscale: READY | Login: owner@example.com"));
        assert!(summary.contains("Pending: 2 item(s), 4096 byte(s)"));
        assert!(!summary.contains("sudo"));
    }

    #[test]
    fn summary_distinguishes_saved_provider_from_active_runtime() {
        let mut assessment = assessment();
        assessment.restart_required = true;

        let summary = render_summary(&assessment, &sync_status(SyncRole::Standalone));
        assert!(summary.contains("Saved provider differs from the active daemon"));
        assert!(summary.contains("restart audeticd"));
    }

    #[test]
    fn connected_status_renders_reachability_policy_and_corrective_action() {
        let mut status = sync_status(SyncRole::ConnectedDevice);
        status.hub = Some(HubConnection {
            base_url: "https://home.example.ts.net:8443/audetic/".into(),
            hub_id: HubId::new(),
            owner_login: "owner@example.com".into(),
        });
        status.last_error = Some("Home Hub is offline".into());
        status.shared_config_enabled = true;

        let rendered = render_sync_status(&status);

        assert!(rendered.contains("Role: Connected Device"));
        assert!(rendered.contains("Home Hub: UNREACHABLE"));
        assert!(rendered.contains("Recording Payload uploads: disabled"));
        assert!(rendered.contains("Library Cache: Live Only"));
        assert!(rendered.contains("Shared Configuration: enabled"));
        assert!(rendered.contains("Last error: Home Hub is offline"));
        assert!(rendered.contains("-> Fix the reported network or hub issue"));
    }

    #[test]
    fn home_hub_result_renders_exact_preview_and_generated_command() {
        let mut status = sync_status(SyncRole::HomeHub);
        status.hub_reachable = true;
        let result = SyncSetupResult {
            status,
            discovered_hubs: Vec::new(),
            discovery_failures: Vec::new(),
            setup_command: Some(
                "audetic setup --sync-role connected-device --hub-url https://home.example.ts.net:8443/audetic/ --hub-id 67e55044-10b1-426f-9247-bb680e5fe0c8"
                    .into(),
            ),
            serve_preview: Some(
                "tailscale serve --bg --https=8443 --set-path=/audetic http://127.0.0.1:3738"
                    .into(),
            ),
        };

        let rendered = render_sync_result(&result);

        assert!(rendered.contains("Tailscale Serve preview:\n  tailscale serve --bg"));
        assert!(rendered.contains(
            "Connected Device setup command:\n  audetic setup --sync-role connected-device"
        ));
    }

    #[test]
    fn sync_requests_preserve_current_policies_and_require_separate_serve_confirmation() {
        let mut current = sync_status(SyncRole::Standalone);
        current.upload_recording_payloads = true;
        current.cache_level = CacheLevel::TextForOfflineUse;
        current.shared_config_enabled = true;

        let preview = sync_request(
            &current,
            SyncRole::HomeHub,
            Some("Home".into()),
            None,
            false,
        );
        assert_eq!(preview.device_name.as_deref(), Some("Home"));
        assert!(preview.upload_recording_payloads);
        assert_eq!(preview.cache_level, CacheLevel::TextForOfflineUse);
        assert!(preview.shared_config_enabled);
        assert!(!preview.confirm_serve_change);

        let confirmed = sync_request(
            &current,
            SyncRole::HomeHub,
            current.device_name.clone(),
            None,
            true,
        );
        assert!(confirmed.confirm_serve_change);
    }
}
