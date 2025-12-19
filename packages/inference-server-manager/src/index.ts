import type { Elysia } from "elysia";
import { createApp } from "./app";
import { Config } from "./config";
import { Observability } from "./observability";
import { Log } from "./observability/logger";

let registered = false;
let app: Elysia | null = null;
let isCleaningUp = false;

const WithTry = async <T>(fn: () => Promise<T>, msg = "") => {
	try {
		const resultsIfAny = await fn();
		return resultsIfAny;
	} catch (error) {
		Log.error(error, msg);
	}
};

const cleanup = async () => {
	// Prevent double cleanup
	if (isCleaningUp) {
		return;
	}
	isCleaningUp = true;

	Log.info("Shutting down Inference Server Manager...");

	// Dispose manager and workers
	WithTry(async () => {
		const { Manager } = await import("./manager");
		await Manager.dispose();
	}, "Failed to dispose manager");

	// End observability session
	WithTry(
		async () => Observability.dispose(),
		"Failed to end observability session",
	);

	if (app?.server) {
		try {
			await app.stop();
		} catch (error) {
			// Ignore errors if server isn't running
			Log.debug({ error }, "Server already stopped or not running");
		}
	}

	Log.info("Inference Server Manager shutdown complete");
};

const registerShutdown = () => {
	if (registered) {
		return;
	}

	registered = true;

	const handleSignal = async (signal: NodeJS.Signals) => {
		Log.info({ signal }, "Received shutdown signal");
		await cleanup();
		const exitCode = signal === "SIGINT" ? 130 : 143;
		process.exit(exitCode);
	};

	process.once("SIGINT", handleSignal);
	process.once("SIGTERM", handleSignal);
};

namespace Main {
	export const run = async () => {
		registerShutdown();

		await Config.init();

		// Start observability session to track app usage
		Log.info("Starting observability session");
		Observability.start();

		// Initialize the inference server manager with worker pool
		const { Manager } = await import("./manager");
		await Manager.init();

		try {
			app = createApp();

			// Support both old and new env var names for backward compatibility
			const port = Number(
				Bun.env.INFERENCE_SERVER_PORT ??
					Bun.env.TRANSCRIPTION_MANAGER_PORT ??
					3141,
			);
			// Bind to 0.0.0.0 to accept connections from any host
			const hostname = Bun.env.INFERENCE_SERVER_HOST ?? "0.0.0.0";
			app.listen({ port, hostname });

			Log.info(
				`Inference Server Manager is running at ${app.server?.hostname}:${app.server?.port}`,
			);

			// Note: Cleanup is handled by signal handlers (SIGINT/SIGTERM)
		} catch (error) {
			Log.error(error, "Failed to start Inference Server Manager");
			await cleanup();
			throw error;
		}
	};
}

Main.run().catch((error) => {
	console.error("Inference Server Manager failed to start", error);
	process.exitCode = 1;
});

// Re-export App type for Eden Treaty
export type { App } from "./app";
