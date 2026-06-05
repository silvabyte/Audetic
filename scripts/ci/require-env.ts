/// <reference types="bun-types" />

// Early env-var validation for CI scripts. Fails fast with a single clear
// message listing every missing var, so debugging a broken workflow doesn't
// require trial-and-error.

export function requireEnv(names: string[]): Record<string, string> {
	const missing: string[] = [];
	const out: Record<string, string> = {};

	for (const name of names) {
		const value = process.env[name];
		if (!value || value.length === 0) {
			missing.push(name);
		} else {
			out[name] = value;
		}
	}

	if (missing.length > 0) {
		console.error(`[ERROR] Missing required env vars: ${missing.join(", ")}`);
		process.exit(1);
	}

	return out;
}
