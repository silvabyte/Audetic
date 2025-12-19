import { Config } from "../config";
import { Log } from "../observability/logger";
import { type Worker, WorkerState, Workers } from "../workers";

const log = Log.child({ module: "Manager" });

export namespace Manager {
	export const workers = new Map<string, Worker>();
	let rrIndex = 0;
	let healthCheckInterval: Timer | null = null;
	let auditCheckInterval: Timer | null = null;

	const HEALTH_INTERVAL_MS = 5000;
	const HEALTH_TIMEOUT_MS = 2000;
	const HEALTH_MAX_FAILURES = 3;
	const AUDIT_INTERVAL_MS = 30000;
	const STARTUP_TIMEOUT_MS = 30000;

	// Track failed spawn attempts per port for exponential backoff
	const spawnFailures = new Map<
		number,
		{ count: number; lastAttempt: number }
	>();
	const MAX_SPAWN_FAILURES = 5;
	const BASE_BACKOFF_MS = 5000;

	export async function init() {
		const config = Config.config;

		if (config.whisperServer.cmd === "") {
			throw new Error(
				"WHISPER_SERVER_CMD not configured. Set whisperServer.cmd in config.",
			);
		}

		log.info(
			{
				poolSize: config.workers.poolSize,
				rotateThreshold: config.workers.rotateThreshold,
				startingPort: config.workers.startingPort,
			},
			"Initializing worker pool",
		);

		// Spawn initial worker pool
		const poolSize = config.workers.poolSize;
		const startPort = config.workers.startingPort;

		for (let i = 0; i < poolSize; i++) {
			const port = startPort + i;
			await spawnWorker(port);
		}

		// Start health monitoring
		healthCheckInterval = setInterval(healthSweep, HEALTH_INTERVAL_MS);
		auditCheckInterval = setInterval(auditSweep, AUDIT_INTERVAL_MS);

		log.info({ workerCount: workers.size }, "Worker pool initialized");
	}

	async function spawnWorker(port: number): Promise<void> {
		// Check if we should back off spawning this port
		const failureInfo = spawnFailures.get(port);
		if (failureInfo && failureInfo.count >= MAX_SPAWN_FAILURES) {
			const backoffMs =
				BASE_BACKOFF_MS * 2 ** (failureInfo.count - MAX_SPAWN_FAILURES);
			const timeSinceLastAttempt = Date.now() - failureInfo.lastAttempt;

			if (timeSinceLastAttempt < backoffMs) {
				log.warn(
					{
						backoffMs,
						failureCount: failureInfo.count,
						port,
						remainingMs: backoffMs - timeSinceLastAttempt,
					},
					"Skipping worker spawn due to backoff",
				);
				return;
			}
		}

		const config = Config.config;
		try {
			log.info(
				{ failureCount: failureInfo?.count || 0, port },
				"Attempting to spawn worker",
			);

			// Update last attempt time
			spawnFailures.set(port, {
				count: failureInfo?.count || 0,
				lastAttempt: Date.now(),
			});

			const cwd = config.whisperServer.cwd || process.cwd();
			const worker = Workers.spawn(
				port,
				config.whisperServer.cmd,
				cwd,
				"", // model - from config if needed
				2, // threads - from config if needed
				"", // extraArgs - from config if needed
			);

			workers.set(worker.id, worker);
			log.info(
				{ port, totalWorkers: workers.size, workerId: worker.id },
				"Worker spawned, waiting for health check",
			);

			// Wait for worker to become healthy
			const healthy = await waitForHealthy(worker, STARTUP_TIMEOUT_MS);
			if (!healthy) {
				// Increment failure count
				const currentFailure = spawnFailures.get(port);
				spawnFailures.set(port, {
					count: (currentFailure?.count || 0) + 1,
					lastAttempt: Date.now(),
				});

				log.error(
					{
						failureCount: spawnFailures.get(port)?.count,
						port,
						timeoutMs: STARTUP_TIMEOUT_MS,
						workerId: worker.id,
					},
					"Worker failed to become healthy within timeout",
				);

				// Clean up failed worker
				workers.delete(worker.id);
				try {
					Workers.terminate(worker, false);
				} catch (terminateError) {
					log.error(
						{ error: terminateError, workerId: worker.id },
						"Error terminating failed worker",
					);
				}
			} else {
				// Reset failure count on success
				spawnFailures.delete(port);

				log.info(
					{
						healthyWorkers: Array.from(workers.values()).filter(
							(w) => w.state === WorkerState.Healthy,
						).length,
						port,
						workerId: worker.id,
					},
					"Worker successfully started and healthy",
				);
			}
		} catch (error) {
			// Increment failure count
			const currentFailure = spawnFailures.get(port);
			spawnFailures.set(port, {
				count: (currentFailure?.count || 0) + 1,
				lastAttempt: Date.now(),
			});

			log.error(
				{
					error,
					errorMessage:
						error instanceof Error ? error.message : "Unknown error",
					errorStack: error instanceof Error ? error.stack : undefined,
					failureCount: spawnFailures.get(port)?.count,
					port,
				},
				"Failed to spawn worker",
			);
		}
	}

	async function waitForHealthy(
		worker: Worker,
		timeoutMs: number,
	): Promise<boolean> {
		const deadline = Date.now() + timeoutMs;

		while (Date.now() < deadline) {
			const healthy = await checkWorkerHealth(worker);
			if (healthy) {
				worker.state = WorkerState.Healthy;
				worker.consecutiveFailures = 0;
				worker.lastHealthyAt = Date.now();
				log.info({ workerId: worker.id }, "Worker is healthy");
				return true;
			}
			await new Promise((resolve) => setTimeout(resolve, 200));
		}

		return false;
	}

	async function checkWorkerHealth(worker: Worker): Promise<boolean> {
		const url = `${worker.baseUrl}/health`;
		try {
			const controller = new AbortController();
			const timeout = setTimeout(() => controller.abort(), HEALTH_TIMEOUT_MS);

			const response = await fetch(url, {
				signal: controller.signal,
			});

			clearTimeout(timeout);

			if (response.ok) {
				return true;
			}

			log.error(
				{
					port: worker.port,
					status: response.status,
					url,
					workerId: worker.id,
				},
				"Worker health check returned non-OK status",
			);
			return false;
		} catch (error) {
			const isTimeout = error instanceof Error && error.name === "AbortError";
			log.error(
				{
					error,
					errorMessage:
						error instanceof Error ? error.message : "Unknown error",
					isTimeout,
					port: worker.port,
					url,
					workerId: worker.id,
				},
				"Worker health check failed",
			);
			return false;
		}
	}

	function healthSweep(): void {
		for (const worker of workers.values()) {
			checkWorkerHealth(worker)
				.then((healthy) => {
					if (healthy) {
						worker.consecutiveFailures = 0;
						worker.lastHealthyAt = Date.now();
						if (worker.state !== WorkerState.Healthy) {
							worker.state = WorkerState.Healthy;
						}
					} else {
						worker.consecutiveFailures++;
						log.error(
							{ failures: worker.consecutiveFailures, workerId: worker.id },
							"Worker health check failed",
						);

						if (worker.consecutiveFailures >= HEALTH_MAX_FAILURES) {
							log.error(
								{ workerId: worker.id },
								"Worker exceeded max failures, replacing",
							);
							replaceWorker(worker);
						}
					}
				})
				.catch((error) => {
					log.error({ error, workerId: worker.id }, "Health check error");
				});
		}
	}

	function auditSweep(): void {
		const config = Config.config;

		// Check for dead workers
		for (const worker of workers.values()) {
			const alive = Workers.isAlive(worker);
			if (!alive) {
				log.warn(
					{ workerId: worker.id },
					"Worker process not alive, respawning",
				);
				workers.delete(worker.id);
				spawnWorker(worker.port);
			}
		}

		// Pool recovery: if we have no workers at all, rebuild the pool
		if (workers.size === 0) {
			log.error("Worker pool is empty, attempting full recovery");
			recoverPool();
		}

		// Check if we have fewer workers than configured
		const healthyCount = Array.from(workers.values()).filter(
			(w) => w.state === WorkerState.Healthy,
		).length;

		if (healthyCount < config.workers.poolSize / 2) {
			log.warn(
				{
					healthyCount,
					targetPoolSize: config.workers.poolSize,
					totalWorkers: workers.size,
				},
				"Low healthy worker count detected",
			);
		}
	}

	async function recoverPool(): Promise<void> {
		const config = Config.config;
		const poolSize = config.workers.poolSize;
		const startPort = config.workers.startingPort;

		log.info({ poolSize }, "Starting pool recovery");

		for (let i = 0; i < poolSize; i++) {
			const port = startPort + i;
			// Check if this port already has a worker
			const existingWorker = Array.from(workers.values()).find(
				(w) => w.port === port,
			);
			if (!existingWorker) {
				await spawnWorker(port);
			}
		}

		log.info({ workerCount: workers.size }, "Pool recovery completed");
	}

	function replaceWorker(worker: Worker): void {
		const port = worker.port;
		const oldWorkerId = worker.id;

		log.info({ port, workerId: oldWorkerId }, "Replacing worker");
		worker.state = WorkerState.Unhealthy;
		worker.acceptingRequests = false;

		// Remove old worker from map immediately to prevent ID conflicts
		workers.delete(oldWorkerId);

		// Spawn replacement
		spawnWorker(port).then(() => {
			// After spawn attempt, terminate the old worker process
			log.info({ oldWorkerId, port }, "Terminating replaced worker");
			Workers.terminate(worker, true);
		});
	}

	export function selectWorker(): Worker | null {
		const healthyWorkers = Array.from(workers.values()).filter(
			(w) => w.state === WorkerState.Healthy && w.acceptingRequests,
		);

		if (healthyWorkers.length === 0) {
			const allWorkers = Array.from(workers.values());
			log.error(
				{
					totalWorkers: workers.size,
					workerStates: allWorkers.map((w) => ({
						acceptingRequests: w.acceptingRequests,
						consecutiveFailures: w.consecutiveFailures,
						id: w.id,
						port: w.port,
						requestCount: w.requestCount,
						state: w.state,
					})),
				},
				"No healthy workers available",
			);
			return null;
		}

		const idx = rrIndex % healthyWorkers.length;
		rrIndex = (rrIndex + 1) % healthyWorkers.length;
		return healthyWorkers[idx] ?? null;
	}

	export async function transcribe(
		audioBase64: string,
		language?: string,
		timestamps = true,
		metadata: Record<string, string> = {},
	): Promise<TranscriptionResult> {
		const worker = selectWorker();
		if (!worker) {
			throw new Error("No healthy workers available");
		}

		worker.requestCount++;
		log.info(
			{
				language,
				requestCount: worker.requestCount,
				timestamps,
				workerId: worker.id,
			},
			"Sending transcription request to worker",
		);

		try {
			const result = await proxyToWorker(
				worker,
				audioBase64,
				language,
				timestamps,
				metadata,
			);

			// Check if worker needs recycling
			if (worker.requestCount >= Config.config.workers.rotateThreshold) {
				log.info(
					{ requestCount: worker.requestCount, workerId: worker.id },
					"Worker reached rotation threshold, scheduling replacement",
				);
				scheduleWorkerRotation(worker);
			}

			return result;
		} catch (error) {
			worker.consecutiveFailures++;
			log.error({ error, workerId: worker.id }, "Transcription request failed");
			throw error;
		}
	}

	async function proxyToWorker(
		worker: Worker,
		audioBase64: string,
		language?: string,
		_timestamps = true,
		metadata: Record<string, string> = {},
	): Promise<TranscriptionResult> {
		const url = `${worker.baseUrl}/inference`;

		// Clean base64 string
		const cleaned = audioBase64
			.replace(/\s/g, "")
			.replace(/data:[^;]*;base64,/, "");

		// Decode base64 to bytes
		const audioBytes = Buffer.from(cleaned, "base64");

		// Create multipart form data
		const formData = new FormData();
		const audioBlob = new Blob([audioBytes], { type: "audio/wav" });
		formData.append("file", audioBlob, "audio.wav");
		formData.append("response_format", "json");
		formData.append("temperature", "0.0");
		formData.append("language", language || "en");

		log.info(
			{ audioSize: audioBytes.length, url, workerId: worker.id },
			"Calling whisper server",
		);

		const controller = new AbortController();
		const timeout = setTimeout(() => controller.abort(), 120000); // 2 min timeout

		try {
			const response = await fetch(url, {
				body: formData,
				method: "POST",
				signal: controller.signal,
			});

			clearTimeout(timeout);

			if (!response.ok) {
				const text = await response.text();
				throw new Error(`HTTP ${response.status}: ${text}`);
			}

			// biome-ignore lint/suspicious/noExplicitAny: Whisper API response structure varies
			const json = (await response.json()) as any;

			// Parse whisper server response
			const text = json.text || json.transcript || "";
			const segments: TranscriptionSegment[] = (json.segments || []).map(
				// biome-ignore lint/suspicious/noExplicitAny: Whisper API response has flexible segment structure
				(s: any) => ({
					confidence: s.confidence || null,
					end: s.end || s.start || 0,
					speaker: s.speaker || null,
					start: s.start || 0,
					text: (s.text || "").trim(),
				}),
			);

			const duration =
				segments.length > 0 ? (segments[segments.length - 1]?.end ?? 0) : 0;

			return {
				confidence: segments.length > 0 ? 1.0 : 0.0,
				duration,
				language: language || "en",
				metadata: {
					...metadata,
					worker_id: worker.id,
					worker_url: worker.baseUrl,
				},
				provider: "whisper-server",
				segments,
				text: text.trim(),
			};
		} finally {
			clearTimeout(timeout);
		}
	}

	function scheduleWorkerRotation(worker: Worker): void {
		worker.acceptingRequests = false;
		setTimeout(() => {
			log.info({ workerId: worker.id }, "Rotating worker");
			replaceWorker(worker);
		}, 5000); // Wait a bit before rotating
	}

	export async function dispose() {
		log.info("Shutting down manager");

		// Stop health checks
		if (healthCheckInterval) {
			clearInterval(healthCheckInterval);
		}
		if (auditCheckInterval) {
			clearInterval(auditCheckInterval);
		}

		// Terminate all workers
		for (const worker of workers.values()) {
			Workers.terminate(worker, true);
		}

		workers.clear();
		log.info("Manager shutdown complete");
	}

	export function getPoolStatus() {
		const status = Array.from(workers.values()).map((w) => ({
			acceptingRequests: w.acceptingRequests,
			consecutiveFailures: w.consecutiveFailures,
			id: w.id,
			port: w.port,
			requestCount: w.requestCount,
			state: w.state,
			uptime: Date.now() - w.startedAt,
		}));

		return {
			healthyWorkers: status.filter((w) => w.state === WorkerState.Healthy)
				.length,
			totalWorkers: workers.size,
			workers: status,
		};
	}
}

// Types for transcription results (collocated with manager)
export interface TranscriptionSegment {
	text: string;
	start: number;
	end: number;
	confidence: number | null;
	speaker: string | null;
}

export interface TranscriptionResult {
	text: string;
	language: string;
	duration: number;
	segments: TranscriptionSegment[];
	confidence: number;
	provider: string;
	metadata: Record<string, string>;
}
