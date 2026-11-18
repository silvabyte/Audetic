/**
 * Stable strings the daemon writes today. The suite treats them as the
 * contract a replacement has to reproduce. They are observations, not a
 * second implementation of the sweep.
 */

export const ENRICHMENT_ERROR_TEXT =
  "Codex rejected model gpt-6-astra: this ChatGPT account does not support that model";

export const INTERRUPTED_RECORDING =
  "Interrupted: the Audetic daemon stopped while this Audio Note was recording";

export const INTERRUPTED_TRANSCRIBING =
  "Interrupted: the Audetic daemon stopped while this Audio Note was transcribing";

export const INTERRUPTED_ENRICHMENT =
  "Interrupted: the Audetic daemon stopped during enrichment; retry processing";

export const INTERRUPTED_ARTIFACT =
  "Interrupted: the Audetic daemon stopped while this artifact was pending; generate the artifact again to retry";

export const INTERRUPTED_IMPORT =
  "Interrupted: the Audetic daemon stopped during import";

export const INTERRUPTED_SYNC =
  "Interrupted: the Audetic daemon stopped during synchronization";

export const NOTE_IDS = {
  recording: 1,
  transcribing: 2,
  enrichmentError: 3,
  enrichmentRunning: 4,
  completed: 5,
  deleted: 6,
  skipped: 7,
} as const;

export const MANUAL_TITLE = "MacBook enrichment failure";
export const ERROR_TRANSCRIPT = "hello from the macbook fixture";
export const MEETING_KIND = "meeting";

export const CODEX_ARGS = [
  "exec",
  "--skip-git-repo-check",
  "--sandbox",
  "read-only",
  "-",
] as const;

/**
 * OpenAPI operations this suite does not call, and why. Every other
 * operation in the served document is requested by the HTTP tests.
 * A new operation has to be added here or exercised, or the suite fails.
 */
export const UNCOVERED_OPERATIONS: Record<string, string> = {
  "POST /audio-notes/start":
    "Opens the microphone. The suite must not prompt for a device or TCC.",
  "POST /audio-notes/stop":
    "Stops a capture session. There is no capture, and stop talks to the audio pipeline.",
  "POST /audio-notes/toggle":
    "Starts or stops capture. Same device and TCC constraint as start.",
  "POST /audio-notes/confirm":
    "Confirms a capture and starts transcription, which needs a model or a network provider.",
  "POST /audio-notes/cancel":
    "Cancels an in-memory capture session. No session exists, and the handler still enters the capture owner.",
  "POST /audio-notes/import":
    "Accepts an upload and starts media inspection plus transcription.",
  "POST /audio-notes/{id}/retry":
    "Re-runs transcription for an errored note. That loads a model or calls a provider.",
  "POST /audio-notes/{id}/process":
    "Claims enrichment and shells out to an agent. The fixture must keep the error text, not re-run it.",
  "POST /audio-notes/{id}/regenerate-title":
    "Releases title ownership and starts an agent run.",
  "PUT /audio-notes/settings":
    "Writes config.toml. The read of the defaults is covered; the write is not required to lock startup behavior.",
  "PATCH /audio-notes/{id}/title":
    "Mutates a note. List and detail already round-trip the stored title.",
  "PUT /audio-notes/{id}/classification":
    "Mutates classification. The stored kind is already returned by list, detail, and the classifications read.",
  "DELETE /audio-notes/{id}/classification":
    "Clears a manual classification. The suite does not need a second writer.",
  "DELETE /audio-notes/{id}":
    "Soft-deletes a note. The fixture already contains a deleted row the list hides.",
  "POST /audio-notes/{id}/artifacts":
    "Generates an artifact by running an agent.",
  "DELETE /audio-notes/{id}/artifacts/{artifact_id}":
    "Deletes an artifact. The interrupted artifact is read, not removed.",
  "POST /agent-profiles/{id}/test":
    "Resolves and can invoke a local agent executable.",
  "POST /agent-profiles/{id}/default":
    "Writes the default agent selection. The built-in Codex argv is asserted on the list.",
  "PUT /provider/config":
    "Rewrites the provider config. The active and persisted reads are covered.",
  "POST /provider/validate":
    "Initializes a provider from a caller-supplied body. The active status read already initializes the default.",
  "POST /provider/reset":
    "Replaces config.toml with defaults and writes a backup.",
  "POST /provider/test":
    "Runs a transcription. The default provider is the network Audetic API.",
  "POST /models/{id}/download":
    "Downloads a model from Hugging Face.",
  "POST /system/restart":
    "Asks systemd or launchd to restart the installed daemon. That is the live service, not this fixture.",
  "POST /system/install-ffmpeg":
    "May download an FFmpeg build. The status read is covered and stays idle.",
  "POST /keybind/install":
    "Writes the Hyprland config. Status is read against an empty temp config.",
  "DELETE /keybind":
    "Removes a Hyprland binding from the user config.",
  "POST /integrations/keys":
    "Mints an ingress secret. The empty key list is the read contract.",
  "DELETE /integrations/keys/{id}":
    "Revokes an ingress key. No key is created.",
  "GET /integrations/plaud":
    "When the plaud CLI is on PATH this runs `plaud version` and `plaud me`, which can take a minute and use the network. The interrupted sync row is checked in SQLite instead.",
  "PUT /integrations/plaud":
    "Enables the Plaud poll. The scheduler would then shell out to the plaud CLI.",
  "POST /integrations/plaud/sync":
    "Schedules a Plaud poll against the real CLI.",
  "POST /integrations/plaud/backfill":
    "Schedules a historical Plaud download.",
  "POST /post-processing/jobs":
    "Creates a shell job. The empty list and the missing-id read are covered.",
  "PATCH /post-processing/jobs/{id}":
    "Updates a shell job. No job is stored.",
  "DELETE /post-processing/jobs/{id}":
    "Deletes a shell job. No job is stored.",
  "POST /post-processing/jobs/{id}/test":
    "Runs the job command.",
};

/**
 * Served paths that are not OpenAPI operations. The HTTP tests still call them.
 */
export const NON_OPERATION_ROUTES = [
  "GET /openapi.json",
  "GET /does-not-exist",
] as const;
