//! CLI consumer for the daemon's unified setup assessment.

use std::io::{self, IsTerminal};
use std::process::Command;

use anyhow::{Context, Result};
use audetic_core::keybind::KeybindTarget;
use audetic_core::setup::{SetupAssessment, SetupCapabilityId, SetupState};
use audetic_core::url::{api_url, app_url, paths};
use dialoguer::{theme::ColorfulTheme, Select};

use crate::args::{KeybindCliArgs, KeybindCommand, ProviderCliArgs};
use crate::client::{json_or_error, CONNECT_HINT};

pub async fn handle_setup_command() -> Result<()> {
    let interactive = io::stdin().is_terminal() && io::stdout().is_terminal();

    loop {
        let assessment = fetch_assessment().await?;
        println!("{}", render_summary(&assessment));

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
            SetupAction::Recheck => continue,
            SetupAction::OpenBrowser => open_setup_page()?,
            SetupAction::Exit => return Ok(()),
        }
    }
}

async fn fetch_assessment() -> Result<SetupAssessment> {
    let response = reqwest::Client::new()
        .get(api_url(paths::SETUP))
        .send()
        .await
        .context(CONNECT_HINT)?;
    let body = json_or_error(response, "get setup assessment").await?;
    serde_json::from_value(body).context("Failed to parse setup assessment")
}

fn render_summary(assessment: &SetupAssessment) -> String {
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

    lines.join("\n")
}

fn state_label(state: SetupState) -> &'static str {
    match state {
        SetupState::Ready => "READY",
        SetupState::NeedsAction => "NEEDS ACTION",
        SetupState::Unavailable => "UNAVAILABLE",
        SetupState::NotApplicable => "N/A",
    }
}

#[derive(Debug, Clone, Copy)]
enum SetupAction {
    Provider,
    Keybind(KeybindTarget),
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
        SetupAction::Recheck,
        SetupAction::OpenBrowser,
        SetupAction::Exit,
    ]);
    labels.extend([
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

    #[test]
    fn summary_is_concise_and_includes_actionable_command() {
        let summary = render_summary(&assessment());

        assert!(summary.contains("dictation READY | meetings NEEDS ACTION"));
        assert!(summary.contains("[!!] meeting_audio"));
        assert!(summary.contains("-> Install the missing tools"));
        assert!(summary.contains("pacman -S --needed pipewire-audio"));
        assert!(!summary.contains("sudo"));
    }

    #[test]
    fn summary_distinguishes_saved_provider_from_active_runtime() {
        let mut assessment = assessment();
        assessment.restart_required = true;

        let summary = render_summary(&assessment);
        assert!(summary.contains("Saved provider differs from the active daemon"));
        assert!(summary.contains("restart audeticd"));
    }
}
