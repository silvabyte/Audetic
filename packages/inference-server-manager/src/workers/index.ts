import { Log } from "../observability/logger";

export enum WorkerState {
	Starting = "starting",
	Healthy = "healthy",
	Unhealthy = "unhealthy",
	Stopped = "stopped",
}

export interface Worker {
	id: string;
	port: number;
	baseUrl: string;
	process: ReturnType<typeof Bun.spawn>;
	state: WorkerState;
	requestCount: number;
	consecutiveFailures: number;
	acceptingRequests: boolean;
	startedAt: number;
	lastHealthyAt: number;
}

export namespace Workers {
	const log = Log.child({ module: "Workers" });
	let workerCounter = 0;

	export function buildWorkerArgs(
		port: number,
		model: string,
		threads: number,
		extraArgs: string,
	): string[] {
		const base = ["--port", port.toString()];
		const modelArgs = model ? ["--model", model] : [];
		const threadArgs = threads > 0 ? ["--threads", threads.toString()] : [];
		const extra = extraArgs ? extraArgs.split(" ").filter((a) => a) : [];
		return [...base, ...modelArgs, ...threadArgs, ...extra];
	}

	export function spawn(
		port: number,
		serverCmd: string,
		serverCwd: string,
		model: string,
		threads: number,
		extraArgs: string,
	): Worker {
		const id = `worker_${port}_${Date.now()}_${workerCounter++}`;
		const args = buildWorkerArgs(port, model, threads, extraArgs);

		log.info({ args, cmd: serverCmd, id, port }, "Spawning whisper server");

		const logStdout = (line: string) => {
			log.info({ line, workerId: id }, "whisper-worker stdout");
		};

		const logStderr = (line: string) => {
			const normalized = line.toLowerCase();
			const isError =
				normalized.includes("error") ||
				normalized.includes("fail") ||
				normalized.includes("exception") ||
				normalized.includes("panic");

			if (isError) {
				log.error({ line, workerId: id }, "whisper-worker stderr");
			} else {
				log.info({ line, workerId: id }, "whisper-worker stderr");
			}
		};

		try {
			const proc = Bun.spawn([serverCmd, ...args], {
				cwd: serverCwd,
				onExit: (_proc, exitCode, signalCode) => {
					log.info(
						{ exitCode, signalCode, workerId: id },
						"Worker process exited",
					);
				},
				stderr: "pipe",
				stdout: "pipe",
			});

			// Stream stdout/stderr
			(async () => {
				try {
					const reader = proc.stdout.getReader();
					const decoder = new TextDecoder();
					while (true) {
						const { done, value } = await reader.read();
						if (done) break;
						const text = decoder.decode(value);
						text.split("\n").forEach((line) => {
							if (line.trim()) logStdout(line);
						});
					}
				} catch (_e) {
					// Stream closed
				}
			})();

			(async () => {
				try {
					const reader = proc.stderr.getReader();
					const decoder = new TextDecoder();
					while (true) {
						const { done, value } = await reader.read();
						if (done) break;
						const text = decoder.decode(value);
						text.split("\n").forEach((line) => {
							if (line.trim()) logStderr(line);
						});
					}
				} catch (_e) {
					// Stream closed
				}
			})();

			const worker: Worker = {
				acceptingRequests: true,
				baseUrl: `http://127.0.0.1:${port}`,
				consecutiveFailures: 0,
				id,
				lastHealthyAt: 0,
				port,
				process: proc,
				requestCount: 0,
				startedAt: Date.now(),
				state: WorkerState.Starting,
			};

			return worker;
		} catch (error) {
			log.error({ error, id, port }, "Failed to spawn worker");
			throw error;
		}
	}

	export function terminate(worker: Worker, graceful: boolean): void {
		try {
			worker.acceptingRequests = false;
			if (graceful) {
				// Give a moment for in-flight requests
				setTimeout(() => {
					worker.process.kill();
				}, 2000);
			} else {
				worker.process.kill();
			}
			worker.state = WorkerState.Stopped;
			log.info({ workerId: worker.id }, "Terminated worker");
		} catch (error) {
			log.error({ error, workerId: worker.id }, "Failed to terminate worker");
		}
	}

	export function isAlive(worker: Worker): boolean {
		return worker.process.exitCode === null;
	}
}
