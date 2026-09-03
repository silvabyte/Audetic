//! Read-only setup assessment for all daemon consumers.
//!
//! Probing and classification live here rather than in an HTTP handler so the
//! CLI and UI receive the same setup truth and the policy is unit-testable.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use audetic_core::keybind::KeybindTarget;
use audetic_core::setup::{
    PlatformInfo, SetupAssessment, SetupCapability, SetupCapabilityId, SetupState, ToolReadiness,
    WorkflowReadiness,
};

use crate::config::Config;
use crate::keybind::KeybindStatus;
use crate::transcription::{get_provider_status_from_config, ProviderStatus};

#[derive(Debug, Clone)]
enum ProviderProbe {
    Ready(String),
    NeedsAction(String),
}

#[derive(Debug, Clone)]
enum KeybindProbe {
    Installed(String),
    NotInstalled(Option<PathBuf>),
    NoConfig,
    Error(String),
}

#[derive(Debug, Clone)]
struct ProbeSnapshot {
    platform: PlatformInfo,
    omarchy_path: Option<PathBuf>,
    hyprland_session: bool,
    hyprland_config: Option<PathBuf>,
    provider: ProviderProbe,
    restart_required: bool,
    preferred_text_backend: Option<String>,
    wtype: Option<PathBuf>,
    ydotool: Option<PathBuf>,
    wl_copy: Option<PathBuf>,
    dictation_keybind: KeybindProbe,
    meeting_keybind: KeybindProbe,
    ffmpeg: Option<PathBuf>,
    pw_cat: Option<PathBuf>,
    pactl: Option<PathBuf>,
}

/// Probe the host and return the complete setup assessment. This function only
/// reads environment variables, files, and executable locations; it never
/// installs packages, invokes sudo, or changes configuration.
pub fn assess(active_provider: &crate::config::WhisperConfig) -> SetupAssessment {
    classify(probe(active_provider))
}

fn probe(active_provider: &crate::config::WhisperConfig) -> ProbeSnapshot {
    let platform = detect_platform();
    let config = load_config_read_only();
    let preferred_text_backend = config
        .as_ref()
        .ok()
        .map(|config| config.wayland.input_method.clone());

    let restart_required = config
        .as_ref()
        .map(|config| config.whisper != *active_provider)
        .unwrap_or(true);
    let provider = match get_provider_status_from_config(active_provider) {
        Ok(ProviderStatus::Ready {
            provider,
            model,
            language,
        }) => {
            let mut detail = provider;
            if let Some(model) = model {
                detail.push_str(&format!(" / {model}"));
            }
            if let Some(language) = language {
                detail.push_str(&format!(" / {language}"));
            }
            ProviderProbe::Ready(detail)
        }
        Ok(ProviderStatus::ConfigError { provider, error }) => {
            ProviderProbe::NeedsAction(format!("{provider}: {error}"))
        }
        Ok(ProviderStatus::NotConfigured) => {
            ProviderProbe::NeedsAction("No transcription provider is configured".to_string())
        }
        Err(error) => ProviderProbe::NeedsAction(error.to_string()),
    };

    let discovery = crate::keybind::discover_config().ok();
    let hyprland_config = discovery
        .as_ref()
        .and_then(|discovery| discovery.writable_config().cloned());
    let keybind_statuses = crate::keybind::get_statuses();
    let dictation_keybind = keybind_statuses
        .as_ref()
        .map(|statuses| probe_keybind_status(statuses.get(KeybindTarget::Dictation)))
        .unwrap_or_else(|error| KeybindProbe::Error(error.to_string()));
    let meeting_keybind = keybind_statuses
        .as_ref()
        .map(|statuses| probe_keybind_status(statuses.get(KeybindTarget::Meeting)))
        .unwrap_or_else(|error| KeybindProbe::Error(error.to_string()));

    ProbeSnapshot {
        platform,
        omarchy_path: detect_omarchy(),
        hyprland_session: detect_hyprland_session(),
        hyprland_config,
        provider,
        restart_required,
        preferred_text_backend,
        wtype: find_tool("wtype"),
        ydotool: find_tool("ydotool"),
        wl_copy: find_tool("wl-copy"),
        dictation_keybind,
        meeting_keybind,
        ffmpeg: audetic_core::ffmpeg::resolve_ffmpeg_binary(),
        pw_cat: find_tool("pw-cat"),
        pactl: find_tool("pactl"),
    }
}

fn probe_keybind_status(status: &KeybindStatus) -> KeybindProbe {
    match status {
        KeybindStatus::Installed { display_key, .. } => {
            KeybindProbe::Installed(display_key.clone())
        }
        KeybindStatus::NotInstalled { config_path, .. } => {
            KeybindProbe::NotInstalled(config_path.clone())
        }
        KeybindStatus::NoConfig { .. } => KeybindProbe::NoConfig,
    }
}

fn classify_keybind(
    linux: bool,
    probe: &KeybindProbe,
    label: &str,
) -> (SetupState, String, Option<String>) {
    if !linux {
        return (
            SetupState::NotApplicable,
            format!("{label} keybind is not used on this platform"),
            None,
        );
    }

    match probe {
        KeybindProbe::Installed(key) => (
            SetupState::Ready,
            format!("{label} keybind installed"),
            Some(key.clone()),
        ),
        KeybindProbe::NotInstalled(path) => (
            SetupState::NeedsAction,
            format!("{label} keybind not installed"),
            path.as_ref().map(|path| path.display().to_string()),
        ),
        KeybindProbe::NoConfig => (
            SetupState::NeedsAction,
            format!("{label} keybind cannot be installed yet"),
            Some("No Hyprland configuration was found".to_string()),
        ),
        KeybindProbe::Error(error) => (
            SetupState::Unavailable,
            format!("{label} keybind status unavailable"),
            Some(error.clone()),
        ),
    }
}

fn classify(snapshot: ProbeSnapshot) -> SetupAssessment {
    let linux = snapshot.platform.os == "linux";
    let mut capabilities = Vec::with_capacity(10);

    capabilities.push(capability(
        SetupCapabilityId::Omarchy,
        if !linux {
            SetupState::NotApplicable
        } else if snapshot.omarchy_path.is_some() {
            SetupState::Ready
        } else {
            SetupState::Unavailable
        },
        (false, false),
        if snapshot.omarchy_path.is_some() {
            "Omarchy detected"
        } else if linux {
            "Omarchy not detected"
        } else {
            "Omarchy is Linux-only"
        },
        snapshot
            .omarchy_path
            .as_ref()
            .map(|path| path.display().to_string()),
        None,
        vec![],
    ));

    capabilities.push(capability(
        SetupCapabilityId::HyprlandSession,
        linux_state(linux, snapshot.hyprland_session),
        (linux, false),
        if snapshot.hyprland_session {
            "Hyprland session detected"
        } else if linux {
            "Hyprland session not detected"
        } else {
            "Hyprland is not used on this platform"
        },
        None,
        linux.then(|| "Start Audetic from your Hyprland session".to_string()),
        vec![],
    ));

    capabilities.push(capability(
        SetupCapabilityId::HyprlandConfig,
        linux_state(linux, snapshot.hyprland_config.is_some()),
        (linux, false),
        if snapshot.hyprland_config.is_some() {
            "Hyprland configuration found"
        } else if linux {
            "Hyprland configuration not found"
        } else {
            "Hyprland is not used on this platform"
        },
        snapshot
            .hyprland_config
            .as_ref()
            .map(|path| path.display().to_string()),
        None,
        vec![],
    ));

    let (provider_state, provider_summary, provider_detail) = match snapshot.provider {
        ProviderProbe::Ready(detail) => (
            SetupState::Ready,
            "Transcription provider ready",
            Some(detail),
        ),
        ProviderProbe::NeedsAction(detail) => (
            SetupState::NeedsAction,
            "Transcription provider needs setup",
            Some(detail),
        ),
    };
    capabilities.push(capability(
        SetupCapabilityId::TranscriptionProvider,
        provider_state,
        (true, true),
        provider_summary,
        provider_detail,
        Some("Run `audetic provider` to configure or test the provider".to_string()),
        vec![],
    ));

    let wtype = tool("wtype", snapshot.wtype, "wtype");
    let ydotool = tool("ydotool", snapshot.ydotool, "ydotool");
    let direct_delivery = wtype.available || ydotool.available;
    let selected_backend =
        select_text_backend(snapshot.preferred_text_backend.as_deref(), &wtype, &ydotool);
    capabilities.push(capability(
        SetupCapabilityId::TextDelivery,
        linux_state(linux, direct_delivery),
        (linux, false),
        if direct_delivery {
            "Text delivery backend ready"
        } else if linux {
            "No text delivery backend found"
        } else {
            "Wayland text delivery is not used on this platform"
        },
        selected_backend.map(|backend| format!("Selected backend: {backend}")),
        linux.then(|| "Install either wtype or ydotool".to_string()),
        vec![wtype, ydotool],
    ));

    let wl_copy = tool("wl-copy", snapshot.wl_copy, "wl-clipboard");
    capabilities.push(capability(
        SetupCapabilityId::ClipboardFallback,
        linux_state(linux, wl_copy.available),
        (false, false),
        if wl_copy.available {
            "Clipboard fallback ready"
        } else if linux {
            "Clipboard fallback unavailable"
        } else {
            "Wayland clipboard fallback is not used on this platform"
        },
        None,
        linux.then(|| "Install wl-clipboard to keep a copy/paste fallback".to_string()),
        vec![wl_copy],
    ));

    let (keybind_state, keybind_summary, keybind_detail) =
        classify_keybind(linux, &snapshot.dictation_keybind, "Dictation");
    capabilities.push(capability(
        SetupCapabilityId::DictationKeybind,
        keybind_state,
        (linux, false),
        &keybind_summary,
        keybind_detail,
        linux.then(|| "Run `audetic keybind install` for the dictation shortcut".to_string()),
        vec![],
    ));

    let (keybind_state, keybind_summary, keybind_detail) =
        classify_keybind(linux, &snapshot.meeting_keybind, "Meeting");
    capabilities.push(capability(
        SetupCapabilityId::MeetingKeybind,
        keybind_state,
        (false, linux),
        &keybind_summary,
        keybind_detail,
        linux.then(|| {
            "Run `audetic keybind install --target meeting` for the meeting shortcut".to_string()
        }),
        vec![],
    ));

    let ffmpeg = tool("ffmpeg", snapshot.ffmpeg, "ffmpeg");
    capabilities.push(capability(
        SetupCapabilityId::Ffmpeg,
        if ffmpeg.available {
            SetupState::Ready
        } else {
            SetupState::NeedsAction
        },
        (false, true),
        if ffmpeg.available {
            "FFmpeg ready"
        } else {
            "FFmpeg not found"
        },
        None,
        Some("Install FFmpeg or use Audetic's bundled FFmpeg installer".to_string()),
        vec![ffmpeg],
    ));

    let pw_cat = tool("pw-cat", snapshot.pw_cat, "pipewire-audio");
    let pactl = tool("pactl", snapshot.pactl, "libpulse");
    let meeting_audio_ready = pw_cat.available && pactl.available;
    capabilities.push(capability(
        SetupCapabilityId::MeetingAudio,
        linux_state(linux, meeting_audio_ready),
        (false, linux),
        if meeting_audio_ready {
            "Meeting audio tools ready"
        } else if linux {
            "Meeting audio tools incomplete"
        } else {
            "PipeWire meeting tools are not used on this platform"
        },
        None,
        linux.then(|| "Install the missing PipeWire/PulseAudio command-line tools".to_string()),
        vec![pw_cat, pactl],
    ));

    let dictation = workflow_state(&capabilities, |capability| {
        capability.required_for_dictation
    });
    let meetings = workflow_state(&capabilities, |capability| capability.required_for_meetings);
    let (missing_arch_packages, arch_package_command) = if snapshot.platform.arch_linux {
        let packages =
            missing_arch_packages(&capabilities, snapshot.preferred_text_backend.as_deref());
        let command = package_command(&packages);
        (packages, command)
    } else {
        (vec![], None)
    };

    SetupAssessment {
        state: dictation,
        restart_required: snapshot.restart_required,
        platform: snapshot.platform,
        workflows: WorkflowReadiness {
            dictation,
            meetings,
        },
        capabilities,
        missing_arch_packages,
        arch_package_command,
    }
}

fn capability(
    id: SetupCapabilityId,
    state: SetupState,
    required_for: (bool, bool),
    summary: &str,
    detail: Option<String>,
    action: Option<String>,
    tools: Vec<ToolReadiness>,
) -> SetupCapability {
    let (required_for_dictation, required_for_meetings) = required_for;
    SetupCapability {
        id,
        state,
        required_for_dictation,
        required_for_meetings,
        summary: summary.to_string(),
        detail,
        action,
        tools,
    }
}

fn linux_state(linux: bool, ready: bool) -> SetupState {
    if !linux {
        SetupState::NotApplicable
    } else if ready {
        SetupState::Ready
    } else {
        SetupState::NeedsAction
    }
}

fn workflow_state(
    capabilities: &[SetupCapability],
    required: impl Fn(&SetupCapability) -> bool,
) -> SetupState {
    if capabilities
        .iter()
        .filter(|capability| required(capability))
        .all(|capability| capability.state.is_ready())
    {
        SetupState::Ready
    } else {
        SetupState::NeedsAction
    }
}

fn tool(id: &str, path: Option<PathBuf>, arch_package: &str) -> ToolReadiness {
    ToolReadiness {
        id: id.to_string(),
        available: path.is_some(),
        path: path.map(|path| path.display().to_string()),
        arch_package: Some(arch_package.to_string()),
    }
}

fn select_text_backend<'a>(
    preferred: Option<&str>,
    wtype: &'a ToolReadiness,
    ydotool: &'a ToolReadiness,
) -> Option<&'a str> {
    match preferred {
        Some("wtype") if wtype.available => Some("wtype"),
        Some("ydotool") if ydotool.available => Some("ydotool"),
        _ if ydotool.available => Some("ydotool"),
        _ if wtype.available => Some("wtype"),
        _ => None,
    }
}

fn missing_arch_packages(
    capabilities: &[SetupCapability],
    preferred_text_backend: Option<&str>,
) -> Vec<String> {
    let mut packages = BTreeSet::new();

    for capability in capabilities {
        if capability.state.is_ready() {
            continue;
        }

        if capability.id == SetupCapabilityId::TextDelivery {
            let preferred = if preferred_text_backend == Some("ydotool") {
                "ydotool"
            } else {
                "wtype"
            };
            packages.insert(preferred.to_string());
            continue;
        }

        for tool in &capability.tools {
            if !tool.available {
                if let Some(package) = &tool.arch_package {
                    packages.insert(package.clone());
                }
            }
        }
    }

    packages.into_iter().collect()
}

fn package_command(packages: &[String]) -> Option<String> {
    (!packages.is_empty()).then(|| format!("pacman -S --needed {}", packages.join(" ")))
}

fn load_config_read_only() -> Result<Config> {
    let path = audetic_core::global::config_file()?;
    if !path.exists() {
        return Ok(Config::default());
    }
    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("Failed to read config file: {}", path.display()))?;
    toml::from_str(&content).context("Failed to parse config file")
}

fn find_tool(name: &str) -> Option<PathBuf> {
    which::which(name).ok()
}

fn detect_omarchy() -> Option<PathBuf> {
    std::env::var_os("OMARCHY_PATH")
        .map(PathBuf::from)
        .filter(|path| path.exists())
        .or_else(|| which::which("omarchy").ok())
        .or_else(|| {
            dirs::home_dir()
                .map(|home| home.join(".local/share/omarchy"))
                .filter(|path| path.exists())
        })
        .or_else(|| {
            dirs::config_dir()
                .map(|config| config.join("omarchy"))
                .filter(|path| path.exists())
        })
}

fn detect_hyprland_session() -> bool {
    std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_some()
        || ["XDG_CURRENT_DESKTOP", "XDG_SESSION_DESKTOP"]
            .iter()
            .filter_map(|name| std::env::var(name).ok())
            .any(|desktop| desktop.to_ascii_lowercase().contains("hyprland"))
}

fn detect_platform() -> PlatformInfo {
    let release = if cfg!(target_os = "linux") {
        read_os_release(Path::new("/etc/os-release"))
    } else {
        None
    };
    let distribution = release
        .as_ref()
        .and_then(|values| values.get("PRETTY_NAME").or_else(|| values.get("NAME")))
        .cloned();
    let arch_linux = release.as_ref().is_some_and(|values| {
        values.get("ID").is_some_and(|id| id == "arch")
            || values
                .get("ID_LIKE")
                .is_some_and(|ids| ids.split_whitespace().any(|id| id == "arch"))
    });

    PlatformInfo {
        os: std::env::consts::OS.to_string(),
        architecture: std::env::consts::ARCH.to_string(),
        distribution,
        arch_linux,
    }
}

fn read_os_release(path: &Path) -> Option<std::collections::HashMap<String, String>> {
    let content = std::fs::read_to_string(path).ok()?;
    Some(
        content
            .lines()
            .filter_map(|line| line.split_once('='))
            .map(|(key, value)| {
                (
                    key.to_string(),
                    value.trim_matches(|ch| ch == '\'' || ch == '"').to_string(),
                )
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot() -> ProbeSnapshot {
        ProbeSnapshot {
            platform: PlatformInfo {
                os: "linux".to_string(),
                architecture: "x86_64".to_string(),
                distribution: Some("Arch Linux".to_string()),
                arch_linux: true,
            },
            omarchy_path: Some(PathBuf::from("/home/test/.local/share/omarchy")),
            hyprland_session: true,
            hyprland_config: Some(PathBuf::from("/home/test/.config/hypr/bindings.conf")),
            provider: ProviderProbe::Ready("audetic-api / base / en".to_string()),
            restart_required: false,
            preferred_text_backend: Some("wtype".to_string()),
            wtype: Some(PathBuf::from("/usr/bin/wtype")),
            ydotool: None,
            wl_copy: Some(PathBuf::from("/usr/bin/wl-copy")),
            dictation_keybind: KeybindProbe::Installed("SUPER + R".to_string()),
            meeting_keybind: KeybindProbe::Installed("SUPER SHIFT + R".to_string()),
            ffmpeg: Some(PathBuf::from("/usr/bin/ffmpeg")),
            pw_cat: Some(PathBuf::from("/usr/bin/pw-cat")),
            pactl: Some(PathBuf::from("/usr/bin/pactl")),
        }
    }

    #[test]
    fn classifies_ready_dictation_and_meetings() {
        let assessment = classify(snapshot());

        assert_eq!(assessment.state, SetupState::Ready);
        assert_eq!(assessment.workflows.dictation, SetupState::Ready);
        assert_eq!(assessment.workflows.meetings, SetupState::Ready);
        assert_eq!(assessment.capabilities.len(), 10);
        assert_eq!(assessment.capabilities[0].id, SetupCapabilityId::Omarchy);
        assert!(assessment.arch_package_command.is_none());
    }

    #[test]
    fn optional_meeting_tools_do_not_block_dictation() {
        let mut input = snapshot();
        input.pw_cat = None;
        input.pactl = None;
        input.ffmpeg = None;

        let assessment = classify(input);

        assert_eq!(assessment.state, SetupState::Ready);
        assert_eq!(assessment.workflows.dictation, SetupState::Ready);
        assert_eq!(assessment.workflows.meetings, SetupState::NeedsAction);
    }

    #[test]
    fn restart_required_is_preserved_in_the_setup_assessment() {
        let mut input = snapshot();
        input.restart_required = true;

        assert!(classify(input).restart_required);
    }

    #[test]
    fn missing_meeting_keybind_only_blocks_meetings() {
        let mut input = snapshot();
        input.meeting_keybind =
            KeybindProbe::NotInstalled(Some(PathBuf::from("/tmp/bindings.conf")));

        let assessment = classify(input);

        assert_eq!(assessment.workflows.dictation, SetupState::Ready);
        assert_eq!(assessment.workflows.meetings, SetupState::NeedsAction);
        assert_eq!(
            assessment
                .capability(SetupCapabilityId::MeetingKeybind)
                .unwrap()
                .summary,
            "Meeting keybind not installed"
        );
    }

    #[test]
    fn package_command_is_minimal_deduplicated_and_never_uses_sudo() {
        let mut input = snapshot();
        input.wtype = None;
        input.ydotool = None;
        input.wl_copy = None;
        input.ffmpeg = None;
        input.pw_cat = None;
        input.pactl = None;

        let assessment = classify(input);

        assert_eq!(
            assessment.missing_arch_packages,
            vec![
                "ffmpeg",
                "libpulse",
                "pipewire-audio",
                "wl-clipboard",
                "wtype"
            ]
        );
        assert_eq!(
            assessment.arch_package_command.as_deref(),
            Some("pacman -S --needed ffmpeg libpulse pipewire-audio wl-clipboard wtype")
        );
        assert!(!assessment.arch_package_command.unwrap().contains("sudo"));
    }

    #[test]
    fn preferred_ydotool_is_the_only_direct_delivery_package() {
        let mut input = snapshot();
        input.preferred_text_backend = Some("ydotool".to_string());
        input.wtype = None;
        input.ydotool = None;

        let assessment = classify(input);

        assert!(assessment
            .missing_arch_packages
            .contains(&"ydotool".to_string()));
        assert!(!assessment
            .missing_arch_packages
            .contains(&"wtype".to_string()));
    }
}
