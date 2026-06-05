/// <reference types="bun-types" />

// Early env-var validation for CI scripts. Fails fast with a single clear
// message listing every missing var, so debugging a broken workflow doesn't
// require trial-and-error.

// The `const` type parameter infers `names` as a tuple of string literals, so
// the returned record has each requested var as a known `string` property
// rather than a `string | undefined` index access. That keeps call sites clean
// under the repo's `noUncheckedIndexedAccess` tsconfig setting.
export function requireEnv<const T extends readonly string[]>(
	names: T,
): Record<T[number], string> {
	const missing: string[] = [];
	const out = {} as Record<T[number], string>;

	for (const name of names) {
		const value = process.env[name];
		if (!value || value.length === 0) {
			missing.push(name);
		} else {
			out[name as T[number]] = value;
		}
	}

	if (missing.length > 0) {
		console.error(`[ERROR] Missing required env vars: ${missing.join(", ")}`);
		process.exit(1);
	}

	return out;
}
