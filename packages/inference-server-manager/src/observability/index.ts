import { Log } from "./logger";
import { System } from "./system";

export namespace Observability {
	class Checker {
		timer: NodeJS.Timer | undefined;
		interval: number;
		constructor(interval = 60_000 * 60 * 24) {
			this.interval = interval;
		}
		start(metadata?: Record<string, unknown>) {
			this.pulse(metadata ?? {});
			this.timer = setInterval(() => {
				this.pulse(metadata ?? {});
			}, this.interval);
		}
		pulse(metadata: Record<string, unknown> | undefined) {
			// throw away env
			const { env, ...pCopy } = System.Process;
			// throw away cpus
			const { cpus, ...sCopy } = System.Info;
			Log.info(
				{
					process: pCopy,
					system: sCopy,
					...metadata,
				},
				"pulse",
			);
		}

		stop() {
			clearInterval(this.timer);
		}
	}
	const checker = new Checker();
	export function start() {
		checker.start();
	}
	export function dispose() {
		checker.stop();
	}
}
