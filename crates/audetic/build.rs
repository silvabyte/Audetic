//! Build the apps/web-ui SPA so it can be embedded into the daemon binary.
//!
//! The compiled assets land at `apps/web-ui/dist/` and are pulled in via
//! `include_dir!` from `src/api/static_assets.rs`. Re-runs only when the SPA
//! source changes. If `apps/web-ui/node_modules` is missing (fresh clone),
//! this runs `bun install` first, so a plain `cargo build` / `make build` /
//! the release pipeline don't require a separate `make ui-install`.

use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let web_ui = manifest_dir.join("../../apps/web-ui");
    let web_ui = web_ui.canonicalize().unwrap_or(web_ui);

    // Re-run the SPA build only when source the bundle depends on changes.
    for path in [
        "src",
        "index.html",
        "package.json",
        "bun.lock",
        "tsconfig.json",
        "vite.config.ts",
    ] {
        println!("cargo:rerun-if-changed={}", web_ui.join(path).display());
    }
    println!("cargo:rerun-if-env-changed=AUDETIC_SKIP_UI_BUILD");

    // Escape hatch: `AUDETIC_SKIP_UI_BUILD=1 cargo build` for environments
    // without bun (e.g. minimal docker images that fetch a prebuilt dist).
    if std::env::var_os("AUDETIC_SKIP_UI_BUILD").is_some() {
        ensure_dist_exists(&web_ui.join("dist"));
        return;
    }

    if !has_command("bun") {
        eprintln!(
            "cargo:warning=`bun` not in PATH; skipping web-ui build. \
             Install bun (https://bun.sh) or set AUDETIC_SKIP_UI_BUILD=1."
        );
        ensure_dist_exists(&web_ui.join("dist"));
        return;
    }

    // `bun run build` needs node_modules; install on a fresh clone so callers
    // don't have to remember `make ui-install` first.
    if !web_ui.join("node_modules").exists() {
        println!("cargo:warning=apps/web-ui/node_modules missing — running `bun install`");
        run_bun(&web_ui, &["install"], "bun install");
    }

    run_bun(&web_ui, &["run", "build"], "bun run build");
}

/// Invoke `bun` in `dir` with `args`; panic with `label` on spawn or non-zero exit.
fn run_bun(dir: &Path, args: &[&str], label: &str) {
    let status = Command::new("bun")
        .args(args)
        .current_dir(dir)
        .status()
        .unwrap_or_else(|e| panic!("failed to invoke `{label}` for {}: {e}", dir.display()));
    if !status.success() {
        panic!("`{label}` failed in {}", dir.display());
    }
}

fn has_command(cmd: &str) -> bool {
    Command::new(cmd)
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// `include_dir!` panics at compile time if the directory is missing. When
/// we skip the build (no bun, or AUDETIC_SKIP_UI_BUILD), drop a placeholder
/// so the macro still resolves.
fn ensure_dist_exists(dist: &Path) {
    if dist.exists() {
        return;
    }
    if let Err(err) = std::fs::create_dir_all(dist) {
        panic!("failed to create {}: {err}", dist.display());
    }
    let placeholder = dist.join("index.html");
    let body = "<!doctype html><meta charset=utf-8><title>audetic</title>\
                <p>UI bundle not built. Run <code>bun --cwd apps/web-ui run build</code> \
                or unset <code>AUDETIC_SKIP_UI_BUILD</code> and rebuild.</p>";
    if let Err(err) = std::fs::write(&placeholder, body) {
        panic!("failed to write {}: {err}", placeholder.display());
    }
}
