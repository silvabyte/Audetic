#!/usr/bin/env bun

/// <reference types="bun-types" />

// Run Codex against the resolved PR and capture review output.
//
// Usage:
//   bun scripts/ci/codex-run.ts
//
// Required env:
//   RUNNER_TEMP, PR_BASE_REF, PR_NUMBER, PR_TITLE
//
// Writes the review markdown to ${RUNNER_TEMP}/codex-review.md via
// `codex --output-last-message`. Exits non-zero only if codex itself failed
// AND no output was produced — a non-zero exit alongside non-empty output is
// treated as "review wrote something useful, downstream comment will surface
// it" (matches the previous shell `set +e` quirk).
//
// Invoked from .github/workflows/codex-review.yml step "Run Codex".

import { existsSync, statSync } from "node:fs";
import { join } from "node:path";
import { $ } from "bun";
import { requireEnv } from "./require-env";

function log(msg: string) {
	console.log(`[codex-run] ${msg}`);
}

async function main() {
	const env = requireEnv([
		"RUNNER_TEMP",
		"PR_DIR",
		"PR_BASE_REF",
		"PR_NUMBER",
		"PR_TITLE",
	]);
	const reviewFile = join(env.RUNNER_TEMP, "codex-review.md");
	const baseRef = `origin/${env.PR_BASE_REF}`;
	const title = `PR #${env.PR_NUMBER}: ${env.PR_TITLE}`;

	await $`codex --version`;

	// Run codex inside the PR-head checkout (PR_DIR) so it reviews the PR's
	// tree, while this helper itself runs from the trusted root checkout. The
	// review markdown is written to RUNNER_TEMP, outside both checkouts.
	const result =
		await $`codex exec review --base ${baseRef} --title ${title} --full-auto --ephemeral --output-last-message ${reviewFile}`
			.cwd(env.PR_DIR)
			.nothrow();

	if (result.exitCode !== 0) {
		const hasOutput = existsSync(reviewFile) && statSync(reviewFile).size > 0;
		if (!hasOutput) {
			log(
				`codex exited ${result.exitCode} with no output — propagating failure`,
			);
			process.exit(result.exitCode);
		}
		log(`codex exited ${result.exitCode} but produced output — continuing`);
	}
}

main();
