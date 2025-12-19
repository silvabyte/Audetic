import { join } from "node:path";
import { type Static, t } from "elysia";
import JSON5 from "json5";
import merge from "lodash.merge";
import { Global } from "./global";
import { Log } from "./observability/logger";

export const WorkerConfig = t.Object({
	poolSize: t.Integer({ default: 2 }),
	rotateThreshold: t.Integer({ default: 25 }),
	startingPort: t.Integer({ default: 39000 }),
});

export const WhisperServer = t.Object({
	cmd: t.String({ default: Bun.env.WHISPER_SERVER_CMD ?? "" }),
	cwd: t.String({ default: Bun.env.WHISPER_SERVER_CWD ?? "" }),
});

export const InferenceServerConfig = t.Object({
	editor: t.String(),
	whisperServer: WhisperServer,
	workers: WorkerConfig,
});

export type InferenceServerConfig = Static<typeof InferenceServerConfig>;

export const defaultConfig: InferenceServerConfig = {
	editor: "nvim",
	whisperServer: {
		cmd: Bun.env.WHISPER_SERVER_CMD ?? "",
		cwd: Bun.env.WHISPER_SERVER_CWD ?? "",
	},
	workers: {
		poolSize: 3,
		rotateThreshold: 25,
		startingPort: 39000,
	},
};

export async function load(): Promise<InferenceServerConfig> {
	const path = join(Global.Path.config, "settings.json5");
	const file = Bun.file(path);
	Log.info({ file: path }, "Loading config");
	const exists = await file.exists();
	if (!exists) {
		Log.warn("Config file not found, creating default");
		await Bun.file(path).write(JSON5.stringify(defaultConfig, null, 2));
		return defaultConfig;
	}

	try {
		const contents = JSON5.parse(await file.text());
		Log.info({ contents }, "Loaded config");
		return merge({}, defaultConfig, contents);
	} catch (error) {
		Log.warn(error, "An unexpected error occurred while loading config");
		return defaultConfig;
	}
}

const _config = await load();

export namespace Config {
	export const config = _config;

	export async function init() {
		await load();
	}

	export const getConfigPath = () => {
		return join(Global.Path.config, "settings.json5");
	};

	export async function open() {
		const configPath = Config.getConfigPath();
		const editor = _config.editor ?? process.env.EDITOR ?? "xdg-open";
		Bun.spawn([editor, configPath], {
			stdio: ["ignore", "ignore", "ignore"],
		});
	}
}
