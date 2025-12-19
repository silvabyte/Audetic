import type { Elysia } from "elysia";
import { t } from "elysia";
import { Manager } from "../manager";
import { Log } from "../observability/logger";

const log = Log.child({ module: "routes" });

// TypeBox Schemas (collocated with routes)
const TranscriptionSegmentSchema = t.Object({
	confidence: t.Nullable(t.Number()),
	end: t.Number(),
	speaker: t.Nullable(t.String()),
	start: t.Number(),
	text: t.String(),
});

const TranscriptionResultSchema = t.Object({
	confidence: t.Number(),
	duration: t.Number(),
	language: t.String(),
	metadata: t.Record(t.String(), t.String()),
	provider: t.String(),
	segments: t.Array(TranscriptionSegmentSchema),
	text: t.String(),
});

const TranscriptionRequestSchema = t.Object({
	content: t.String({ description: "Base64 encoded audio", minLength: 1 }),
	language: t.Optional(t.String({ description: "Language code (e.g., 'en')" })),
	metadata: t.Optional(t.Record(t.String(), t.String())),
	timestamps: t.Optional(t.Boolean({ default: true })),
});

const TranscriptionResponseSchema = t.Object({
	message: t.Optional(t.String()),
	result: t.Optional(TranscriptionResultSchema),
	success: t.Boolean(),
});

const ErrorResponseSchema = t.Object({
	code: t.Optional(t.String()),
	error: t.String(),
	success: t.Boolean(),
});

const HealthResponseSchema = t.Object({
	pool: t.Optional(
		t.Object({
			healthyWorkers: t.Number(),
			totalWorkers: t.Number(),
		}),
	),
	service: t.String(),
	status: t.String(),
	success: t.Boolean(),
	timestamp: t.String(),
});

const ProviderCapabilitiesSchema = t.Object({
	batch: t.Boolean(),
	diarization: t.Boolean(),
	languages: t.Array(t.String()),
	maxDuration: t.Nullable(t.Number()),
	maxFileSize: t.Nullable(t.Number()),
	streaming: t.Boolean(),
	supportedFormats: t.Array(t.String()),
	wordTimestamps: t.Boolean(),
});

const ProviderInfoSchema = t.Object({
	available: t.Boolean(),
	capabilities: ProviderCapabilitiesSchema,
	costPerMinute: t.Nullable(t.Number()),
	description: t.String(),
	id: t.String(),
	name: t.String(),
	speed: t.Nullable(t.Number()),
});

const ProvidersResponseSchema = t.Object({
	providers: t.Array(ProviderInfoSchema),
	success: t.Boolean(),
});

/**
 * Route registration for Inference Server Manager
 */
export function registerRoutes(app: Elysia): void {
	app
		.get(
			"/health",
			() => {
				const poolStatus = Manager.getPoolStatus();
				return {
					pool: {
						healthyWorkers: poolStatus.healthyWorkers,
						totalWorkers: poolStatus.totalWorkers,
					},
					service: "inference-server-manager",
					status: "healthy",
					success: true,
					timestamp: new Date().toISOString(),
				};
			},
			{
				detail: {
					description:
						"Check if the inference server manager service is healthy",
					summary: "Health check endpoint",
					tags: ["System"],
				},
				response: HealthResponseSchema,
			},
		)

		.get(
			"/api/v1/providers",
			() => {
				const poolStatus = Manager.getPoolStatus();

				return {
					providers: [
						{
							available: poolStatus.healthyWorkers > 0,
							capabilities: {
								batch: false,
								diarization: false,
								languages: [
									"en",
									"es",
									"fr",
									"de",
									"it",
									"pt",
									"nl",
									"ru",
									"zh",
									"ja",
								],
								maxDuration: null,
								maxFileSize: null,
								streaming: false,
								supportedFormats: ["wav", "mp3", "m4a", "flac", "ogg", "opus"],
								wordTimestamps: true,
							},
							costPerMinute: 0.0,
							description:
								"Managed pool of Whisper server workers with load balancing",
							id: "whisper-server",
							name: "Whisper Server",
							speed: null,
						},
					],
					success: true,
				};
			},
			{
				detail: {
					description:
						"Get a list of all available transcription providers and their capabilities",
					summary: "List available transcription providers",
					tags: ["Providers"],
				},
				response: ProvidersResponseSchema,
			},
		)

		.post(
			"/api/v1/transcriptions",
			async ({ body, set }) => {
				try {
					log.info(
						{ language: body.language },
						"Received transcription request",
					);

					const result = await Manager.transcribe(
						body.content,
						body.language,
						body.timestamps ?? true,
						body.metadata ?? {},
					);

					return {
						message: "Transcription completed successfully",
						result,
						success: true,
					};
				} catch (error) {
					log.error({ error }, "Transcription request failed");
					set.status = 500;
					return {
						code: "TRANSCRIPTION_ERROR",
						error: error instanceof Error ? error.message : "Unknown error",
						success: false,
					};
				}
			},
			{
				body: TranscriptionRequestSchema,
				detail: {
					description:
						"Submit audio content for synchronous transcription processing",
					summary: "Submit a transcription job",
					tags: ["Transcription"],
				},
				response: {
					200: TranscriptionResponseSchema,
					500: ErrorResponseSchema,
				},
			},
		)

		.get(
			"/api/v1/status",
			() => {
				const poolStatus = Manager.getPoolStatus();
				return poolStatus;
			},
			{
				detail: {
					description: "Get detailed status of all workers in the pool",
					summary: "Get worker pool status",
					tags: ["System"],
				},
			},
		);
}
