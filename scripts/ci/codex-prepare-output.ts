#!/usr/bin/env bun

/// <reference types="bun-types" />

// Prepare the Codex review body for the sticky PR comment step.
//
// Usage:
//   bun scripts/ci/codex-prepare-output.ts
//
// Required env:
//   RUNNER_TEMP, GITHUB_OUTPUT
//
// Optional env:
//   SKIP ('true' | 'false') — when 'true', the workflow short-circuited (fork PR)
//   PR_NUMBER — included in the fallback message; may be empty if PR resolution failed
//
// Writes a heredoc-formatted `review_body=<...>` block to $GITHUB_OUTPUT.
// Truncates bodies that would exceed GitHub's comment byte limit.
//
// Invoked from .github/workflows/codex-review.yml step "Prepare review output"
// (runs with `if: always()`).

import { appendFileSync, existsSync, readFileSync, statSync } from "node:fs";
import { join } from "node:path";
import { requireEnv } from "./require-env";

const MAX_COMMENT_BYTES = 60_000;
const TRUNCATION_NOTICE =
	"\n\n[Codex review truncated to fit GitHub comment limits. See the workflow logs for full output.]";

function buildBody(
	runnerTemp: string,
	skip: string | undefined,
	prNumber: string | undefined,
) {
	if (skip === "true") {
		return "Codex PR review skipped: this workflow only runs for same-repository PR branches opened by the maintainer.";
	}
	const reviewFile = join(runnerTemp, "codex-review.md");
	if (existsSync(reviewFile) && statSync(reviewFile).size > 0) {
		return readFileSync(reviewFile, "utf8");
	}
	return `Codex PR review did not produce output. Check the workflow logs for PR #${prNumber ?? "?"}.`;
}

function truncate(body: string): string {
	if (Buffer.byteLength(body, "utf8") <= MAX_COMMENT_BYTES) return body;
	const head = Buffer.from(body, "utf8")
		.subarray(0, MAX_COMMENT_BYTES)
		.toString("utf8");
	return head + TRUNCATION_NOTICE;
}

function main() {
	const env = requireEnv(["RUNNER_TEMP", "GITHUB_OUTPUT"]);
	const skip = process.env.SKIP;
	const prNumber = process.env.PR_NUMBER;

	const body = truncate(buildBody(env.RUNNER_TEMP, skip, prNumber));

	const delimiter = `codex_review_${Math.floor(Date.now() / 1000)}_${Math.floor(
		Math.random() * 0x7fffffff,
	)}`;
	appendFileSync(
		env.GITHUB_OUTPUT,
		`review_body<<${delimiter}\n${body}\n${delimiter}\n`,
	);
}

main();
