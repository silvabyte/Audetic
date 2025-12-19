import { join } from "node:path";
import pino from "pino";
import { AppName, Global } from "../global";

export namespace Log {
	const targets = [
		{
			target: "pino-pretty",
		},
		{
			options: {
				file: join(Global.Path.logs, "log"),
				frequency: "hourly",
				mkdir: true,
			},
			target: "pino-roll",
		},
	];

	export const transport = pino.transport({
		level: Bun.env.LOG_LEVEL ?? "info",
		targets,
	});

	export const instance = pino(transport).child({
		app: AppName,
	});

	export function withTraceContext(traceId?: string, spanId?: string) {
		return instance.child({
			span_id: spanId,
			trace_id: traceId,
		});
	}

	export const info = instance.info.bind(instance);
	export const debug = instance.debug.bind(instance);
	export const error = instance.error.bind(instance);
	export const warn = instance.warn.bind(instance);
	export const fatal = instance.fatal.bind(instance);
	export const trace = instance.trace.bind(instance);
	export const child = instance.child.bind(instance);
	export const level = instance.level;
}
