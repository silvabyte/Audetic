#!/usr/bin/env bun

/// <reference types="bun-types" />

// Resolve PR context for the Codex review workflow.
//
// Usage:
//   bun scripts/ci/codex-resolve-pr.ts
//
// Required env:
//   GH_TOKEN, GITHUB_REPOSITORY, GITHUB_OUTPUT, EVENT_NAME
//
// Optional env:
//   EVENT_PR_NUMBER (set by pull_request triggers; empty on workflow_dispatch)
//   INPUT_PR_NUMBER (set by workflow_dispatch input; empty otherwise)
//
// Invoked from .github/workflows/codex-review.yml step "Resolve PR context".

import { appendFileSync } from "node:fs";
import { $ } from "bun";
import { requireEnv } from "./require-env";

function log(msg: string) {
	console.log(`[codex-resolve-pr] ${msg}`);
}

function fail(msg: string): never {
	console.error(`[codex-resolve-pr] ERROR: ${msg}`);
	process.exit(1);
}

interface PullPayload {
	head: { sha: string; repo: { full_name: string } };
	base: { ref: string; repo: { full_name: string } };
	title: string;
}

async function main() {
	const env = requireEnv([
		"GH_TOKEN",
		"GITHUB_REPOSITORY",
		"GITHUB_OUTPUT",
		"EVENT_NAME",
	]);

	const raw =
		env.EVENT_NAME === "workflow_dispatch"
			? process.env.INPUT_PR_NUMBER
			: process.env.EVENT_PR_NUMBER;

	if (!raw || !/^\d+$/.test(raw)) {
		console.log(`::error::Invalid PR number: ${raw ?? ""}`);
		process.exit(1);
	}

	const prNumber = raw;
	const repo = env.GITHUB_REPOSITORY;
	const result = await $`gh api repos/${repo}/pulls/${prNumber}`
		.env({ ...process.env, GH_TOKEN: env.GH_TOKEN })
		.nothrow()
		.quiet();

	if (result.exitCode !== 0) {
		fail(
			`gh api repos/${repo}/pulls/${prNumber} failed: ${result.stderr.toString().trim()}`,
		);
	}

	let payload: PullPayload;
	try {
		payload = JSON.parse(result.stdout.toString()) as PullPayload;
	} catch (e) {
		fail(`failed to parse PR payload: ${(e as Error).message}`);
	}

	const headRepo = payload.head.repo.full_name;
	const baseRepo = payload.base.repo.full_name;

	if (headRepo !== baseRepo) {
		log(`Skipping fork PR ${prNumber}: ${headRepo} != ${baseRepo}.`);
		appendFileSync(env.GITHUB_OUTPUT, `skip=true\n`);
		appendFileSync(env.GITHUB_OUTPUT, `number=${prNumber}\n`);
		return;
	}

	appendFileSync(env.GITHUB_OUTPUT, `skip=false\n`);
	appendFileSync(env.GITHUB_OUTPUT, `number=${prNumber}\n`);
	appendFileSync(env.GITHUB_OUTPUT, `head_sha=${payload.head.sha}\n`);
	appendFileSync(env.GITHUB_OUTPUT, `base_ref=${payload.base.ref}\n`);
	appendFileSync(env.GITHUB_OUTPUT, `title=${payload.title}\n`);
}

main();
