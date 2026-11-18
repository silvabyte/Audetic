import { Schema } from "effect";

const nullableString = Schema.NullOr(Schema.String);
const nullableInt = Schema.NullOr(Schema.Int);

export const EnrichmentStatus = Schema.Literals([
  "pending",
  "running",
  "completed",
  "error",
  "skipped",
]);

export const TitleSource = Schema.Literals(["manual", "generated"]);

const Segment = Schema.Struct({
  start: Schema.Number,
  end: Schema.Number,
  text: Schema.String,
});

const JsonObject = Schema.Record(Schema.String, Schema.Json);

export const ServiceInfo = Schema.Struct({
  service: Schema.Literal("audetic"),
  version: Schema.String,
  status: Schema.Literal("running"),
});

export const VersionInfo = Schema.Struct({
  name: Schema.Literal("audetic"),
  version: Schema.String,
  instance_id: Schema.String,
});

export const AudioNoteSummary = Schema.Struct({
  id: Schema.Int,
  title: nullableString,
  title_source: Schema.NullOr(TitleSource),
  source_filename: nullableString,
  status: Schema.String,
  duration_seconds: nullableInt,
  started_at: Schema.String,
  audio_path: Schema.String,
  transcript_path: nullableString,
  transcript_text: nullableString,
  capture_source: Schema.String,
  source_provider: nullableString,
  source_external_id: nullableString,
  source_recorded_at: nullableString,
  classification: Schema.NullOr(JsonObject),
  classification_kind: nullableString,
  classification_kind_override: nullableString,
  enrichment_status: EnrichmentStatus,
  enrichment_error: nullableString,
});

export const AudioNotesList = Schema.Struct({
  notes: Schema.Array(AudioNoteSummary),
});

export const AudioNoteDetail = Schema.Struct({
  id: Schema.Int,
  title: nullableString,
  title_source: Schema.NullOr(TitleSource),
  source_filename: nullableString,
  status: Schema.String,
  audio_path: Schema.String,
  transcript_path: nullableString,
  transcript_text: nullableString,
  transcript_segments: Schema.NullOr(Schema.Array(Segment)),
  duration_seconds: nullableInt,
  started_at: Schema.String,
  completed_at: nullableString,
  error: nullableString,
  created_at: Schema.String,
  capture_source: Schema.String,
  source_provider: nullableString,
  source_external_id: nullableString,
  source_recorded_at: nullableString,
  classification: Schema.NullOr(JsonObject),
  classification_kind: nullableString,
  classification_kind_override: nullableString,
  enrichment_status: EnrichmentStatus,
  enrichment_error: nullableString,
});

export const NotFoundMessage = Schema.Struct({
  success: Schema.Literal(false),
  message: Schema.String,
});

export const ApiErrorBody = Schema.Struct({
  error: Schema.Literal(true),
  message: Schema.String,
});

export const MissingRoute = Schema.Struct({
  error: Schema.Literal("API route not found"),
});

export const AudioNoteStatus = Schema.Struct({
  active: Schema.Boolean,
  capture_degraded: Schema.Boolean,
  note_id: nullableInt,
  phase: Schema.String,
  duration_seconds: nullableInt,
  title: nullableString,
  audio_path: nullableString,
  last_error: nullableString,
});

export const WaybarStatus = Schema.Struct({
  text: Schema.String,
  class: Schema.String,
  tooltip: Schema.String,
});

export const AudioNoteSettings = Schema.Struct({
  auto_paste: Schema.Struct({
    microphone: Schema.Boolean,
    microphone_and_system: Schema.Boolean,
  }),
  ai_processing: Schema.Struct({
    enabled: Schema.Boolean,
  }),
});

export const Classifications = Schema.Struct({
  kinds: Schema.Array(Schema.String),
});

export const RecentTitles = Schema.Struct({
  titles: Schema.Array(Schema.String),
});

export const Artifact = Schema.Struct({
  id: Schema.Int,
  note_id: Schema.Int,
  kind: Schema.String,
  title: Schema.String,
  template_id: nullableString,
  agent_profile_id: nullableInt,
  status: Schema.Literals(["pending", "running", "completed", "error"]),
  content_markdown: nullableString,
  content_json: Schema.NullOr(Schema.Json),
  error: nullableString,
  stdout: nullableString,
  stderr: nullableString,
  created_at: Schema.String,
  updated_at: Schema.String,
  completed_at: nullableString,
});

export const Artifacts = Schema.Struct({
  artifacts: Schema.Array(Artifact),
});

export const AgentProfile = Schema.Struct({
  id: Schema.Int,
  name: Schema.String,
  kind: Schema.String,
  executable: Schema.String,
  args: Schema.Array(Schema.String),
  prompt_mode: Schema.Literals(["stdin", "arg", "file_arg"]),
  default_profile: Schema.Boolean,
  enabled: Schema.Boolean,
  available: Schema.Boolean,
  created_at: Schema.String,
  updated_at: Schema.String,
});

export const AgentProfiles = Schema.Struct({
  profiles: Schema.Array(AgentProfile),
});

export const SummaryTemplate = Schema.Struct({
  id: Schema.String,
  name: Schema.String,
  description: Schema.String,
  kind: Schema.String,
  requires_timestamps: Schema.Boolean,
  sections: Schema.Array(
    Schema.Struct({
      title: Schema.String,
      instruction: Schema.String,
      format: Schema.String,
    }),
  ),
});

export const SummaryTemplates = Schema.Struct({
  templates: Schema.Array(SummaryTemplate),
});

export const IntegrationOverview = Schema.Struct({
  public_base_url: Schema.String,
  index_webhook_url: Schema.String,
  generic_audio_url: Schema.String,
  ingress_origin: Schema.String,
  upload_limit_bytes: Schema.Int,
});

export const AccessKeys = Schema.Struct({
  keys: Schema.Array(
    Schema.Struct({
      id: Schema.String,
      name: Schema.String,
      scope: Schema.Literals(["index", "generic"]),
      created_at: Schema.String,
      last_used_at: nullableString,
      revoked_at: nullableString,
    }),
  ),
});

export const ExternalImports = Schema.Struct({
  imports: Schema.Array(
    Schema.Struct({
      id: Schema.Int,
      provider: Schema.String,
      external_id: Schema.String,
      status: Schema.String,
      audio_note_id: nullableInt,
      recorded_at: nullableString,
      source_filename: nullableString,
      error: nullableString,
      attempt_count: Schema.Int,
      created_at: Schema.String,
      updated_at: Schema.String,
    }),
  ),
});

const ProviderStatus = Schema.Union([
  Schema.Struct({
    status: Schema.Literal("ready"),
    provider: Schema.String,
    model: nullableString,
    language: nullableString,
  }),
  Schema.Struct({
    status: Schema.Literal("config_error"),
    provider: Schema.String,
    error: Schema.String,
  }),
  Schema.Struct({
    status: Schema.Literal("not_configured"),
  }),
]);

export const ProviderInfo = Schema.Struct({
  provider: nullableString,
  model: nullableString,
  language: nullableString,
  api_endpoint: nullableString,
  has_api_key: Schema.Boolean,
  command_path: nullableString,
  model_path: nullableString,
});

export const ProviderRuntime = Schema.Struct({
  restart_required: Schema.Boolean,
  active: ProviderInfo,
  active_status: ProviderStatus,
  persisted: ProviderInfo,
  persisted_status: ProviderStatus,
});

export const WhisperConfig = Schema.Struct({
  model: nullableString,
  language: nullableString,
  command_path: nullableString,
  model_path: nullableString,
  api_endpoint: nullableString,
  provider: nullableString,
  api_key: nullableString,
});

const DownloadProgress = Schema.Union([
  Schema.Struct({
    state: Schema.Literal("downloading"),
    downloaded_bytes: Schema.Int,
    total_bytes: Schema.Int,
  }),
  Schema.Struct({ state: Schema.Literal("completed") }),
  Schema.Struct({
    state: Schema.Literal("error"),
    message: Schema.String,
  }),
]);

export const ModelDescriptor = Schema.Struct({
  id: Schema.String,
  label: Schema.String,
  description: Schema.String,
  engine: Schema.Literals(["parakeet", "whisper"]),
  multilingual: Schema.Boolean,
  supports_language_selection: Schema.Boolean,
  recommended: Schema.Boolean,
  size_bytes: Schema.Number,
  installed: Schema.Boolean,
  download: Schema.NullOr(DownloadProgress),
});

export const ModelsList = Schema.Struct({
  models: Schema.Array(ModelDescriptor),
});

export const SetupState = Schema.Literals([
  "ready",
  "needs_action",
  "unavailable",
  "not_applicable",
]);

export const SetupAssessment = Schema.Struct({
  state: SetupState,
  restart_required: Schema.Boolean,
  platform: Schema.Struct({
    os: Schema.String,
    architecture: Schema.String,
    distribution: nullableString,
    arch_linux: Schema.Boolean,
  }),
  workflows: Schema.Struct({
    microphone: SetupState,
    microphone_and_system: SetupState,
  }),
  capabilities: Schema.Array(
    Schema.Struct({
      id: Schema.String,
      state: SetupState,
      required_for_microphone: Schema.Boolean,
      required_for_system_audio: Schema.Boolean,
      summary: Schema.String,
      detail: nullableString,
      action: nullableString,
      tools: Schema.Array(
        Schema.Struct({
          id: Schema.String,
          available: Schema.Boolean,
          path: nullableString,
          arch_package: nullableString,
        }),
      ),
    }),
  ),
  missing_arch_packages: Schema.Array(Schema.String),
  arch_package_command: nullableString,
});

export const SystemDeps = Schema.Struct({
  ffmpeg: Schema.Boolean,
});

export const InstallStatus = Schema.Struct({
  phase: Schema.Literals([
    "idle",
    "starting",
    "downloading",
    "extracting",
    "done",
    "error",
  ]),
  downloadedBytes: nullableInt,
  totalBytes: nullableInt,
  percent: Schema.NullOr(Schema.Number),
  binaryPath: nullableString,
  message: nullableString,
});

export const PostProcessingEvents = Schema.Struct({
  events: Schema.Array(
    Schema.Struct({
      name: Schema.String,
      label: Schema.String,
      description: Schema.String,
    }),
  ),
});

export const PostProcessingJobs = Schema.Struct({
  jobs: Schema.Array(Schema.Unknown),
});

const KeybindStatus = Schema.Union([
  Schema.Struct({
    status: Schema.Literal("installed"),
    target: Schema.String,
    config_path: Schema.String,
    display_key: Schema.String,
    command: Schema.String,
    generated_line: Schema.String,
  }),
  Schema.Struct({
    status: Schema.Literal("not_installed"),
    target: Schema.String,
    config_path: nullableString,
  }),
  Schema.Struct({
    status: Schema.Literal("no_config"),
    target: Schema.String,
  }),
]);

export const KeybindStatuses = Schema.Struct({
  note: KeybindStatus,
  system_note: KeybindStatus,
});

export const LogsResult = Schema.Struct({
  app_logs: Schema.Array(Schema.String),
  transcriptions: Schema.Array(
    Schema.Struct({
      id: Schema.Int,
      text: Schema.String,
      audio_path: Schema.String,
      created_at: Schema.String,
    }),
  ),
});

export const OpenApiDocument = Schema.Struct({
  openapi: Schema.String,
  info: Schema.Struct({
    title: Schema.String,
    version: Schema.String,
  }),
  servers: Schema.Array(
    Schema.Struct({
      url: Schema.String,
      description: Schema.optionalKey(Schema.String),
    }),
  ),
  paths: Schema.Record(Schema.String, Schema.Record(Schema.String, Schema.Unknown)),
  components: Schema.Struct({
    schemas: Schema.Record(Schema.String, Schema.Unknown),
  }),
});

export function decode<S extends Schema.Codec<unknown, unknown, never, never>>(
  schema: S,
  value: unknown,
  label: string,
): S["Type"] {
  try {
    return Schema.decodeUnknownSync(schema)(value);
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    throw new Error(`${label}\n${message}`);
  }
}
