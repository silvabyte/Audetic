# Effect migration

Status: Proposed

Mat wants one set of conventions for the Audetic stack: Effect, in
TypeScript. The priority is that an agent can write a change and prove it
correct as fast as possible. That outranks taste and the smallest edit.
He has picked Effect plus Foldkit for `apps/web-ui`. His hypothesis for
the daemon is Effect on Bun (`@effect/platform-bun`), shipped as a
standalone binary from `bun build --compile`. That hypothesis is tested
below. It is not a requirement.

This spec does not change the running daemons. The recommendation is in
Part B: Effect on Bun for orchestration, with one Rust native module for
capture and local inference.

## Deployment that stays up

Verified 13 November 2026. Both machines keep running, unchanged, through
every step of Part C. Each step leaves `make install` working on macOS and
on Linux, and leaves the installed daemon serving the database that is
already there.

The primary ingest is Mat's MacBook (Apple Silicon). The LaunchAgent
`ai.audetic.daemon` (`RunAtLoad`, `KeepAlive`) runs
`~/Applications/Audetic.app/Contents/MacOS/audeticd`.
`ai.audetic.menubar` runs the embedded menu bar app. The app is v0.1.25,
ad-hoc signed, no Team ID, built from `41887b6` (`#76`, automatic paste
per capture source). That commit contains migrations `0001` and `0002`
only. The database is `~/Library/Application Support/audetic/audetic.db`:
243 notes, 2 migrations applied. The process listens on `127.0.0.1:3737`
and `127.0.0.1:3739`.

The Beelink (Arch Linux) runs a secondary systemd user service,
`audeticd`. It has 23 notes. The newest is from September 2026. Its
migration count was not recorded. It does not share the MacBook's
database.

This checkout is past `41887b6`. `0003_enrichment_status_skipped.sql` is
the third migration. The next `make install` of current `main` on the
MacBook upgrades that database through the existing runner (`VACUUM INTO`,
then the checksummed chain). A replacement has to apply that same chain.
It does not invent a parallel one.

`make install` is still the only install path (ADR 0001). On macOS it
builds `Audetic.app` and runs `audeticd install`, because TCC attributes
Microphone and Screen Recording to the bundle
(`Makefile` install comment, `crates/audetic/src/install/macos.rs`). On
Linux it copies the binary to `~/.local/share/audetic/bin/audeticd` and
writes the user unit.

## Part A. Feasibility

### Subsystems

| Subsystem | Where it lives | Effect | Blocker |
|---|---|---|---|
| Microphone capture | `audio/mic_source.rs`, `audio/input_device.rs`, cpal 0.17 | Recovery policy and the phase machine | The cpal callback runs on the audio thread and copies samples into `Mutex<Vec<f32>>` (`mic_source.rs` `on_data`). Opening the device blocks in `tccd` on macOS. |
| macOS system audio | `audio/system_source/macos.rs`, `audio/system_tap.rs` | Same machine as the mic | cpal opens the default output as a loopback input. The permission comment names `AudioHardwareCreateProcessTap`. ScreenCaptureKit is not linked. Screen Recording TCC is `CGPreflightScreenCaptureAccess` / `CGRequestScreenCaptureAccess` (`audio/system_source/permissions.rs`). |
| Linux system audio | `audio/system_source/linux.rs` | The "mic only if the tool is missing" policy | A `pw-cat --record --raw` child, target from `pactl get-default-sink`, read on a `std::thread`. Output-switch recovery is a no-op. PipeWire is not a library in this crate. |
| Device watcher | `audio/device_watcher/macos.rs` | The debounced `SettledSwitch` command | A dedicated thread and a CoreAudio property listener (`kAudioHardwarePropertyDefaultInputDevice` / `DefaultOutputDevice`). Other OSes are a stub. |
| Resample and mix | `audio/resample.rs` (`rubato`), `audio/audio_mixer.rs` | The policy of when to resample | The callback only downmixes. Resample to the 16 kHz pipeline rate (`app/mod.rs`, both sources constructed at 16000) runs when a segment closes. Pure TS can own the decision. It should not own the buffer. |
| Local inference | `transcription/providers/local_engine.rs`, `transcribe-rs` 0.3.11 | Catalog, download, job state | In-process. `spawn_blocking` calls `transcribe_blocking`. Parakeet is ONNX, CPU. Whisper is whisper.cpp, Metal on macOS (`Cargo.toml` target features), Vulkan only behind the `vulkan` feature. Engines are cached in a process-wide `Mutex`. `whisper_cpp.rs` is a second path that shells out to `whisper-cli`. Models live at `<data_dir>/models/<id>`. |
| Remote providers | `transcription/providers/openai_api.rs`, `assembly_api.rs`, `audetic_api.rs`, `audetic-core/src/jobs_client.rs` | Yes. HTTP, Schema at the boundary, one Layer per provider | None for the HTTP providers. `openai_cli.rs` shells out to the `whisper` Python CLI and needs the same process Layer as the agent runner. |
| Auto-paste, clipboard, hotkeys | `text_io/mod.rs`, `audetic-core/src/clipboard.rs`, `keybind/`, menu bar | The delivery flags and the "paste failed, note stays completed" rule (`audio_notes/processing.rs`) | Linux injection is `wtype`, `ydotool`, or clipboard plus `xdotool` / KDE `qdbus`. Clipboard copy is `arboard`, then `pbcopy` / `wl-copy` / `xclip` / `xsel`. There is no `CGEvent` and no `osascript`. On macOS the injector falls through to `InjectionMethod::Clipboard`, and `simulate_paste` does not press Cmd+V. macOS hotkeys are the menu bar's `KeyboardShortcuts` (`apps/menubar-macos/.../AppState.swift`). Hyprland binds are curl commands written into the compositor config (`keybind/mod.rs`). |
| Menu bar | `apps/menubar-macos`, LaunchAgent `ai.audetic.menubar` | The client is already HTTP on `127.0.0.1:3737` | SwiftUI `MenuBarExtra`. Bundle id `ai.audetic.menubar`. It stays Swift. |
| File watching and imports | `audio_notes/import.rs`, `integrations/ingress.rs`, `integrations/plaud.rs` | Yes | There is no `notify` watcher. Import takes an explicit file. Ingress is a second HTTP server. Plaud is a 60s poll of the `plaud` CLI. |
| SQLite and migrations | `db/migrations/mod.rs`, `db/init.rs`, `audetic-core/src/global.rs` | The checksum rules and the repository | `rusqlite` is synchronous, one connection per call, no pool, no WAL. `schema_migrations` stores version plus SHA-256. `ensure_current` refuses a database that is behind or ahead. `upgrade` runs only under the instance lock, after `VACUUM INTO` to `audetic.db.pre-v{N}.sqlite3`. `bun:sqlite` can open this file. The Effect program has to implement those rules, including the checksum refusal. |
| HTTP API and OpenAPI | `api/mod.rs`, `api/docs.rs` | Yes. `@effect/platform-bun` HTTP, Schema for every body | 67 `utoipa` operations. Loopback only (`url.rs`: `127.0.0.1`, port 3737, `AUDETIC_PORT`). Ingress is a separate listener on 3739 with bearer keys. The main API has an origin allow-list and no session auth. The SPA is `include_dir` of `apps/web-ui/dist` (`build.rs` runs `bun run build`). |
| CLI | `crates/audetic-cli`, binary `audetic` | Yes. `@effect/cli` over HTTP | `audeticd` (install, uninstall, openapi) is the daemon binary, not the CLI. The CLI never opens the database. |
| TOML config | `audetic-core/src/config.rs`, `example_config.toml` | Yes. Schema decode | Loaded once at startup (`app/mod.rs`). No file watcher. A writer must preserve unknown keys so a rollback does not strip the file. |
| launchd and systemd | `install/macos.rs`, `install/linux.rs`, the plist and the unit template | The decision to install | The work is `launchctl` and `systemctl --user`. Templates: `RunAtLoad` and `KeepAlive` on macOS, `Restart=always`, `MemoryMax=6G`, `CPUQuota=80%` on Linux. Effect can spawn those commands. It does not replace them. |
| macOS bundle | `Makefile` `macos-app`, `crates/audetic/macos/Info.plist` | No | `CFBundleIdentifier` `ai.audetic.daemon`, `CFBundleExecutable` `audeticd`, minimum 14.6, microphone and screen-capture usage strings. The menu bar is embedded at `Contents/Library/LoginItems`. Nested binaries are signed inside out. |
| TCC | `audio/system_source/permissions.rs`, `macos/audetic.entitlements` | The watcher is a state machine (poll, then `exit` so KeepAlive restarts) | Grants stick to the code identity. Ad-hoc signing means that identity is the cdhash (`Makefile` `macos-sign` comment). A new binary does not inherit the old grant. Microphone is `AVCaptureDevice`. Screen Recording is CoreGraphics. The grant applies on the next process, which is why the watcher exits. Accessibility is not requested anywhere in the daemon. The menu bar's global shortcuts need it on `ai.audetic.menubar`, a different bundle, so a new daemon binary does not drop that grant. |
| Launch at login | the two LaunchAgents, the systemd `WantedBy=graphical-session.target` | No | There is no `SMAppService`. Login items are the embedded menu bar app plus the `ai.audetic.menubar` agent. |
| Codesign and notarization | `Makefile`, ADR 0001, `audetic.entitlements` | No | Ad-hoc identity `-`, hardened runtime, timestamp none. Notarization and Developer ID were removed. The entitlements file grants `com.apple.security.device.audio-input` only, and says to add `com.apple.security.cs.*` if a dependency needs JIT. A Bun binary needs those keys. See below. |
| CI | `.github/workflows/rust.yml` | The Effect tests are a `bun test` step on the same two runners | Self-hosted Linux (`silvabytebee64`, Arch) and self-hosted macOS. The job installs bun, runs `make install-test`, typechecks and tests the web UI, then `cargo build` and `cargo test`. Audio tests skip device and TCC prompts when `CI` is set. |
| Memory, CPU, startup, soak | the unit file's `MemoryMax` / `CPUQuota` | The runtime Layer is small next to a model | No benchmark in the repo. Models load lazily (`local_engine.rs`), not at boot. Startup is the upgrade, the sweeps, the macOS TCC watcher, config, the device watcher, then the two listeners (`app/mod.rs` `run_service`). The 6G and 80% caps exist because Whisper models are large. Measured Bun numbers are in the next section. They do not include a model or a soak. |

### Bun, native code, cross-compile, entitlements

Checked against Bun 1.4.4 (the version installed on this VM) and the docs
fetched 18 November 2026.

`bun:ffi` is experimental. The FFI doc says not to rely on it in
production, and that a Node-API module is the stable way to call native
code. An earlier bug, oven-sh/bun#30717, where `bun:ffi` `dlopen` of an
embedded library failed inside a compiled binary, is closed (fixed by
#30720, 15 May 2026). Experimental still means the capture seam does not
use `bun:ffi`.

`bun build --compile` embeds `.node` addons. `dlopen` cannot read the
compiled virtual filesystem (`/$bunfs`), so Bun extracts the addon to a
temp file. Current docs also say a sibling library (the sharp / libvips
case) can be embedded with `bun build --compile --aot --asset <dir>`.
oven-sh/bun#44063 is still open (updated 27 September 2026): the compiled
binary extracts the `.node` alone, and the addon's `@rpath` or `$ORIGIN`
does not find the `.dylib` / `.so` that was embedded next to it. This
spec did not build an addon, so it does not claim to reproduce that bug.
The seam below does not depend on the fix. The native module is a file
beside the executable, loaded by absolute path. System frameworks
(CoreAudio, AVFoundation) are not shipped.

`--target` cross-compilation is documented for `bun-darwin-arm64`,
`bun-darwin-x64`, `bun-linux-x64`, `bun-linux-arm64`, plus musl and
Windows variants. This VM produced all four of the first set. See the
measurements. The Linux binaries are dynamically linked to glibc
(`ld-linux`). The Beelink is Arch, so the glibc target is the right one.
musl is available if a later host needs it.

Codesign: Bun's executable doc requires an entitlements plist with
`com.apple.security.cs.allow-jit`,
`com.apple.security.cs.allow-unsigned-executable-memory`,
`com.apple.security.cs.disable-executable-page-protection`,
`com.apple.security.cs.allow-dyld-environment-variables`, and
`com.apple.security.cs.disable-library-validation`, and says codesign
support needs Bun 1.2.4 or newer. Those five keys are exactly the keys
`audetic.entitlements` says not to add until a dependency needs JIT.
Adding them changes the hardened-runtime profile of a process that also
holds the microphone and the screen-recording grant. oven-sh/bun#29120
(truncated ad-hoc signature on `bun-darwin-arm64` in Bun 1.3.12) is
closed. This VM did not codesign and did not run the Mach-O files. The
Mac still signs with `codesign` during `make macos-app`. Bun's
cross-compiled signature is not the TCC identity.

`BunHttpServer` in `@effect/platform-bun` 4.1.4 is annotated
`@stability unstable`. Pin the version. HTTP stays behind one Layer so a
signature change does not spread.

### Measured binary

Measured 18 November 2026 on this Linux x86-64 VM (4 vCPU), Bun 1.4.4,
`effect` 4.1.4, `@effect/platform-bun` 4.1.4. Not measured on the MacBook
or the Beelink. The program is `HttpServer.serve` provided with
`BunHttpServer.layer({ hostname: "127.0.0.1", port })`. The handler opens
`bun:sqlite` in memory, creates `probe`, inserts one row, and returns
`{"ok": true, "n": 1}`. Compiled with `bun build --compile ./server.ts`.
A second binary does the same work with `Bun.serve` and `bun:sqlite` and
no Effect, so the runtime cost is visible.

Cold start is wall time from just before process spawn until the HTTP 200
body is fully read. 20 fresh processes each. The body was
`{"ok":true,"n":1}` every time. RSS is `VmHWM` from `/proc/<pid>/status`
after that response.

| Binary | Size (bytes) | Median | Min | p95 | Max | Median VmHWM |
|---|---:|---:|---:|---:|---:|---:|
| Effect + `bun:sqlite` | 109627568 | 39.47 ms | 37.98 ms | 41.31 ms | 47.96 ms | 37790 KiB |
| `Bun.serve` + `bun:sqlite` | 109123760 | 5.01 ms | 3.81 ms | 7.27 ms | 7.80 ms | 19700 KiB |

`ldd` on the Effect binary: `libc`, `libpthread`, `libdl`, `libm`, and
the glibc loader. Nothing else.

Effect adds 503808 bytes on top of the Bun runtime, about 34 ms of cold
start, and 18090 KiB of high-water mark, for a server that does not load
a model. 40 ms to the first response is a fine launchd start. It is not
a capture deadline. Twenty short processes are not a soak. CPU under
transcription was not measured. The current `audeticd` binary was not
built here, so this spec does not compare sizes with Rust.

Cross-compile from the same host, same source, produced and not executed,
not codesigned:

| `--target` | Size (bytes) | `file` |
|---|---:|---|
| `bun-darwin-arm64` | 89655266 | Mach-O 64-bit arm64 |
| `bun-darwin-x64` | 98659808 | Mach-O 64-bit x86_64 |
| `bun-linux-arm64` | 109693944 | ELF ARM aarch64, `/lib/ld-linux-aarch64.so.1` |
| `bun-linux-x64` | 109627568 | ELF x86-64, `/lib64/ld-linux-x86-64.so.2` |

### What Rust does that TypeScript cannot

The audio callback has a deadline. `on_data` runs on cpal's thread, takes
one mutex, and appends f32 samples (`audio/resample.rs` documents that
budget). A JavaScript heap pause on that thread drops or glitches audio.
Hopping every buffer to the Bun event loop has the same failure mode under
GC. `bun:ffi` on the callback is the experimental API the FFI doc tells
you not to ship, and it still would not give the callback a real-time
thread.

The current code is not zero-copy. The callback borrows `&[f32]` and
copies it into a `Vec`. TypeScript at that boundary would copy too. The
gap is the thread and the CoreAudio listener, not a zero-copy pipeline
that already exists. The file the rest of the daemon sees is a wav path.

Local inference is a native library: ONNX Runtime, whisper.cpp, Metal,
optional Vulkan, loaded in-process and cached. Bun does not compile those.
A subprocess around `whisper-cli` already exists and is the slow path.
The fast path stays a loaded engine.

TCC preflight is Objective-C and CoreGraphics. The process that opens the
device is the process whose cdhash is on the grant
(`install/macos.rs`: the executable has to be inside the `.app`, or the
responsible process is the terminal). A second helper binary is a second
identity unless it is signed into the same bundle and proven. That proof
is larger than an in-process module.

### The native seam

One Rust cdylib, Node-API, loaded by absolute path from the directory of
the running executable (`Contents/MacOS` on macOS, the installed `bin`
directory on Linux). Not `bun:ffi`. Not embedded in the Bun filesystem.
Not a second process.

The module owns the callback, the CoreAudio listener, the wav write, and
`transcribe_blocking`. Effect sees four operations, each a Schema value
in and a Schema value or a tagged error out:

- `start` (microphone, or microphone plus system)
- `stop` → wav path, sample rate, capture state, degraded flag
- `recover` (device switch or stream death)
- `transcribe` (model id, wav path) → text and segments

Samples do not cross into JavaScript. ONNX and whisper, if they are not
statically linked into that one module, sit beside it so `$ORIGIN` /
`@loader_path` resolve. That works because the module was not extracted
to `/tmp`. Pure TypeScript is not a candidate for this seam.

The Mac cutover is gated on a spike that signs a Bun binary with the
current microphone entitlement plus Bun's five JIT keys, under a
different bundle id, and checks that the prompts appear. Until that
spike passes, launchd keeps running the Rust `audeticd`.

## Part B. Alternatives

Scores are 1 to 5. Five is better for an agent who has to write a change
and prove it. "Migration cost" scores 5 when the cost is low. Evidence is
the inventory above, the measurements, and the current CI and pre-commit
hook (`.githooks/pre-commit` runs a release build and clippy).

| Criterion | Keep Rust, Effect only in the UI | Effect on Node | Effect on Bun + Rust module |
|---|---:|---:|---:|
| Typed errors and requirements | 4. Enums are already strong. Writers and SQL still drift (`docs/specs/explicit-state-machines.md`). | 5. Schema, tagged errors, `Match`. | 5. Same, and the native boundary is Schema too. |
| Test and CI loop | 2. Daemon proof is `cargo test` plus the release/clippy hook. UI proof is already `bun test`. | 4. `bun test` or `node --test` is seconds. Native addons rebuild with node-gyp on both runners. | 5. The Effect server compiled in 283 ms here. `bun test` is the loop for orchestration. Cargo remains for the one module. |
| Native fit | 5. cpal, CoreAudio, ONNX, and whisper.cpp are already in process. | 3. N-API is mature. `better-sqlite3` or `node:sqlite` is another addon. The callback still cannot be JS. | 4. Same N-API module, `bun:sqlite` measured inside the compiled binary, callback stays in Rust. `bun:ffi` is rejected. |
| Packaging and signing | 5. `make install` and ad-hoc TCC already work. cdhash still changes every rebuild. | 2. Single-file Node is a worse story than `bun build --compile`, and a SEA binary needs the same JIT entitlements. Not measured here. | 3. Four targets compiled here. JIT entitlements are new. Darwin binaries were not run. cdhash still changes. |
| One convention | 2. Rust daemon, Effect UI. Agents keep two error models and two state-machine encodings. | 4. One language and one Effect style. Node is a second runtime beside the bun UI build. | 5. Effect on both sides. Bun is already the UI toolchain. The Rust that remains is the seam. |
| Migration cost and risk | 5. No daemon rewrite. The live databases stay on the binary that opens them today. | 2. Same rewrite as Bun, plus a sqlite addon and a packaging design this repo does not have. | 2. Strangler is long. TCC, the live databases, and the install path are the risks. Part C is how they stay bounded. |

Rejected: a second process. Rust stays the launchd principal so TCC never
moves, and Effect serves HTTP beside it. That preserves the grant without
a spike, and it splits the command loop in `app/mod.rs` from the server
that receives the command. Every capture call becomes IPC. That is a
wider seam than a cdylib in the process whose cdhash already holds the
grant. Two supervisors, two ports, and the menu bar has to know which one
is alive.

### Recommendation

Build the daemon orchestration as Effect on Bun, and ship it with
`bun build --compile`, with one in-process Rust Node-API module for
capture and local inference.

Agent speed is the priority, and the changes agents actually make are
notes, HTTP, enrichment, and the UI. Those are Schema, tagged errors, and
`Match`. The pure Effect server compiled in under a second here and
answered in about 40 ms. The Rust that TypeScript cannot replace is the
callback, the CoreAudio listener, and the inference library, and that
Rust is one module with a Schema boundary, still covered by `cargo test`.

Effect on Node loses on the pieces this VM measured and on the toolchain
the repo already installs. `bun:sqlite` ran inside the compiled binary
with no addon build. CI already installs Bun for the web UI. Node's
single-file packaging was not measured and is not better than the four
binaries `bun build --compile --target` produced.

Keeping the whole daemon in Rust keeps the best native fit and the
working install, and it keeps two conventions. That loses on the
priority. The explicit-state-machine plan would encode each lifecycle
twice, a Rust enum and a generated TypeScript union.

The recommendation is falsified for the installed process if the Mac
spike in Part C cannot get microphone and screen-recording prompts on an
ad-hoc Bun binary that carries the JIT entitlements. In that case the
launchd process stays the Rust daemon, and Effect stays in the UI and in
code that does not open CoreAudio. That spike happens before any
LaunchAgent is repointed. It is not a reason to start on Node, and it is
not a reason to skip the Effect orchestration work.

## Part C. Plan

Strangler order. The Rust daemon remains what `make install` starts until
the cutover PRs. No PR adds a migration the Rust binary cannot open.
`0003` is already in the Rust chain. New schema waits until both machines
have cut over and rollback no longer means "start yesterday's `audeticd`".

Every PR has `bun test` or `cargo test` in CI on the two self-hosted
runners, a rollback that is `git revert` plus `make install` of the
parent when the PR changed what install copies, and an install check.
Before the cutover, the install check is: `make install` of current
`main` still produces the Rust daemon, and the running MacBook and
Beelink processes are untouched (same binary path, `/api/version` unchanged).
The cutover PRs replace that with a real upgrade check on each machine,
against a copy of the database first and the live file only after the
copy passes. The live file is backed up with the existing `VACUUM INTO`
path before that step.

### PR 1. Characterization suite

Black-box tests against the current `audeticd`. A temp database, never
the live file. Fixtures at migration 2 (the MacBook) and migration 3
(this checkout). The suite asserts the upgrade from 2 to 3, the
`schema_migrations` checksums, the startup sweep (a `recording` row
becomes `error`), note list and detail, enrichment status including an
`error` row whose `enrichment_error` text round-trips, version, and the
OpenAPI path set. It runs in CI against the Rust binary.

Rollback: delete the tests. Install check: the running daemons are
untouched.

This suite is the contract a replacement has to pass. Later PRs point
the same tests at the Effect binary without deleting the Rust run.

### PR 2. Effect package, one composition root

`services/daemon`. One `main` that launches one Layer. Config, paths,
clock, and HTTP are Layers. No capture, no install change. `bun test`
boots it on an ephemeral port and checks `/health`. Pin `effect` and
`@effect/platform-bun`.

Rollback: delete the package. Install check: unchanged Rust install.

### PR 3. Transitions as Schema and Match

Replace the Rust work in `docs/specs/explicit-state-machines.md` PR 1.
`EnrichmentStatus` and the audio-note phase are Schema tagged unions.
One `transition` function each, exhaustive `Match`, illegal pairs return
a tagged error and write nothing. Tests cover the legal pairs in that
spec, including `Failed` from `skipped` and `Claim` from `completed`.
A property test runs the two clocks (live phase and the row) through
start, stop, confirm, fail, retry, and `Interrupted`. That test is the
CI gate. The same PR commits a TLA+ spec of that race. TLC is not a new
CI dependency. The self-hosted runners do not have it. The PR records
whether TLC was run.

No I/O. Rollback: delete the module. Install check: unchanged.

### PR 4. SQLite layer

`bun:sqlite` behind a Layer. Opens a copy of the fixtures. Verifies
checksums. Refuses a database newer than the build, the way
`ensure_current` does. Compare-and-swap updates include the expected
status. Does not add a migration and does not open a live database.

Rollback: delete the layer. Install check: unchanged.

### PR 5. Read routes

Version, status, note list, note detail, on the Effect server. The
characterization suite runs against this binary and against `audeticd`.
Responses match on the fixtures.

Rollback: stop running the suite against it. Install check: unchanged.
`make install` still starts Rust.

### PR 6. Writes, config, imports

Note start/stop/confirm/cancel/retry as the PR 3 transition plus the
compare-and-swap from PR 4. Enrichment claim. TOML decode that preserves
unknown keys. Import and the Plaud poll as Effects. Ingress bearer auth
on 3739.

Rollback: revert. Install check: unchanged.

### PR 7. Provider and agent Layers

Remote transcription providers and `JobsClient` are Layers. Swapping a
provider is providing a different Layer at the root. The agent runner is
a Layer over a subprocess, with the same timeout and the same
`enrichment_error` text the characterization fixture round-trips. The
builtin Codex args stay `exec --skip-git-repo-check --sandbox read-only -`
with no model flag (`db/agent_profiles.rs`).

Rollback: revert. Install check: unchanged.

### PR 8. Native module, stub

The cdylib exports `health`. The Effect process loads it by absolute
path beside the binary. Linux CI builds the `.so` and dlopens it. The
macOS runner builds the dylib. No microphone, no screen recording.

Rollback: revert. Install check: unchanged. The module is not copied
into `Audetic.app` yet.

### PR 9. Mac TCC spike

On the MacBook only. A side bundle id, `ai.audetic.daemon.spike`, not
`ai.audetic.daemon`, not the LaunchAgent. Ad-hoc sign with
`com.apple.security.device.audio-input` plus Bun's five JIT keys. Confirm
the microphone prompt and the screen-recording prompt, then quit. Rebuild
once and record whether the grant survives. It should not: ad-hoc
identity is the cdhash, which is already true of the Rust app. Delete
the spike bundle. The real app and its grants stay.

This PR has no CI assertion of the prompt. The CI half is that the
signed-entitlements plist parses and the spike target is not referenced
by `make install`. The machine half is the manual check above.

Rollback: delete the spike bundle. Install check: `ai.audetic.daemon` is
still the Rust binary, `/api/version` unchanged.

If the prompts do not appear, stop the cutover PRs and keep the Rust
binary as the launchd process. PRs 1 through 8 stay valid.

### PR 10. Capture and inference behind the module

Move the cpal callback, the system tap, the device watcher, and
`transcribe_blocking` into the cdylib. Effect keeps the phase machine
and calls the four operations from Part A. CI uses a fake Layer. Device
tests stay manual, matching today's `CI` gate. Model files stay at
`<data_dir>/models/<id>`.

Rollback: revert. Install check: unchanged. The installed daemon is
still Rust.

### PR 11. Linux cutover

`make install` on Linux copies the compiled Bun binary and the sibling
native module, and writes the same unit. The characterization suite
passes against a copy of a migration-2 database and a migration-3
database before the Beelink's real file is touched. Backup first
(`VACUUM INTO`). Then install on the Beelink and confirm the existing
notes still list. The MacBook is still the Rust app.

Rollback: previous revision, `make install`. The database has no new
migration, so the Rust binary opens it. Install check is this upgrade.

### PR 12. macOS cutover

Only after PR 9 passed. `make macos-app` puts the Bun binary at
`Contents/MacOS/audeticd` with the menu bar still embedded, signs inside
out, and `audeticd install` replaces the LaunchAgent's bundle. Expect a
TCC re-prompt, because the cdhash changed. Confirm microphone, screen
recording, a short capture, and that the 243 notes are still there.
The Beelink is already on the Bun binary and stays up.

Rollback: previous revision, `make install`. Same database rule as
PR 11.

### PR 13. CLI

`audetic` becomes an Effect HTTP client. Same subcommands, same JSON.
It can ship any time after PR 5, because it talks to whichever daemon is
on 3737. It is last so the cutover is not mixed with a second binary.

Rollback: the Rust `audetic` binary from the parent revision. Install
check: `audetic notes` against the installed daemon on each machine.

### Foldkit

Foldkit is pre-1.0. npm `foldkit` is 0.171.0 (18 November 2026): a
TypeScript UI framework on Effect, Elm-shaped. Its own site says minor
releases can still change the public API, and 1.0 is the stability
promise. That argues for sequencing, not for waiting to start, and not
for leading with the rewrite.

Do not port `apps/web-ui` before PR 3. The first Foldkit PR is one route,
the notes list, on a pinned version. That route's MobX store goes away
with it. The rest of the SPA stays React, Vite, and MobX
(`apps/web-ui/NOTES.md`) until its own PR. The daemon cutover does not
wait on Foldkit 1.0. The menu bar is not a Foldkit app.

The pre-1.0 status is a pin and a one-route slice. It is not an argument
for a second UI convention forever, and it is not an argument against
Mat's choice of Foldkit.

### Is the Rust enrichment transition worth doing first?

No. `docs/specs/explicit-state-machines.md` PR 1 would land
`EnrichmentStatus::transition` in the Rust daemon. This plan's PR 3 is
that function as Schema and `Match`, tested on the same pairs, and PR 1
of this plan locks the behavior from the outside. Writing the Rust
function first spends the agent loop on code the cutover deletes.

If a product fix has to ship before PR 3 exists, patch the Rust writer
in the style that is already there. Do not start the eight-PR Rust
state-machine program.

### Conventions

Layers for providers, the database, the clock, the agent runner, and the
native module. A provider swap is a different Layer at the one root.

Schema at every boundary: HTTP bodies, the native-module payloads, TOML,
and the rows the repository returns.

Tagged errors. `Match` handles them. A stringly `anyhow` error stops at
the edge of the Rust module, where it becomes a tagged error.

State machines are Schema tagged unions and one `transition` function,
exhaustive `Match`. The illegal pair writes nothing.

The capture and transcription races (the two clocks, the startup sweep,
the enrichment claim, stream death during stop) have the property test
from PR 3 and the TLA+ spec committed beside it.

### Alchemy readiness

Alchemy is later. The published package `alchemy` is `2.0.0-beta.87`,
described as infrastructure as Effects. Nothing in this migration
imports it.

There is one composition root, the daemon `main` from PR 2. Install and
serve are two entry points of that program. They share the path and
config Layers. Tests provide overrides at that root. There is not a
second root for deploy.

Layers that are resource-shaped, and that a later Alchemy program could
own:

- the LaunchAgent plist and the systemd user unit
- the app-bundle copy and the codesign invocation
- the CLI placed on `PATH`
- creation of the config directory and the data directory

Layers that stay local to the process:

- the SQLite connection and the live database file (Alchemy does not
  migrate or replace `audetic.db`)
- the HTTP listeners
- capture, the device watcher, and the inference engine
- the TCC grant watcher
- clipboard and injection
- agent subprocesses
- the Plaud poller
- the menu bar, which is a separate Swift process

### Observed defect

On the MacBook, 69 notes are in enrichment error because the agent
command used the model `gpt-6-astra`. Codex with a ChatGPT account does
not support that model.

The string is not in this repository. The builtin Codex profile runs
`codex exec --skip-git-repo-check --sandbox read-only -` and does not
pass `--model` (`db/agent_profiles.rs`). Enrichment shells out through
`run_agent` (`agents/mod.rs`) and stores the failure on the row
(`note_intelligence/mod.rs`, `set_enrichment_state` to `error`). The
model name came from the MacBook's profile args or from Codex's own
config, then got written into `enrichment_error`.

This is an operator fix on the current daemon: point Codex at a model
that account can run, then retry those notes. The migration must not
delete the rows or the error text. The characterization fixture includes
an `error` row for that reason. It is not a blocker for the Effect work,
and it does not belong inside a cutover PR.
