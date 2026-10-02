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

- `/audio-notes` — one chronological stream of captured and imported audio notes (`/` redirects here).
  Search titles/transcripts, filter by inferred classification (including future kinds), and paginate.
- `/audio-notes/:id` — raw transcript, audio playback and segment seeking, title editing/regeneration,
  classification/enrichment progress and retries, templates/artifacts, and soft deletion. The detail
  workspace keeps playback mounted across Transcript, Generated, and Details tabs; generated talking
  points add seekable chapter markers. Manual Classification is edited under Details and remains
  separate from the visible AI suggestion.
  Transcription and enrichment are independent: failed AI processing never hides the raw transcript.
- `/settings/{setup,provider,capture,keybind,post-processing,integrations,appearance,config-file}` — Setup Center is the
  read-only machine capability overview; Provider offers typed validation, save, and daemon restart
- `components/command-bar.tsx` — one capture control and status stream, owned by `AudioNotesStore`.
  Microphone capture is the default; optional system audio and review/trim are acquisition options,
  not a choice of meaning. Automatic paste and clipboard copying default off. Settings → Capture &
  delivery reads/writes the daemon-persisted preference through `GET`/`PUT /audio-notes/settings`.
  `AudioNoteSettingsStore` stays safely off until that preference loads; load/save errors are visible.
  Capture options use `auto_paste: null` to inherit a loaded preference, or explicit `false`/`true` to
  override it for one recording. Overrides reset after capture starts; source/review/clipboard
  options last for the app session. Saves never change an active capture. Delivery is raw text before
  asynchronous AI processing, never generated output. No delivery setting is stored in localStorage.
- `components/audio-review-panel.tsx`, `transcript-player.tsx`, `note-title-header.tsx`, and
  `note-artifacts-panel.tsx` are reusable note components. There are no separate dictation/meeting
  stores, routes, or status polls, and background completions do not unexpectedly change routes.
- `stores/integrations-store.ts` and Settings → Integrations manage one-time scoped webhook keys,
  display the isolated public ingress endpoints, configure the official Plaud CLI sync, and show recent
  delivery outcomes. The browser never talks to the public ingress listener directly.
- `stores/setup-store.ts` — consumes `GET /api/setup` for workflow and machine readiness. Missing
  FFmpeg never blocks the application; Settings → Setup offers the app-local installer using
  `onboarding-store`'s existing `POST /api/system/install-ffmpeg` status polling.

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
a real running daemon. The daemon allowlists the local Vite origins; in production the SPA is
same-origin, and browser requests from other origins are rejected before handlers run.

`bun run codegen` regenerates `src/api/schema.ts` from `../../target/openapi.json`, exported from
the daemon's utoipa document. Run it after changing daemon routes/schemas. Never hand-edit the
generated schema or cast around incompatible endpoints. The post-processing client uses the same
typed `daemon` client and the single `audio_note.completed` event.

`bun run test` runs the Observer lint-rule tests and Audio Notes behavior tests (Bun, real typed
client with an injected network boundary, plus React server-render assertions). Tests cover stream
queries, stale responses, safe delivery defaults, multipart imports, trim failures, deleted-note
races, transcript playback without timestamps, independent enrichment errors/retry, persisted
delivery settings, safe pre-load/failure behavior, and nullable/explicit capture overrides. These are
not browser QA; verify microphone/system capture and clipboard delivery against a real daemon.

`make ui-typecheck` (`bun run typecheck`) is the only check unique to this package; `make quality`
runs it alongside the Rust gate. CI (`.github/workflows/rust.yml`) installs `bun`, runs
`bun install` + `bun run typecheck`, then builds/tests the daemon — which exercises the real
`bun run build` + `include_dir!` embedding.

## Install story

Build from source: `make install` → `audeticd install`
(`crates/audetic/src/install/mod.rs`) — user-local, no sudo: copies the daemon to
`~/.local/share/audetic/bin/`, writes `~/.config/systemd/user/audeticd.service`, `enable --now`s it,
waits for `127.0.0.1:3737`, and opens `http://127.0.0.1:3737/` in the browser.
There is no hosted installer and no auto-updater — see
`docs/adr/0001-source-only-distribution.md`. `make uninstall` reverses it.

## Things I'd revisit

- **Daemon lifecycle is Linux-only.** `audetic install` assumes systemd user units and `xdg-open`.
  There's no story for macOS/Windows (launchd plist, a different launcher, etc.) — and the SPA still
  assumes the daemon is already running.
- **Tray on macOS lives in the menu-bar agent.** `apps/menubar-macos` (SwiftUI `MenuBarExtra`) now
  surfaces daemon status, audio note capture controls, "Open Audetic", and
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
- **Capture browser QA is separate from automated UI tests.** Exercise start → record → stop →
  review/trim → transcribe, then classification/artifacts against the real daemon before release.
- **MobX `observableRequiresReaction` warnings on loader reads.** Strict mode warns when route
  loaders call store methods that read observables outside a reaction. Pre-existing and benign; could
  silence per-call with `untracked()` if it gets noisy.
