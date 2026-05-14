//! OpenAPI specification aggregator.
//!
//! `ApiDoc::openapi()` produces the full OpenAPI 3.x document for the daemon's
//! HTTP API. Served at `/openapi.json`. The UI's TypeScript types are generated
//! from this spec.

use utoipa::OpenApi;

use super::routes::{
    history, keybind, logs, meetings, post_processing, provider, recording, system, update,
};

#[derive(OpenApi)]
#[openapi(
    info(
        title = "Audetic daemon API",
        description = "HTTP control surface for the Audetic voice-to-text daemon. The UI and CLI both consume this spec.",
        version = env!("CARGO_PKG_VERSION"),
        license(name = "MIT"),
    ),
    servers(
        (url = "http://127.0.0.1:3737/api", description = "Local daemon"),
    ),
    paths(
        // Service
        super::status,
        super::version,
        // Recording (dictation)
        recording::toggle_recording,
        recording::recording_status,
        // History
        history::list_history,
        history::get_history_by_id,
        // Keybind
        keybind::get_status,
        keybind::install_keybind,
        keybind::uninstall_keybind,
        // Logs
        logs::get_logs,
        // Provider
        provider::get_config,
        provider::get_status,
        // System
        system::get_deps,
        system::start_install_ffmpeg,
        system::get_install_ffmpeg_status,
        // Update
        update::check_update,
        update::install_update,
        update::get_auto_update,
        update::set_auto_update,
        // Meetings
        meetings::start_meeting,
        meetings::stop_meeting,
        meetings::cancel_meeting,
        meetings::toggle_meeting,
        meetings::meeting_status,
        meetings::list_meetings,
        meetings::get_meeting,
        meetings::retry_meeting,
        // Post-processing jobs
        post_processing::list_events,
        post_processing::list_jobs,
        post_processing::create_job,
        post_processing::get_job,
        post_processing::update_job,
        post_processing::delete_job,
        post_processing::test_job,
    ),
    components(schemas(
        // Service
        super::ServiceInfo,
        super::VersionInfo,
        // Recording
        recording::ToggleRequest,
        recording::ToggleResponse,
        recording::CompletedJobSummary,
        recording::RecordingStatusResponse,
        // History
        crate::history::HistoryEntry,
        // Keybind
        crate::keybind::KeybindStatus,
        keybind::InstallRequest,
        keybind::InstallResponse,
        keybind::UninstallResponse,
        // Logs
        crate::logs::LogsResult,
        // Provider
        crate::transcription::ProviderInfo,
        crate::transcription::ProviderStatus,
        // System
        system::SystemDeps,
        system::InstallPhase,
        system::InstallStatusResponse,
        // Update
        crate::update::UpdateReport,
        update::UpdateInstallRequest,
        update::AutoUpdateRequest,
        update::AutoUpdateResponse,
        update::AutoUpdateState,
        // Meetings
        meetings::MeetingStartRequest,
        meetings::MeetingStartResponse,
        meetings::MeetingStopResponse,
        meetings::MeetingToggleResponse,
        meetings::MeetingStatusResponse,
        meetings::MeetingSummary,
        meetings::MeetingsListResponse,
        meetings::MeetingDetailResponse,
        // Post-processing
        crate::post_processing::Action,
        crate::post_processing::Job,
        crate::post_processing::NewJob,
        crate::post_processing::UpdateJob,
        crate::post_processing::EventKind,
        post_processing::EventDescriptor,
        post_processing::EventsListResponse,
        post_processing::JobsListResponse,
        post_processing::DeleteResponse,
        post_processing::TestJobResponse,
    )),
    tags(
        (name = "service", description = "Service identity and liveness"),
        (name = "recording", description = "Dictation (voice-to-text) control"),
        (name = "meetings", description = "Long-form meeting recording"),
        (name = "history", description = "Past transcriptions"),
        (name = "keybind", description = "Hyprland keybinding management"),
        (name = "provider", description = "Transcription provider configuration"),
        (name = "system", description = "External tool / dependency availability"),
        (name = "update", description = "Daemon self-update"),
        (name = "logs", description = "Application and transcription logs"),
        (name = "post_processing", description = "User-defined commands fired on daemon events"),
    ),
)]
pub struct ApiDoc;
