//! OpenAPI specification aggregator.
//!
//! `ApiDoc::openapi()` produces the full OpenAPI 3.x document for the daemon's
//! HTTP API. Served at `/openapi.json`. The UI's TypeScript types are generated
//! from this spec.

use utoipa::OpenApi;

use super::routes::{
    agents, audio_note_artifacts, audio_notes, keybind, logs, models, post_processing, provider,
    setup, summary_templates, system,
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
        // System
        setup::get_setup,
        system::get_deps,
        system::restart_daemon,
        system::start_install_ffmpeg,
        system::get_install_ffmpeg_status,
        // AudioNotes
        audio_notes::start_audio_note,
        audio_notes::get_audio_note_settings,
        audio_notes::set_audio_note_settings,
        audio_notes::stop_audio_note,
        audio_notes::confirm_audio_note,
        audio_notes::cancel_audio_note,
        audio_notes::toggle_audio_note,
        audio_notes::audio_note_status,
        audio_notes::list_audio_notes,
        audio_notes::recent_audio_note_titles,
        audio_notes::get_audio_note,
        audio_notes::update_audio_note_title,
        audio_notes::regenerate_audio_note_title,
        audio_notes::delete_audio_note,
        audio_notes::audio_note_audio,
        audio_notes::retry_audio_note,
        audio_notes::process_audio_note,
        audio_notes::import_audio_note,
        // AudioNote intelligence
        agents::list_agent_profiles,
        agents::test_agent_profile,
        agents::select_default_agent,
        summary_templates::list_summary_templates,
        audio_note_artifacts::list_audio_note_artifacts,
        audio_note_artifacts::generate_artifact,
        audio_note_artifacts::get_audio_note_artifact,
        audio_note_artifacts::delete_audio_note_artifact,
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
        // AudioNotes
        audio_notes::AudioNoteStartRequest,
        audio_notes::AudioNoteSettings,
        audio_notes::AudioNoteImportRequest,
        crate::audio_notes::AudioNoteCaptureSource,
        audio_notes::AudioNoteStartResponse,
        audio_notes::AudioNoteConfirmRequest,
        audio_notes::AudioNoteStopResponse,
        audio_notes::AudioNoteToggleResponse,
        audio_notes::AudioNoteStatusResponse,
        audio_notes::AudioNoteSummary,
        audio_notes::AudioNotesListResponse,
        audio_notes::AudioNoteDetailResponse,
        audio_notes::AudioNoteTitleSource,
        audio_notes::RecentAudioNoteTitlesResponse,
        audio_notes::AudioNoteTitleUpdateRequest,
        audio_notes::AudioNoteTitleResponse,
        audio_notes::AudioNoteTitleRegenerationResponse,
        audetic_core::jobs_client::Segment,
        audio_notes::AudioNoteRetryResponse,
        audio_notes::AudioNoteDeleteResponse,
        audio_notes::AudioNoteImportResponse,
        // AudioNote intelligence
        crate::db::agent_profiles::AgentProfile,
        crate::db::agent_profiles::PromptMode,
        agents::AgentProfilesResponse,
        agents::AgentProfileTestResponse,
        crate::summary_templates::SummaryTemplate,
        crate::summary_templates::SummaryTemplateSection,
        summary_templates::SummaryTemplatesResponse,
        crate::db::audio_note_artifacts::ArtifactStatus,
        crate::db::audio_note_artifacts::AudioNoteArtifact,
        crate::audio_note_artifacts::GenerateArtifactRequest,
        crate::audio_note_artifacts::GenerateArtifactResponse,
        audio_note_artifacts::AudioNoteArtifactsResponse,
        audio_note_artifacts::DeleteArtifactResponse,
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
        (name = "audio_notes", description = "Unified audio capture, transcription, and enrichment"),
        (name = "audio_note_artifacts", description = "Generated Audio Note artifacts and structured outputs"),
        (name = "agents", description = "Local coding-agent CLI profiles"),
        (name = "summary_templates", description = "Built-in Audio Note processor templates"),
        (name = "keybind", description = "Hyprland keybinding management"),
        (name = "provider", description = "Transcription provider configuration"),
        (name = "models", description = "On-device transcription model management"),
        (name = "system", description = "External tool / dependency availability"),
        (name = "setup", description = "Unified host setup assessment"),
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
            paths::AUDIO_NOTES_TOGGLE,
            paths::AUDIO_NOTES_IMPORT,
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
            paths::SETUP,
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
        assert!(spec["components"]["schemas"]["KeybindStatuses"]["properties"]["note"].is_object());
        assert!(
            spec["components"]["schemas"]["KeybindStatuses"]["properties"]["system_note"]
                .is_object()
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
    fn obsolete_capture_endpoints_and_schemas_are_absent() {
        let spec = serde_json::to_value(ApiDoc::openapi()).unwrap();
        for path in ["/toggle", "/status", "/history", "/meetings", "/transcribe"] {
            assert!(spec["paths"].get(path).is_none(), "obsolete path {path}");
        }
        assert!(spec["components"]["schemas"]
            .get("RecordingStatusResponse")
            .is_none());
        assert!(spec["paths"]["/audio-notes/{id}/process"]["post"].is_object());
    }

    #[test]
    fn audio_note_status_schema_requires_capture_health() {
        let spec = serde_json::to_value(ApiDoc::openapi()).unwrap();
        let schema = &spec["components"]["schemas"]["AudioNoteStatusResponse"];

        assert_eq!(schema["properties"]["capture_degraded"]["type"], "boolean");
        assert!(schema["required"]
            .as_array()
            .unwrap()
            .iter()
            .any(|field| field == "capture_degraded"));
    }

    #[test]
    fn audio_note_title_operations_and_presentation_fields_are_public() {
        let spec = serde_json::to_value(ApiDoc::openapi()).unwrap();

        assert!(spec["paths"]["/audio-notes/recent-titles"]["get"].is_object());
        assert!(spec["paths"]["/audio-notes/{id}/title"]["patch"].is_object());
        assert!(spec["paths"]["/audio-notes/{id}/regenerate-title"]["post"].is_object());
        for schema_name in ["AudioNoteSummary", "AudioNoteDetailResponse"] {
            let properties = &spec["components"]["schemas"][schema_name]["properties"];
            assert!(properties["title_source"].is_object());
            assert!(properties["source_filename"].is_object());
            for field in [
                "classification",
                "enrichment_status",
                "enrichment_error",
                "capture_source",
                "transcript_text",
            ] {
                assert!(properties[field].is_object(), "{schema_name} lacks {field}");
            }
        }
    }

    #[test]
    fn capture_settings_and_multipart_import_are_typed() {
        let spec = serde_json::to_value(ApiDoc::openapi()).unwrap();
        assert!(spec["paths"]["/audio-notes/settings"]["get"].is_object());
        assert!(spec["paths"]["/audio-notes/settings"]["put"].is_object());
        let multipart = &spec["paths"]["/audio-notes/import"]["post"]["requestBody"]["content"]
            ["multipart/form-data"];
        assert_eq!(
            multipart["schema"]["$ref"],
            "#/components/schemas/AudioNoteImportRequest"
        );
        let upload = &spec["components"]["schemas"]["AudioNoteImportRequest"];
        assert_eq!(upload["properties"]["file"]["type"], "string");
        assert_eq!(upload["properties"]["file"]["format"], "binary");
        assert!(upload["properties"]["title"].is_object());
        let required =
            spec["components"]["schemas"]["AudioNoteStartRequest"]["required"].as_array();
        assert!(required.is_none_or(|fields| !fields.iter().any(|field| field == "auto_paste")));
    }
}
