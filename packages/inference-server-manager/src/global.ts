import fs from "node:fs/promises";
import path from "node:path";
import { xdgCache, xdgConfig, xdgData, xdgState } from "xdg-basedir";

// App name for display/logging purposes
export const AppName = "inference-server-manager";

// XDG directory name - kept as transcription_manager for backward compatibility
// with existing voice.audetic.link deployment
const XdgDirName = "transcription_manager";

const data = path.join(xdgData as string, XdgDirName);
const cache = path.join(xdgCache as string, XdgDirName);
const config = path.join(xdgConfig as string, XdgDirName);
const state = path.join(xdgState as string, XdgDirName);

export namespace Global {
	export const Path = {
		audio: path.join(data, "audio"),
		bin: path.join(data, "bin"),
		cache,
		config,
		data,
		logs: path.join(data, "logs"),
		state,
	} as const;
}

await Promise.all([
	fs.mkdir(Global.Path.data, { recursive: true }),
	fs.mkdir(Global.Path.config, { recursive: true }),
	fs.mkdir(Global.Path.state, { recursive: true }),
	fs.mkdir(Global.Path.logs, { recursive: true }),
	fs.mkdir(Global.Path.audio, { recursive: true }),
	fs.mkdir(Global.Path.bin, { recursive: true }),
]);

const CACHE_VERSION = "9";

export const version = await Bun.file(path.join(Global.Path.cache, "version"))
	.text()
	.catch(() => "0");

if (version !== CACHE_VERSION) {
	try {
		const contents = await fs.readdir(Global.Path.cache);
		await Promise.all(
			contents.map((item) =>
				fs.rm(path.join(Global.Path.cache, item), {
					force: true,
					recursive: true,
				}),
			),
		);
	} catch (_e) {}
	await Bun.file(path.join(Global.Path.cache, "version")).write(CACHE_VERSION);
}
