import os from "node:os";

export namespace System {
	export const Info = {
		arch: os.arch(),
		cpus: os.cpus(),
		freemem: os.freemem(),
		homedir: os.homedir(),
		hostname: os.hostname(),
		loadavg: os.loadavg(),
		platform: os.platform(),
		release: os.release(),
		tmpdir: os.tmpdir(),
		totalmem: os.totalmem(),
		type: os.type(),
		uptime: os.uptime(),
		userInfo: os.userInfo(),
		version: os.version(),
	};
	export const Process = {
		arch: process.arch,
		argv: process.argv,
		cpuUsage: process.cpuUsage(),
		cwd: process.cwd(),
		env: process.env,
		execArgv: process.execArgv,
		execPath: process.execPath,
		hrtime: process.hrtime(),
		memoryUsage: process.memoryUsage(),
		pid: process.pid,
		platform: process.platform,
		ppid: process.ppid,
		release: process.release,
		uptime: process.uptime(),
		version: process.version,
		versions: process.versions,
	};
}
