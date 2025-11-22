#!/usr/bin/env bun

/// <reference types="bun-types" />

import { access, copyFile, mkdir, mkdtemp, rm, stat } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { $ } from "bun";

const ROOT_DIR = path.resolve(import.meta.dir, "../../");
const RELEASE_DIR = path.join(ROOT_DIR, "release", "cli");
const RELEASES_ROOT = path.join(RELEASE_DIR, "releases");
const CARGO_TOML = path.join(ROOT_DIR, "Cargo.toml");
const CARGO_LOCK = path.join(ROOT_DIR, "Cargo.lock");
const SERVICE_FILE = path.join(ROOT_DIR, "audetic.service");
const EXAMPLE_CONFIG = path.join(ROOT_DIR, "example_config.toml");

$.cwd(ROOT_DIR);

function getVersionFilePath(channel: string): string {
	if (channel === "stable") {
		return path.join(RELEASE_DIR, "version");
	}
	return path.join(RELEASE_DIR, `version-${channel}`);
}

const TARGET_LOOKUP: Record<string, string> = {
	"linux-x86_64-gnu": "x86_64-unknown-linux-gnu",
	"linux-aarch64-gnu": "aarch64-unknown-linux-gnu",
	"macos-aarch64": "aarch64-apple-darwin",
	"macos-x86_64": "x86_64-apple-darwin",
};

const env = Bun.env;

const config = {
	channel: env.CHANNEL ?? "stable",
	targets: parseTargets(env.TARGETS ?? "linux-x86_64-gnu"),
	allowDirty: flag("ALLOW_DIRTY", false),
	dryRun: flag("DRY_RUN", false),
	skipTests: flag("SKIP_TESTS", false),
	skipTag: flag("SKIP_TAG", false),
	useCross: flag("USE_CROSS", false),
	extraFeatures: env.EXTRA_FEATURES,
	autoCommit: flag("AUTO_COMMIT", true),
	releaseDate: env.RELEASE_DATE ?? new Date().toISOString(),
	continueOnError: flag("CONTINUE_ON_ERROR", true),
	bumpStrategy: (env.VERSION_AUTO_BUMP ?? "patch").toLowerCase(),
};

if (!config.targets.length) {
	console.error("No TARGETS provided.");
	process.exit(1);
}

console.log("==> Audetic release");
console.log(`Targets: ${config.targets.join(", ")}`);
console.log(`Dry run: ${config.dryRun ? "yes" : "no"}`);

await ensureCommands([config.useCross ? "cross" : "cargo", "tar"]);
if (!config.allowDirty && !config.dryRun) {
	await assertCleanGit();
}

const version = await resolveVersion();
console.log(`Version: ${version}`);

await syncVersions(version);
await maybeRunTests();
await ensureNotes(version);

const tmpRoot = await mkdtemp(path.join(os.tmpdir(), "audetic-release-"));
const artifacts: Artifact[] = [];
const failures: TargetFailure[] = [];

try {
	for (const targetId of config.targets) {
		const rustTarget = TARGET_LOOKUP[targetId];
		if (!rustTarget) {
			failures.push({ targetId, error: new Error("Unknown target id") });
			continue;
		}

		try {
			await buildTarget(targetId, rustTarget);
			if (!config.dryRun) {
				artifacts.push(
					await packageTarget(version, targetId, rustTarget, tmpRoot),
				);
			}
		} catch (error) {
			const err = error instanceof Error ? error : new Error(String(error));
			failures.push({ targetId, error: err });
			console.error(`!! ${targetId} failed: ${err.message}`);
			if (!config.continueOnError) {
				throw err;
			}
		}
	}
} finally {
	await rm(tmpRoot, { recursive: true, force: true }).catch(() => {});
}

if (!config.dryRun) {
	await writeManifest(version, artifacts);
	if (failures.length === 0) {
		await publishAssets();
		await tagRelease(version);
		await format();
		await commitAndPush(version);
	} else {
		console.warn("Skipping publish/tag because some targets failed.");
	}
}

printSummary(artifacts, failures);
if (failures.length) {
	process.exit(1);
}

type Artifact = {
	targetId: string;
	archivePath: string;
	sha: string;
	size: number;
};

type TargetFailure = {
	targetId: string;
	error: Error;
};

function flag(name: string, fallback: boolean): boolean {
	const value = env[name];
	if (value === undefined) return fallback;
	const normalized = value.trim().toLowerCase();
	if (!normalized) return fallback;
	return ["1", "true", "yes", "on"].includes(normalized);
}

function parseTargets(raw: string): string[] {
	return raw
		.split(/\s+/)
		.map((item) => item.trim())
		.filter(Boolean);
}

async function ensureCommands(commands: string[]) {
	for (const cmd of commands) {
		if (!Bun.which(cmd)) {
			console.error(`Missing required command: ${cmd}`);
			process.exit(1);
		}
	}
}

async function assertCleanGit() {
	const status = (await $`git status --porcelain`.text()).trim();
	if (status) {
		console.error(
			"Working tree is dirty. Set ALLOW_DIRTY=1 to skip this check.",
		);
		process.exit(1);
	}
}

async function resolveVersion(): Promise<string> {
	if (env.VERSION?.trim()) {
		validateSemver(env.VERSION.trim());
		return env.VERSION.trim();
	}

	const versionFilePath = getVersionFilePath(config.channel);
	const versionFile = await readFileOrNull(versionFilePath);
	const manifestVersion = await readCargoVersion();
	const base = versionFile ?? manifestVersion ?? "0.0.0";

	if (config.bumpStrategy === "none") {
		validateSemver(base);
		return base;
	}

	const next = bumpVersion(base, config.bumpStrategy);
	validateSemver(next);
	return next;
}

function validateSemver(value: string) {
	if (!/^\d+\.\d+\.\d+([+-][\w.-]+)?$/.test(value)) {
		console.error(`VERSION must be semantic (received "${value}")`);
		process.exit(1);
	}
}

async function readFileOrNull(filePath: string): Promise<string | null> {
	try {
		await access(filePath);
		return (await Bun.file(filePath).text()).trim() || null;
	} catch {
		return null;
	}
}

async function readCargoVersion(): Promise<string | null> {
	const contents = await readFileOrNull(CARGO_TOML);
	if (!contents) return null;
	const match = contents.match(/^\s*version\s*=\s*"([^"]+)"/m);
	return match?.[1] ?? null;
}

function bumpVersion(value: string, strategy: string): string {
	const match = value.match(/^(\d+)\.(\d+)\.(\d+)/);
	if (!match) {
		console.error(
			`Unable to auto-bump version "${value}". Provide VERSION or set VERSION_AUTO_BUMP=none.`,
		);
		process.exit(1);
	}
	const [major, minor, patch] = match.slice(1).map(Number);
	if (major === undefined) throw new Error("Invalid major version");
	if (minor === undefined) throw new Error("Invalid minor version");
	if (patch === undefined) throw new Error("Invalid patch version");
	switch (strategy) {
		case "major":
			return `${major + 1}.0.0`;
		case "minor":
			return `${major}.${minor + 1}.0`;
		case "none":
			return value;
		default:
			return `${major}.${minor}.${patch + 1}`;
	}
}

async function syncVersions(version: string) {
	console.log("==> Syncing project metadata");
	const versionFile = getVersionFilePath(config.channel);

	if (config.dryRun) {
		console.log(` [dry-run] would write version ${version} to ${path.basename(versionFile)}`);
		return;
	}

	console.log(` Writing version ${version} to ${path.basename(versionFile)}`);
	await Bun.write(versionFile, `${version}\n`);
	await updateTomlVersion(CARGO_TOML, version);

	const lockContents = await readFileOrNull(CARGO_LOCK);
	if (lockContents?.includes('name = "audetic"')) {
		await updateTomlVersion(CARGO_LOCK, version, 'name = "audetic"');
	}
}

async function updateTomlVersion(
	filePath: string,
	version: string,
	anchor?: string,
) {
	const contents = await Bun.file(filePath).text();
	const regex = anchor
		? new RegExp(`(${anchor}[\\s\\S]*?version = ")([^"]+)(")`)
		: /(\[package\][\s\S]*?^version\s*=\s*")([^"]+)(")/m;
	const next = contents.replace(regex, `$1${version}$3`);
	await Bun.write(filePath, next);
}

async function maybeRunTests() {
	if (config.skipTests) {
		console.log("==> Skipping tests (SKIP_TESTS=1)");
		return;
	}
	if (config.dryRun) {
		console.log("==> [dry-run] cargo test");
		return;
	}
	console.log("==> cargo test");
	await $`cargo test`;
}

async function ensureNotes(version: string) {
	const releaseDir = path.join(RELEASES_ROOT, version);
	const notesPath = path.join(releaseDir, "notes.md");
	await mkdir(releaseDir, { recursive: true });
	if (config.dryRun) {
		console.log(`==> [dry-run] ensure ${notesPath}`);
		return;
	}
	try {
		await access(notesPath);
	} catch {
		const content = `# Audetic ${version}\n\n- TODO: describe highlights.\n`;
		await Bun.write(notesPath, content);
	}
}

async function buildTarget(targetId: string, rustTarget: string) {
	const builder = config.useCross ? "cross" : "cargo";
	const featureArgs = config.extraFeatures
		? ["--features", config.extraFeatures]
		: [];
	const label = `${builder} build --release --target ${rustTarget}${
		featureArgs.length ? ` --features ${config.extraFeatures}` : ""
	}`;
	console.log(`==> [${targetId}] ${label}`);
	if (config.dryRun) {
		return;
	}
	if (featureArgs.length) {
		await $`${builder} build --release --target ${rustTarget} --features ${config.extraFeatures}`;
	} else {
		await $`${builder} build --release --target ${rustTarget}`;
	}
}

async function packageTarget(
	version: string,
	targetId: string,
	rustTarget: string,
	tmpRoot: string,
): Promise<Artifact> {
	const binaryPath = path.join(
		ROOT_DIR,
		"target",
		rustTarget,
		"release",
		"audetic",
	);
	await assertPath(binaryPath, "compiled binary");

	const stageDir = path.join(tmpRoot, targetId);
	await mkdir(stageDir, { recursive: true });

	await copyFile(binaryPath, path.join(stageDir, "audetic"));
	await assertPath(SERVICE_FILE, "audetic.service");
	await copyFile(SERVICE_FILE, path.join(stageDir, "audetic.service"));
	await assertPath(EXAMPLE_CONFIG, "example_config");
	await copyFile(EXAMPLE_CONFIG, path.join(stageDir, "example_config.toml"));
	await Bun.write(
		path.join(stageDir, "README.txt"),
		`Audetic ${version} (${targetId})

Files:
  audetic             - main binary
  audetic.service     - systemd user unit template
  example_config.toml - starter configuration

Installation instructions: https://install.audetic.ai/
`,
	);

	const releaseDir = path.join(RELEASES_ROOT, version);
	await mkdir(releaseDir, { recursive: true });

	const archiveName = `audetic-${version}-${targetId}.tar.gz`;
	const archivePath = path.join(releaseDir, archiveName);
	await $`tar -C ${stageDir} -czf ${archivePath} .`;

	const sha = await sha256File(archivePath);
	await Bun.write(
		`${archivePath}.sha256`,
		`${sha}  ${path.basename(archivePath)}\n`,
	);
	const size = (await stat(archivePath)).size;
	return { targetId, archivePath, sha, size };
}

async function assertPath(filePath: string, label: string) {
	try {
		await access(filePath);
	} catch {
		throw new Error(`Missing ${label} at ${filePath}`);
	}
}

async function sha256File(filePath: string): Promise<string> {
	const hasher = new Bun.CryptoHasher("sha256");
	for await (const chunk of Bun.file(filePath).stream()) {
		hasher.update(chunk);
	}
	return hasher.digest("hex");
}

async function writeManifest(version: string, artifacts: Artifact[]) {
	if (!artifacts.length) return;
	const manifestPath = path.join(RELEASES_ROOT, version, "manifest.json");
	let manifest: Record<string, unknown> = {};
	try {
		const contents = await Bun.file(manifestPath).text();
		manifest = JSON.parse(contents);
	} catch {
		manifest = {};
	}

	const targets = (manifest.targets as Record<string, unknown>) ?? {};
	for (const artifact of artifacts) {
		targets[artifact.targetId] = {
			archive: path.basename(artifact.archivePath),
			sha256: artifact.sha,
			size: artifact.size,
		};
	}

	const next = {
		...manifest,
		version,
		channel: config.channel,
		release_date: config.releaseDate,
		notes_url: `https://install.audetic.ai/cli/releases/${version}/notes.md`,
		targets,
	};
	await Bun.write(manifestPath, `${JSON.stringify(next, null, 2)}\n`);
}

async function publishAssets() {
	if (!Bun.which("godeploy")) {
		console.warn("godeploy not found; skipping publish step.");
		return;
	}
	console.log("==> godeploy deploy --clear-cache");
	await $`godeploy deploy --clear-cache`;
}

async function tagRelease(version: string) {
	if (config.skipTag) {
		console.log("==> Skipping git tag (SKIP_TAG=1)");
		return;
	}
	const ref = `refs/tags/v${version}`;
	try {
		await $`git rev-parse --quiet --verify ${ref}`.quiet();
		console.warn(`Tag v${version} already exists. Skipping.`);
		return;
	} catch {
		// missing tag
	}
	await $`git tag -a ${`v${version}`} -m ${`Audetic ${version}`}`;
	await $`git push origin ${`v${version}`}`;
}

async function format() {
	if (config.dryRun) {
		console.log("==> [dry-run] skip format");
		return;
	}

	console.log("==> formating release files");
	await $`bun fmt`;
}

async function commitAndPush(version: string) {
	if (config.dryRun) {
		console.log("==> [dry-run] skip git commit/push");
		return;
	}
	if (!config.autoCommit) {
		console.log("==> Skipping git commit/push (AUTO_COMMIT=0)");
		return;
	}

	console.log("==> Staging release artifacts");
	await $`git add --all`;
	const staged = (await $`git diff --cached --name-only`.text()).trim();
	if (!staged) {
		console.log("==> Nothing to commit");
		return;
	}

	const message =
		env.RELEASE_COMMIT_MESSAGE?.trim() || `chore(release): v${version}`;
	console.log(`==> git commit -m "${message}"`);
	await $`git commit -m ${message}`;
	console.log("==> git push");
	await $`git push`;
}

function printSummary(artifacts: Artifact[], failures: TargetFailure[]) {
	if (artifacts.length) {
		console.log("==> Artifacts");
		for (const artifact of artifacts) {
			const relative = path.relative(ROOT_DIR, artifact.archivePath);
			console.log(
				`  - ${artifact.targetId}: ${relative} (sha256 ${artifact.sha}, ${artifact.size} bytes)`,
			);
		}
	} else {
		console.log("==> No artifacts produced");
	}

	if (failures.length) {
		console.warn(
			`Failed targets: ${failures
				.map((entry) => `${entry.targetId} (${entry.error.message})`)
				.join(", ")}`,
		);
	}
}
