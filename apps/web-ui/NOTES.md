# apps/web-ui — notes

## What this is

Browser-only SPA that talks directly to the audetic daemon at `127.0.0.1:3737`. No Electron. Eventual replacement for `apps/ui` (which we're keeping until web-ui reaches parity, then deleting).

Stack matches the renderer side of `apps/ui`: React 19, Vite, Tailwind v4, MobX (`<Observer>` only — see `feedback_mobx.md`), react-router-dom v6 data routes, openapi-fetch + openapi-typescript, Radix + lucide + sonner.

## Run it

```bash
# 1. start the daemon (separate terminal)
cargo run -p audetic    # or systemctl --user start audetic

# 2. install + run
cd apps/web-ui
bun install
bun run codegen   # regenerates src/api/schema.ts from the running daemon
bun run dev       # vite at http://localhost:5173 (or :5174 if busy)
```

Daemon must be running first — the SPA does not spawn it.

## What landed (commit c4e0246)

- Scaffold: `package.json`, `vite.config.ts`, `tsconfig.json`, `index.html`, `src/main.tsx`.
- Renderer copied from `apps/ui/src/renderer/src/` (api, lib, stores, components, routes).
- Adapted away from Electron IPC: `ui-store` → localStorage; `root-store` drops install + appUpdate; `settings/{layout,config-file,updates}` adapted; `transcription-card` wrapped in `<Observer>`.
- Dropped: dashboard route, install-store, app-update-store, onboarding-card.
- New: `components/command-bar.tsx` — sticky top bar with status pill, daemon-down chip, dictation toggle, meeting entry. Subsumes `<ActiveMeetingBanner/>` below it.
- Sidebar: Dictations / Meetings / Settings (no Dashboard). `/` redirects to `/dictations`.

## Smoke verified

`/dictations` (50 entries), `/meetings` (45 meetings), `/settings/{keybind,updates,config-file}` render. Theme switch persists to `localStorage["audetic.themeMode"]`. Daemon-down chip wired to `daemonReachable`. MobX warnings down ~98% (only pre-existing strict-mode loader noise remains).

## Next steps (deferred from this PR)

These all live in `apps/ui/src/main/` + `apps/ui/src/preload/` today and need a different home (or to die) before `apps/ui` can be deleted:

- **Daemon spawn / lifecycle.** Web SPA assumes daemon is already running. Either keep manual / systemd as the supported path, or build a small launcher (Tauri shell? a desktop entry that starts the daemon and opens a browser to the served UI?).
- **Onboarding installer.** Today: detect bundled vs installed daemon version, install systemd unit, install ffmpeg. Web has no install story — figure out distribution (curl-bash + how the user gets to the SPA).
- **Hosting the built SPA.** `vite build` makes static files. Options: serve from the daemon itself (add a static-files route to crates/audetic), serve from a tiny separate daemon, or ship as a tarball the user `python -m http.server`'s. Pick before we ship.
- **Tray icon.** No browser equivalent. If we still want a tray, it lives in whatever shell launches the daemon.
- **Auto-update of the UI itself.** `electron-updater` made sense for the Electron app binary. Browser SPA = users hard-refresh. The daemon's own update endpoints (`/update/check`, `/update/install`) already work and are wired in `/settings/updates`.
- **Native dialogs / shell.openPath.** Replaced for `config-file` with a Copy button. If other places need it, decide per-feature whether browser-friendly UX is enough.
- **Window state persistence, deep links, packaging.** All Electron-specific. Skip.

## Things I'd revisit but didn't here

- **Settings/Updates auto-update flag is locally tracked** — daemon doesn't expose `GET /update/auto`. If we want truth-on-reload, daemon needs a getter.
- **MobX `observableRequiresReaction` warnings on loader reads** — strict mode warns about route loaders calling `getRootStore().history.load(...)`. Pre-existing; benign. Could silence per-call with `untracked()` if the warnings get noisy.
- **`apps/ui` parity** — meeting-detail and meetings list were copied verbatim and work, but I haven't actually started a meeting in web-ui to confirm the full lifecycle (start → record → stop → compress → transcribe → auto-nav). Worth doing before declaring v1 done.
- **No build / typecheck in CI yet.** Add `apps/web-ui` to whatever check runs on PRs.
