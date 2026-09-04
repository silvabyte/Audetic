//! OpenAPI specification aggregator.
//!
//! `ApiDoc::openapi()` produces the full OpenAPI 3.x document for the daemon's
//! HTTP API. Served at `/openapi.json`. The UI's TypeScript types are generated
//! from this spec.

use utoipa::OpenApi;

use super::routes::{
    agents, history, keybind, logs, meeting_artifacts, meetings, models, post_processing, provider,
    recording, setup, summary_templates, sync, system, transcribe,
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
        provider::get_runtime_status,
        provider::get_raw_config,
        provider::set_raw_config,
        provider::validate_config,
        provider::reset_config,
        provider::run_test,
        // Local models + on-device transcription
        models::list_models,
        models::get_model,
        models::download_model,
        transcribe::transcribe,
        // System
        setup::get_setup,
        system::get_deps,
        system::restart_daemon,
        system::start_install_ffmpeg,
        system::get_install_ffmpeg_status,
        // Meetings
        meetings::start_meeting,
        meetings::stop_meeting,
        meetings::confirm_meeting,
        meetings::cancel_meeting,
        meetings::toggle_meeting,
        meetings::meeting_status,
        meetings::list_meetings,
        meetings::recent_meeting_titles,
        meetings::get_meeting,
        meetings::update_meeting_title,
        meetings::regenerate_meeting_title,
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
        // Library Sync
        sync::get_status,
        sync::discover,
        sync::configure,
        sync::retry,
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
        crate::history::HistorySource,
        // Keybind
        audetic_core::keybind::KeybindTarget,
        crate::keybind::KeybindConflict,
        crate::keybind::KeybindStatus,
        crate::keybind::KeybindStatuses,
        crate::keybind::InstallResult,
        crate::keybind::UninstallResult,
        keybind::InstallRequest,
        // Logs
        crate::logs::LogsResult,
        // Provider
        crate::transcription::ProviderInfo,
        crate::transcription::ProviderStatus,
        crate::transcription::ProviderTestResult,
        crate::config::WhisperConfig,
        provider::ProviderTestRequest,
        provider::ProviderRuntimeStatus,
        // Local models + on-device transcription
        crate::transcription::models::ModelDescriptor,
        crate::transcription::models::DownloadProgress,
        models::ModelsListResponse,
        transcribe::TranscribeResponse,
        // System
        audetic_core::setup::SetupState,
        audetic_core::setup::SetupCapabilityId,
        audetic_core::setup::ToolReadiness,
        audetic_core::setup::SetupCapability,
        audetic_core::setup::PlatformInfo,
        audetic_core::setup::WorkflowReadiness,
        audetic_core::setup::SetupAssessment,
        system::SystemDeps,
        system::RestartAccepted,
        system::InstallPhase,
        system::InstallStatusResponse,
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
        meetings::MeetingTitleSource,
        meetings::RecentMeetingTitlesResponse,
        meetings::MeetingTitleUpdateRequest,
        meetings::MeetingTitleResponse,
        meetings::MeetingTitleRegenerationResponse,
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
        // Library Sync
        audetic_core::sync::SyncRole,
        audetic_core::sync::CacheLevel,
        audetic_core::sync::DeviceId,
        audetic_core::sync::HubId,
        audetic_core::sync::HubConnection,
        audetic_core::sync::HubCandidate,
        audetic_core::sync::SyncDiscoveryFailure,
        audetic_core::sync::ServeMappingState,
        audetic_core::sync::SyncNetworkAssessment,
        audetic_core::sync::SyncSetupRequest,
        audetic_core::sync::SyncSetupResult,
        audetic_core::sync::SyncStatus,
        audetic_core::sync::RecordId,
        audetic_core::sync::UploadState,
        audetic_core::sync::PayloadAvailability,
        sync::SyncRetryResponse,
        super::error::ApiErrorBody,
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
        (name = "setup", description = "Unified host setup assessment"),
        (name = "update", description = "Daemon self-update"),
        (name = "logs", description = "Application and transcription logs"),
        (name = "post_processing", description = "User-defined commands fired on daemon events"),
        (name = "sync", description = "Local Shared Library role setup and status"),
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
            paths::PROVIDER_RUNTIME,
            paths::PROVIDER_CONFIG,
            paths::PROVIDER_VALIDATE,
            paths::PROVIDER_RESET,
            paths::PROVIDER_TEST,
            paths::MODELS,
            paths::TRANSCRIBE,
            paths::SETUP,
            paths::SYNC_STATUS,
            paths::SYNC_DISCOVER,
            paths::SYNC_CONFIGURE,
            paths::SYNC_RETRY,
            paths::SYSTEM_RESTART,
            paths::KEYBIND_STATUS,
            paths::KEYBIND_INSTALL,
            paths::KEYBIND,
        ] {
            assert!(
                spec_paths.contains(known),
                "audetic_core::url::paths references \"{known}\" but the OpenAPI \
                 spec has no such operation. Spec paths: {spec_paths:?}"
            );
        }
    }

    #[test]
    fn setup_operation_and_stable_enums_are_registered() {
        let spec = serde_json::to_value(ApiDoc::openapi()).unwrap();

        assert_eq!(
            spec["paths"][paths::SETUP]["get"]["operationId"],
            "get_setup_assessment"
        );
        assert!(spec["components"]["schemas"]["SetupAssessment"].is_object());
        assert!(spec["components"]["schemas"]["SetupCapabilityId"].is_object());
        assert!(spec["components"]["schemas"]["SetupState"].is_object());
    }

    #[test]
    fn keybind_contract_registers_stable_targets_and_both_statuses() {
        let spec = serde_json::to_value(ApiDoc::openapi()).unwrap();

        assert!(spec["components"]["schemas"]["KeybindTarget"].is_object());
        assert!(
            spec["components"]["schemas"]["KeybindStatuses"]["properties"]["dictation"].is_object()
        );
        assert!(
            spec["components"]["schemas"]["KeybindStatuses"]["properties"]["meeting"].is_object()
        );
        assert_eq!(
            spec["paths"][paths::KEYBIND_INSTALL]["post"]["operationId"],
            "install_keybind"
        );
    }

    #[test]
    fn provider_validation_and_restart_operations_are_typed() {
        let spec = serde_json::to_value(ApiDoc::openapi()).unwrap();

        assert_eq!(
            spec["paths"][paths::PROVIDER_VALIDATE]["post"]["operationId"],
            "validate_provider_config"
        );
        assert!(
            spec["paths"][paths::PROVIDER_VALIDATE]["post"]["requestBody"]["content"]
                ["application/json"]["schema"]["$ref"]
                .as_str()
                .is_some_and(|reference| reference.ends_with("/WhisperConfig"))
        );
        assert_eq!(
            spec["paths"][paths::SYSTEM_RESTART]["post"]["operationId"],
            "restart_daemon"
        );
        assert!(spec["components"]["schemas"]["RestartAccepted"].is_object());
        assert!(spec["components"]["schemas"]["ProviderRuntimeStatus"].is_object());
    }

    #[test]
    fn sync_slice_one_operations_and_schemas_are_registered() {
        let spec = serde_json::to_value(ApiDoc::openapi()).unwrap();

        assert_eq!(
            spec["paths"][paths::SYNC_STATUS]["get"]["operationId"],
            "get_sync_status"
        );
        assert_eq!(
            spec["paths"][paths::SYNC_DISCOVER]["post"]["operationId"],
            "discover_home_hubs"
        );
        assert_eq!(
            spec["paths"][paths::SYNC_CONFIGURE]["post"]["operationId"],
            "configure_sync_role"
        );
        for schema in [
            "SyncStatus",
            "SyncSetupRequest",
            "SyncSetupResult",
            "SyncNetworkAssessment",
            "HubConnection",
            "ApiErrorBody",
        ] {
            assert!(
                spec["components"]["schemas"][schema].is_object(),
                "missing schema {schema}"
            );
        }
    }

    #[test]
    fn recording_status_schema_requires_capture_health() {
        let spec = serde_json::to_value(ApiDoc::openapi()).unwrap();
        let schema = &spec["components"]["schemas"]["RecordingStatusResponse"];

        assert_eq!(schema["properties"]["capture_degraded"]["type"], "boolean");
        assert!(schema["required"]
            .as_array()
            .unwrap()
            .iter()
            .any(|field| field == "capture_degraded"));
    }

    #[test]
    fn meeting_status_schema_requires_capture_health() {
        let spec = serde_json::to_value(ApiDoc::openapi()).unwrap();
        let schema = &spec["components"]["schemas"]["MeetingStatusResponse"];

        assert_eq!(schema["properties"]["capture_degraded"]["type"], "boolean");
        assert!(schema["required"]
            .as_array()
            .unwrap()
            .iter()
            .any(|field| field == "capture_degraded"));
    }

    #[test]
    fn meeting_title_operations_and_presentation_fields_are_public() {
        let spec = serde_json::to_value(ApiDoc::openapi()).unwrap();

        assert!(spec["paths"]["/meetings/recent-titles"]["get"].is_object());
        assert!(spec["paths"]["/meetings/{id}/title"]["patch"].is_object());
        assert!(spec["paths"]["/meetings/{id}/regenerate-title"]["post"].is_object());
        for schema_name in ["MeetingSummary", "MeetingDetailResponse"] {
            let properties = &spec["components"]["schemas"][schema_name]["properties"];
            assert!(properties["title_source"].is_object());
            assert!(properties["source_filename"].is_object());
        }
    }

    #[test]
    fn slice_three_meeting_and_artifact_contracts_use_portable_uuids() {
        let spec = serde_json::to_value(ApiDoc::openapi()).unwrap();
        for path in [
            "/meetings/{id}",
            "/meetings/{id}/audio",
            "/meetings/{id}/title",
            "/meetings/{id}/regenerate-title",
            "/meetings/{id}/retry",
            "/meetings/{id}/artifacts",
            "/meetings/{id}/artifacts/{artifact_id}",
        ] {
            assert!(spec["paths"][path].is_object(), "missing path {path}");
        }
        for schema_name in ["MeetingSummary", "MeetingDetailResponse"] {
            let properties = &spec["components"]["schemas"][schema_name]["properties"];
            assert_eq!(properties["id"]["$ref"], "#/components/schemas/RecordId");
            assert_eq!(
                properties["origin_device_id"]["$ref"],
                "#/components/schemas/DeviceId"
            );
            assert!(properties["source"].is_object());
            assert!(properties["upload_state"].is_object());
            assert!(properties["payload_availability"].is_object());
            assert!(properties["audio_path"].is_null());
            assert!(properties["transcript_path"].is_null());
        }
        let artifact = &spec["components"]["schemas"]["MeetingArtifact"]["properties"];
        assert_eq!(artifact["id"]["$ref"], "#/components/schemas/RecordId");
        assert_eq!(
            artifact["meeting_id"]["$ref"],
            "#/components/schemas/RecordId"
        );
    }
}
