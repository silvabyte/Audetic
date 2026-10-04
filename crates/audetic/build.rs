//! Build the apps/web-ui SPA so it can be embedded into the daemon binary.
//!
//! The compiled assets land at `apps/web-ui/dist/` and are pulled in via
//! `include_dir!` from `src/api/static_assets.rs`. Re-runs only when the SPA
//! source changes. Every UI rebuild first synchronizes dependencies with the
//! committed lockfile, including in existing checkouts after `git pull`.

use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let web_ui = manifest_dir.join("../../apps/web-ui");
    let web_ui = web_ui.canonicalize().unwrap_or(web_ui);

    embed_macos_info_plist(&manifest_dir);

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
    if std::env::var("AUDETIC_SKIP_UI_BUILD").as_deref() == Ok("1") {
        println!(
            "cargo:warning=AUDETIC_SKIP_UI_BUILD=1: using existing UI assets or a placeholder"
        );
        ensure_dist_exists(&web_ui.join("dist"));
        return;
    }

    if !has_command("bun") {
        panic!(
            "Bun is required to build the bundled web UI. \
             Install Bun (https://bun.sh) and ensure `bun --version` works. \
             For an intentional backend-only/prebuilt-UI build, set AUDETIC_SKIP_UI_BUILD=1."
        );
    }

    // Directory existence says nothing about freshness after a source upgrade.
    // Bun's incremental install is cheap when current; frozen mode prevents a
    // build from silently changing the dependency graph committed by the author.
    run_bun(
        &web_ui,
        &["install", "--frozen-lockfile"],
        "bun install --frozen-lockfile",
    );

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

/// Embed `macos/Info.plist` into linked artifacts as a `__TEXT,__info_plist`
/// Mach-O section when building for macOS. Without this, the OS will not
/// present `NSMicrophoneUsageDescription` / `NSScreenCaptureUsageDescription`
/// prompts and the audio APIs return either silence or `kTCCServiceDisabled`.
///
/// Applied to both the main `audetic` binary and to examples (so the
/// smoke-test example also gets correct permission prompts). Tests are not
/// targeted: they tend not to hit gated APIs and embedding the plist there
/// would tie unit-test binaries to a fixed bundle identifier.
fn embed_macos_info_plist(manifest_dir: &Path) {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return;
    }
    let plist = manifest_dir.join("macos").join("Info.plist");
    if !plist.exists() {
        println!(
            "cargo:warning=macos/Info.plist not found at {} — \
             macOS permission prompts will not appear.",
            plist.display()
        );
        return;
    }
    println!("cargo:rerun-if-changed={}", plist.display());
    let plist_str = plist
        .to_str()
        .expect("Info.plist path is not valid UTF-8 — refusing to embed");
    let arg = format!("-Wl,-sectcreate,__TEXT,__info_plist,{plist_str}");
    println!("cargo:rustc-link-arg-bins={arg}");
    println!("cargo:rustc-link-arg-examples={arg}");
}

fn has_command(cmd: &str) -> bool {
    Command::new(cmd)
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// `include_dir!` panics at compile time if the directory is missing. When
/// we explicitly skip the build (AUDETIC_SKIP_UI_BUILD=1), drop a placeholder
/// so the macro still resolves.
fn ensure_dist_exists(dist: &Path) {
    if dist.join("index.html").is_file() {
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
