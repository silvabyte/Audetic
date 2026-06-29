# apps/web-ui — notes

## What this is

The Audetic UI: a browser-only SPA, **bundled into the daemon binary** and served at
`http://127.0.0.1:3737/`. There is no Electron app — `apps/ui/` and the `platform/` folder were
deleted; this is the only UI.

Stack: React 19, Vite, Tailwind v4 (`@tailwindcss/vite`, `@theme inline` — no `tailwind.config.ts`),
MobX (`<Observer>` only, strict mode — see `feedback_mobx.md`), `react-router-dom` v6 data routes
(loaders are thin bridges to store methods, no `useLoaderData`), `openapi-fetch` +
`openapi-typescript`, Radix primitives + lucide + sonner.

Routes / surface:

- `/dictations` — voice-to-text history (the index route; `/` redirects here)
- `/meetings` and `/meetings/:id` — meeting list + detail, with an auto-nav reaction that jumps to
  `/meetings/:id` when a meeting finishes its pipeline
- `/settings/{provider,keybind,updates,appearance,config-file}`
- `components/command-bar.tsx` — omnipresent sticky strip: a live state orb (pulses/glows on the
  daemon's dictation / meeting / pipeline state), a daemon-down chip, and icon actions to toggle
  dictation and meeting. `<ActiveMeetingBanner/>` renders below it for meeting-only affordances.
- `components/onboarding-overlay.tsx` — first-run gate driven by `onboarding-store`: checks
  `GET /api/system/deps` for ffmpeg, and if missing walks the user through
  `POST /api/system/install-ffmpeg` + status polling. (Daemon binary install is done by
  `audetic install` before the SPA ever loads, so ffmpeg is the only first-run gate left.)

## How it's built and embedded

`crates/audetic/build.rs` runs `bun run build` in this directory at compile time; the output lands
in `apps/web-ui/dist/`, which `crates/audetic/src/api/static_assets.rs` pulls in via
`include_dir!("$CARGO_MANIFEST_DIR/../../apps/web-ui/dist")`. `crates/audetic/src/api/mod.rs` mounts
the API under `/api` and uses `serve_static` as the fallback, so the SPA is served at `/` with a
history fallback to `index.html` (hashed `/assets/*` get a long cache; `index.html` is `no-cache`).

- `bun install` must have been run once in this checkout before `cargo build` works (the build script
  invokes `bun run build`, which needs `node_modules`). `make ui-install` does this.
- Escape hatch: `AUDETIC_SKIP_UI_BUILD=1 cargo build` skips the SPA build (and `build.rs` also
  silently skips if `bun` isn't on PATH). In either case it drops a placeholder `dist/index.html` so
  `include_dir!` still resolves — the daemon builds but serves a "UI not built" stub. Use this only
  for environments without `bun`; CI builds the real bundle.

## Run it in dev

```bash
# 1. start the daemon first — the SPA does NOT spawn it
cargo run -p audetic        # or: systemctl --user start audetic

# 2. (once per checkout) install deps
make ui-install             # cd apps/web-ui && bun install

# 3. dev server
make ui-dev                 # cd apps/web-ui && bun run dev — vite at :5173 (or :5174 if busy)
```

Vite proxies `/api` to the daemon at `127.0.0.1:3737` (see `vite.config.ts`), so the dev SPA talks to
a real running daemon. Permissive CORS on the daemon makes this work cross-origin; in production the
SPA is same-origin.

`make codegen` (`bun run codegen`) regenerates `src/api/schema.ts` from the running daemon's
`GET /api/openapi.json` (utoipa). Run it after changing daemon routes/schemas.

`make ui-typecheck` (`bun run typecheck`) is the only check unique to this package; `make quality`
runs it alongside the Rust gate. CI (`.github/workflows/rust.yml`) installs `bun`, runs
`bun install` + `bun run typecheck`, then builds/tests the daemon — which exercises the real
`bun run build` + `include_dir!` embedding.

## Install story

For end users: `audetic install` (`crates/audetic/src/cli/install.rs` →
`crates/audetic/src/install/mod.rs`) — user-local, no sudo: copies the binary to
`~/.local/share/audetic/bin/`, writes `~/.config/systemd/user/audetic.service`, `enable --now`s it,
waits for `127.0.0.1:3737`, and opens `http://127.0.0.1:3737/` in the browser.
`release/cli/latest.sh` (served at `https://install.audetic.ai/cli/latest.sh`) is the
`curl … | bash` wrapper that downloads the daemon and hands off to `audetic install`; `make
installer-lint` checks it.

## Things I'd revisit

- **Daemon lifecycle is Linux-only.** `audetic install` assumes systemd user units and `xdg-open`.
  There's no story for macOS/Windows (launchd plist, a different launcher, etc.) — and the SPA still
  assumes the daemon is already running.
- **Tray on macOS lives in the menu-bar agent.** `apps/menubar-macos` (SwiftUI `MenuBarExtra`) now
  surfaces daemon status, point-and-click dictation/meeting toggles, "Open Audetic", and
  user-customizable global keyboard shortcuts. It's an independent HTTP consumer of the daemon
  (like the CLI), bundled inside `Audetic.app/Contents/Library/LoginItems` and registered as a
  LaunchAgent (`ai.audetic.menubar`) by `audeticd install`. Linux still uses the Hyprland keybind;
  Windows has no tray yet.
- **Native dialogs are replaced per-feature.** `config-file` swapped `shell.openPath` for a Copy
  button. Other places that wanted a native picker/opener get a browser-friendly UX per-feature; no
  general replacement.
- **Auto-update UI vs the daemon.** Settings → Updates now reads the truth from the daemon:
  `GET /api/update/auto` getter exists and `config-store` loads/writes it via `GET`/`PUT /api/update/auto`,
  and `GET /api/update/check` drives the version card. So the "locally-tracked flag" caveat from the
  Electron era no longer applies. The browser SPA itself updates by hard-refresh against the
  daemon-served bundle (`index.html` is `no-cache`).
- **Meeting lifecycle not exercised end-to-end in web-ui.** The meetings list / detail / banner /
  auto-nav are wired and render, but I haven't actually run a meeting in web-ui to confirm the full
  chain (start → record → stop → compress → transcribe → auto-nav to `/meetings/:id`). Worth doing
  before calling v1 done.
- **MobX `observableRequiresReaction` warnings on loader reads.** Strict mode warns when route
  loaders call store methods that read observables outside a reaction. Pre-existing and benign; could
  silence per-call with `untracked()` if it gets noisy.
