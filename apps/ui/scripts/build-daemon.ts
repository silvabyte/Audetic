#!/usr/bin/env bun
/**
 * Pre-build hook for electron-builder.
 *
 * Builds the Audetic daemon for the requested target(s), stages the binary
 * into apps/ui/resources/bin/audetic-${arch}, and syncs apps/ui/package.json
 * `version` from the workspace's [workspace.package].version so the bundled
 * daemon and the shell app advertise the same version string.
 *
 * Usage:
 *   bun apps/ui/scripts/build-daemon.ts            # native target
 *   bun apps/ui/scripts/build-daemon.ts --target=x86_64-unknown-linux-gnu
 *   bun apps/ui/scripts/build-daemon.ts --target=aarch64-apple-darwin
 *
 * Multiple --target flags can be supplied (comma-separated or repeated).
 */

import { mkdir, copyFile, readFile, writeFile } from "node:fs/promises";
import { existsSync } from "node:fs";
import path from "node:path";
import { $ } from "bun";

const ROOT_DIR = path.resolve(import.meta.dir, "../../..");
const APP_DIR = path.resolve(import.meta.dir, "..");
const STAGE_DIR = path.join(APP_DIR, "resources", "bin");
const ROOT_CARGO_TOML = path.join(ROOT_DIR, "Cargo.toml");
const APP_PACKAGE_JSON = path.join(APP_DIR, "package.json");

interface TargetSpec {
  /** Cargo target triple, e.g. x86_64-unknown-linux-gnu */
  triple: string;
  /** Cargo features to pass; omit for the host's defaults. */
  features?: string;
  /** Whether to pass --no-default-features. */
  noDefaultFeatures?: boolean;
}

const KNOWN_TARGETS: Record<string, TargetSpec> = {
  // Linux — keep the linux-audio default features so SystemAudioSource
  // (PipeWire) is compiled in.
  "x86_64-unknown-linux-gnu": { triple: "x86_64-unknown-linux-gnu" },
  "aarch64-unknown-linux-gnu": { triple: "aarch64-unknown-linux-gnu" },
  // macOS — Phase 7 wires up the macos-audio feature; for now leave the cargo
  // build to use the host's default features. Phase 7 swaps these entries to
  // include `--features macos-audio --no-default-features`.
  "aarch64-apple-darwin": { triple: "aarch64-apple-darwin" },
  "x86_64-apple-darwin": { triple: "x86_64-apple-darwin" },
};

async function main(): Promise<void> {
  const targets = parseTargets();
  await ensureStageDir();
  await syncVersion();
  for (const t of targets) {
    await buildAndStage(t);
  }
}

function parseTargets(): TargetSpec[] {
  const raw: string[] = [];
  for (const arg of Bun.argv.slice(2)) {
    if (arg.startsWith("--target=")) raw.push(...arg.slice(9).split(","));
  }
  if (raw.length === 0) return [{ triple: "" }]; // host default
  return raw.map((triple) => {
    const known = KNOWN_TARGETS[triple];
    if (!known) {
      console.warn(
        `! unknown target ${triple}; building with no extra cargo flags`,
      );
      return { triple };
    }
    return known;
  });
}

async function ensureStageDir(): Promise<void> {
  await mkdir(STAGE_DIR, { recursive: true });
}

async function syncVersion(): Promise<void> {
  const cargo = await readFile(ROOT_CARGO_TOML, "utf8");
  const m = cargo.match(/\[workspace\.package\][\s\S]*?^version\s*=\s*"([^"]+)"/m);
  if (!m) {
    console.error("could not find [workspace.package].version in", ROOT_CARGO_TOML);
    process.exit(1);
  }
  const cargoVersion = m[1];

  const pkgRaw = await readFile(APP_PACKAGE_JSON, "utf8");
  const pkg = JSON.parse(pkgRaw) as { version?: string };
  if (pkg.version === cargoVersion) {
    console.log(`==> version already in sync (${cargoVersion})`);
    return;
  }
  console.log(`==> syncing apps/ui/package.json version: ${pkg.version} -> ${cargoVersion}`);
  // Preserve the file's existing formatting / key order: replace just the
  // version field via a regex rather than re-stringifying.
  const next = pkgRaw.replace(
    /"version"\s*:\s*"[^"]+"/,
    `"version": "${cargoVersion}"`,
  );
  await writeFile(APP_PACKAGE_JSON, next);
}

async function buildAndStage(target: TargetSpec): Promise<void> {
  const triple = target.triple || (await hostTriple());
  console.log(`==> cargo build --release for ${triple}`);

  const args = ["build", "--release", "-p", "audetic"];
  if (target.triple) args.push("--target", target.triple);
  if (target.noDefaultFeatures) args.push("--no-default-features");
  if (target.features) args.push("--features", target.features);

  $.cwd(ROOT_DIR);
  await $`cargo ${{ raw: args.join(" ") }}`;

  const binarySource = target.triple
    ? path.join(ROOT_DIR, "target", target.triple, "release", "audetic")
    : path.join(ROOT_DIR, "target", "release", "audetic");
  if (!existsSync(binarySource)) {
    console.error("expected binary not found at", binarySource);
    process.exit(1);
  }

  const arch = archForTriple(triple);
  const dest = path.join(STAGE_DIR, `audetic-${arch}`);
  await copyFile(binarySource, dest);
  console.log(`==> staged ${path.relative(ROOT_DIR, binarySource)} -> ${path.relative(ROOT_DIR, dest)}`);
}

async function hostTriple(): Promise<string> {
  // Use rustc to get the canonical host triple. Avoids hand-rolling the
  // platform/arch mapping.
  const out = await $`rustc -vV`.text();
  const m = out.match(/^host:\s*(.+)$/m);
  if (!m) throw new Error("could not parse rustc -vV output");
  return m[1].trim();
}

function archForTriple(triple: string): "x64" | "arm64" | string {
  if (triple.startsWith("x86_64")) return "x64";
  if (triple.startsWith("aarch64")) return "arm64";
  return triple.split("-")[0];
}

await main();
