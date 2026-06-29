//! OpenAPI specification aggregator.
//!
//! `ApiDoc::openapi()` produces the full OpenAPI 3.x document for the daemon's
//! HTTP API. Served at `/openapi.json`. The UI's TypeScript types are generated
//! from this spec.

use utoipa::OpenApi;

use super::routes::{
    agents, history, keybind, logs, meeting_artifacts, meetings, models, post_processing, provider,
    recording, summary_templates, system, transcribe, update,
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
        provider::get_raw_config,
        provider::set_raw_config,
        provider::reset_config,
        provider::run_test,
        // Local models + on-device transcription
        models::list_models,
        models::get_model,
        models::download_model,
        transcribe::transcribe,
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
        meetings::confirm_meeting,
        meetings::cancel_meeting,
        meetings::toggle_meeting,
        meetings::meeting_status,
        meetings::list_meetings,
        meetings::get_meeting,
        meetings::delete_meeting,
        meetings::meeting_audio,
        meetings::retry_meeting,
        meetings::import_meeting,
        // Meeting intelligence
        agents::list_agent_profiles,
        agents::test_agent_profile,
        summary_templates::list_summary_templates,
        meeting_artifacts::list_meeting_artifacts,
        meeting_artifacts::generate_artifact,
        meeting_artifacts::get_meeting_artifact,
        meeting_artifacts::delete_meeting_artifact,
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
        crate::transcription::ProviderTestResult,
        crate::config::WhisperConfig,
        provider::ProviderTestRequest,
        // Local models + on-device transcription
        crate::transcription::models::ModelDescriptor,
        crate::transcription::models::DownloadProgress,
        models::ModelsListResponse,
        transcribe::TranscribeResponse,
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
        meetings::MeetingConfirmRequest,
        meetings::MeetingStopResponse,
        meetings::MeetingToggleResponse,
        meetings::MeetingStatusResponse,
        meetings::MeetingSummary,
        meetings::MeetingsListResponse,
        meetings::MeetingDetailResponse,
        audetic_core::jobs_client::Segment,
        meetings::MeetingRetryResponse,
        meetings::MeetingDeleteResponse,
        meetings::MeetingImportResponse,
        // Meeting intelligence
        crate::db::agent_profiles::AgentProfile,
        crate::db::agent_profiles::PromptMode,
        agents::AgentProfilesResponse,
        agents::AgentProfileTestResponse,
        crate::summary_templates::SummaryTemplate,
        crate::summary_templates::SummaryTemplateSection,
        summary_templates::SummaryTemplatesResponse,
        crate::db::meeting_artifacts::ArtifactStatus,
        crate::db::meeting_artifacts::MeetingArtifact,
        crate::meeting_artifacts::GenerateArtifactRequest,
        crate::meeting_artifacts::GenerateArtifactResponse,
        meeting_artifacts::MeetingArtifactsResponse,
        meeting_artifacts::DeleteArtifactResponse,
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
        (name = "meeting_artifacts", description = "Generated meeting summaries and notes"),
        (name = "agents", description = "Local coding-agent CLI profiles"),
        (name = "summary_templates", description = "Built-in meeting artifact templates"),
        (name = "history", description = "Past transcriptions"),
        (name = "keybind", description = "Hyprland keybinding management"),
        (name = "provider", description = "Transcription provider configuration"),
        (name = "models", description = "On-device transcription model management"),
        (name = "transcribe", description = "One-shot file transcription"),
        (name = "system", description = "External tool / dependency availability"),
        (name = "update", description = "Daemon self-update"),
        (name = "logs", description = "Application and transcription logs"),
        (name = "post_processing", description = "User-defined commands fired on daemon events"),
    ),
)]
pub struct ApiDoc;

#[cfg(test)]
mod tests {
    use super::ApiDoc;
    use crate::api::url::{api_url, paths};
    use utoipa::OpenApi;

    /// utoipa requires a literal in the `servers(url = ...)` macro, so we can't
    /// reference `api::url::API_PREFIX` there directly. This test catches the
    /// case where the two drift apart. (Lives in the daemon — `audetic-core`,
    /// which owns the url module, has no access to the OpenAPI doc.)
    #[test]
    fn openapi_servers_url_matches_api_url() {
        let doc = ApiDoc::openapi();
        let server_url = doc
            .servers
            .as_ref()
            .and_then(|s| s.first())
            .map(|s| s.url.clone())
            .expect("OpenAPI doc must declare at least one server");

        // Server URL is the base (no path suffix), so we compare against `api_url("")`.
        assert_eq!(
            server_url,
            api_url(""),
            "OpenAPI servers URL drifted from api::url::api_url(\"\"). \
             Update either api/docs.rs servers() or audetic_core::url to match."
        );
    }

    /// Every `paths::*` constant that names a well-known endpoint must
    /// correspond to an operation in the OpenAPI spec. If you rename a route or
    /// drop a path const without updating the other side, this fails loudly.
    #[test]
    fn well_known_paths_exist_in_openapi_spec() {
        let doc = ApiDoc::openapi();
        let spec_paths: std::collections::HashSet<String> =
            doc.paths.paths.keys().cloned().collect();

        for known in [
            paths::VERSION,
            paths::TOGGLE,
            paths::MEETINGS_TOGGLE,
            paths::MEETINGS_IMPORT,
            paths::AGENT_PROFILES,
            paths::SUMMARY_TEMPLATES,
            paths::POST_PROCESSING_JOBS,
            paths::POST_PROCESSING_EVENTS,
            paths::PROVIDER,
            paths::PROVIDER_STATUS,
            paths::PROVIDER_CONFIG,
            paths::PROVIDER_RESET,
            paths::PROVIDER_TEST,
            paths::MODELS,
            paths::TRANSCRIBE,
        ] {
            assert!(
                spec_paths.contains(known),
                "audetic_core::url::paths references \"{known}\" but the OpenAPI \
                 spec has no such operation. Spec paths: {spec_paths:?}"
            );
        }
    }
}
