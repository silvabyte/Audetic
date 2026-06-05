//! macOS native global-hotkey controller.
//!
//! The daemon registers a *system-wide* hotkey (default `⌘R`) that toggles
//! dictation by POSTing the same `/api/toggle` endpoint the Linux Hyprland
//! binding hits — so the toggle path is identical and fully decoupled from the
//! hotkey machinery.
//!
//! Carbon's `RegisterEventHotKey` (which `global-hotkey` uses on macOS)
//! delivers events through a `CFRunLoop` that must be pumped on the **main
//! thread**. The daemon's main thread is otherwise occupied by the Tokio
//! runtime, so `main()` flips it: the async service runs on a worker thread and
//! the main thread runs [`run_main_loop`]. The keybind API mutates the live
//! registration through [`request`] (a channel into the loop), so changing the
//! hotkey in the UI takes effect without restarting the daemon.

use std::sync::mpsc::{self, Sender};
use std::sync::OnceLock;

use anyhow::{anyhow, Result};
use global_hotkey::hotkey::{Code, HotKey, Modifiers};
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
use tokio::runtime::Handle;
use tracing::{debug, error, info, warn};

/// Built-in default chord, used when the user hasn't chosen one.
pub const DEFAULT_HOTKEY: &str = "CMD+R";

/// A parsed chord: the registrable hotkey plus a human-readable label (`⌘R`).
pub struct ParsedHotkey {
    pub hotkey: HotKey,
    pub display: String,
}

/// Commands the keybind API sends to the running hotkey loop.
pub enum HotkeyCommand {
    /// Register `hotkey`, replacing any currently-registered one.
    Register(HotKey),
    /// Unregister the current hotkey (disable).
    Unregister,
}

/// Sender into the live [`run_main_loop`]. Set once the loop starts.
static CONTROL_TX: OnceLock<Sender<HotkeyCommand>> = OnceLock::new();

/// Send a command to the running hotkey loop. No-op (with a warning) if the
/// loop hasn't started yet or has gone away.
pub fn request(cmd: HotkeyCommand) {
    match CONTROL_TX.get() {
        Some(tx) => {
            if tx.send(cmd).is_err() {
                warn!("hotkey loop is gone; command dropped");
            }
        }
        None => warn!("hotkey loop not started yet; command dropped"),
    }
}

/// Resolve the configured chord from config. `None` => disabled; `Some(chord)`
/// => the active chord string (the built-in default when unset).
pub fn resolve_chord(cfg: &crate::config::Config) -> Option<String> {
    match cfg.macos.hotkey.as_deref() {
        None => Some(DEFAULT_HOTKEY.to_string()),
        Some(s) if s.trim().is_empty() => None,
        Some(s) => Some(s.to_string()),
    }
}

/// Resolve the hotkey to register at startup, reading config. Returns `None`
/// when the hotkey is disabled or the configured chord is invalid.
pub fn initial_hotkey() -> Option<HotKey> {
    let cfg = match crate::config::Config::load() {
        Ok(c) => c,
        Err(e) => {
            warn!("could not load config for hotkey: {e:#}");
            return None;
        }
    };
    let chord = resolve_chord(&cfg)?;
    match parse_chord(&chord) {
        Ok(p) => Some(p.hotkey),
        Err(e) => {
            warn!("invalid macos hotkey {chord:?}: {e:#}");
            None
        }
    }
}

/// Run the global-hotkey loop on the current (main) thread, forever.
///
/// Pumps the CFRunLoop in short slices so Carbon can deliver hotkey presses,
/// then drains both the hotkey-event channel (→ toggle dictation) and the
/// control channel (→ re-register/unregister from the API).
pub fn run_main_loop(rt: Handle, initial: Option<HotKey>) -> ! {
    let manager = match GlobalHotKeyManager::new() {
        Ok(m) => m,
        Err(e) => {
            error!("failed to initialize global hotkey manager: {e}; hotkeys disabled");
            park_forever();
        }
    };

    let (tx, rx) = mpsc::channel::<HotkeyCommand>();
    if CONTROL_TX.set(tx).is_err() {
        warn!("hotkey control channel already initialized");
    }

    let mut current: Option<HotKey> = None;
    if let Some(hk) = initial {
        match manager.register(hk) {
            Ok(()) => {
                current = Some(hk);
                info!("registered global hotkey (toggles dictation)");
            }
            Err(e) => error!("failed to register global hotkey: {e}"),
        }
    } else {
        info!("global hotkey disabled");
    }

    let toggle_url = audetic_core::url::api_url(audetic_core::url::paths::TOGGLE);
    let client = reqwest::Client::new();
    let events = GlobalHotKeyEvent::receiver();

    loop {
        // Pump the run loop for a slice so Carbon delivers any pending hotkey
        // events into `events`, then poll our channels.
        pump_runloop(0.1);

        while let Ok(ev) = events.try_recv() {
            if ev.state == HotKeyState::Pressed {
                debug!("hotkey pressed; toggling dictation");
                let url = toggle_url.clone();
                let client = client.clone();
                rt.spawn(async move {
                    if let Err(e) = client.post(&url).send().await {
                        warn!("toggle POST failed: {e}");
                    }
                });
            }
        }

        while let Ok(cmd) = rx.try_recv() {
            match cmd {
                HotkeyCommand::Register(hk) => {
                    if let Some(old) = current.take() {
                        let _ = manager.unregister(old);
                    }
                    match manager.register(hk) {
                        Ok(()) => {
                            current = Some(hk);
                            info!("re-registered global hotkey");
                        }
                        Err(e) => error!("failed to register hotkey: {e}"),
                    }
                }
                HotkeyCommand::Unregister => {
                    if let Some(old) = current.take() {
                        match manager.unregister(old) {
                            Ok(()) => info!("global hotkey disabled"),
                            Err(e) => error!("failed to unregister hotkey: {e}"),
                        }
                    }
                }
            }
        }
    }
}

/// Pump the main thread's CFRunLoop for `seconds`, letting Carbon dispatch
/// hotkey events. `returnAfterSourceHandled = false` so it runs the full slice.
fn pump_runloop(seconds: f64) {
    use core_foundation_sys::runloop::{kCFRunLoopDefaultMode, CFRunLoopRunInMode};
    // SAFETY: `kCFRunLoopDefaultMode` is a Core Foundation constant string and
    // `CFRunLoopRunInMode` operates on the current thread's run loop. We only
    // ever call this from the main thread (see `run_main_loop`).
    unsafe {
        CFRunLoopRunInMode(kCFRunLoopDefaultMode, seconds, 0);
    }
}

/// Park the current thread forever (used when the hotkey manager can't start).
fn park_forever() -> ! {
    loop {
        std::thread::park();
    }
}

/// Parse a chord string (`"CMD+R"`, `"CMD SHIFT R"`, `"⌘⇧R"` is *not* accepted —
/// use token form) into a registrable hotkey plus a `⌘`-style display label.
///
/// Accepted modifiers: `CMD`/`COMMAND`/`SUPER`/`META`/`WIN` (→ ⌘), `SHIFT`,
/// `CTRL`/`CONTROL`, `ALT`/`OPTION`/`OPT`. At least one modifier is required so
/// a bare key can't be grabbed system-wide.
pub fn parse_chord(s: &str) -> Result<ParsedHotkey> {
    let normalized = s.replace(['+', ',', '-'], " ");
    let parts: Vec<&str> = normalized.split_whitespace().collect();
    if parts.is_empty() {
        return Err(anyhow!("empty hotkey"));
    }

    let key_tok = *parts.last().unwrap();
    let mod_toks = &parts[..parts.len() - 1];
    if mod_toks.is_empty() {
        return Err(anyhow!(
            "hotkey '{s}' needs at least one modifier (e.g. CMD+R)"
        ));
    }

    let mut mods = Modifiers::empty();
    for m in mod_toks {
        match m.to_uppercase().as_str() {
            "CMD" | "COMMAND" | "SUPER" | "META" | "WIN" => mods |= Modifiers::META,
            "SHIFT" => mods |= Modifiers::SHIFT,
            "CTRL" | "CONTROL" => mods |= Modifiers::CONTROL,
            "ALT" | "OPTION" | "OPT" => mods |= Modifiers::ALT,
            other => return Err(anyhow!("unknown modifier '{other}' in '{s}'")),
        }
    }

    let code = key_to_code(key_tok)?;
    let hotkey = HotKey::new(Some(mods), code);
    let display = format!("{}{}", mods_display(mods), key_display(key_tok));
    Ok(ParsedHotkey { hotkey, display })
}

fn key_to_code(k: &str) -> Result<Code> {
    let up = k.to_uppercase();
    if up.len() == 1 {
        let ch = up.chars().next().unwrap();
        if ch.is_ascii_alphabetic() {
            return Ok(letter_code(ch));
        }
        if ch.is_ascii_digit() {
            return Ok(digit_code(ch));
        }
    }
    match up.as_str() {
        "SPACE" => Ok(Code::Space),
        "ENTER" | "RETURN" => Ok(Code::Enter),
        "TAB" => Ok(Code::Tab),
        "ESC" | "ESCAPE" => Ok(Code::Escape),
        _ => Err(anyhow!("unsupported key '{k}'")),
    }
}

fn letter_code(ch: char) -> Code {
    match ch {
        'A' => Code::KeyA,
        'B' => Code::KeyB,
        'C' => Code::KeyC,
        'D' => Code::KeyD,
        'E' => Code::KeyE,
        'F' => Code::KeyF,
        'G' => Code::KeyG,
        'H' => Code::KeyH,
        'I' => Code::KeyI,
        'J' => Code::KeyJ,
        'K' => Code::KeyK,
        'L' => Code::KeyL,
        'M' => Code::KeyM,
        'N' => Code::KeyN,
        'O' => Code::KeyO,
        'P' => Code::KeyP,
        'Q' => Code::KeyQ,
        'R' => Code::KeyR,
        'S' => Code::KeyS,
        'T' => Code::KeyT,
        'U' => Code::KeyU,
        'V' => Code::KeyV,
        'W' => Code::KeyW,
        'X' => Code::KeyX,
        'Y' => Code::KeyY,
        // Only A-Z reach here (callers gate on `is_ascii_alphabetic`).
        _ => Code::KeyZ,
    }
}

fn digit_code(ch: char) -> Code {
    match ch {
        '0' => Code::Digit0,
        '1' => Code::Digit1,
        '2' => Code::Digit2,
        '3' => Code::Digit3,
        '4' => Code::Digit4,
        '5' => Code::Digit5,
        '6' => Code::Digit6,
        '7' => Code::Digit7,
        '8' => Code::Digit8,
        // Only 0-9 reach here (callers gate on `is_ascii_digit`).
        _ => Code::Digit9,
    }
}

/// macOS modifier glyphs in the conventional ⌃⌥⇧⌘ order.
fn mods_display(m: Modifiers) -> String {
    let mut s = String::new();
    if m.contains(Modifiers::CONTROL) {
        s.push('⌃');
    }
    if m.contains(Modifiers::ALT) {
        s.push('⌥');
    }
    if m.contains(Modifiers::SHIFT) {
        s.push('⇧');
    }
    if m.contains(Modifiers::META) {
        s.push('⌘');
    }
    s
}

fn key_display(k: &str) -> String {
    let up = k.to_uppercase();
    match up.as_str() {
        "SPACE" => "Space".to_string(),
        "ENTER" | "RETURN" => "Return".to_string(),
        "ESC" | "ESCAPE" => "Esc".to_string(),
        "TAB" => "Tab".to_string(),
        _ => up,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_default() {
        let p = parse_chord(DEFAULT_HOTKEY).unwrap();
        assert_eq!(p.display, "⌘R");
    }

    #[test]
    fn parses_multi_modifier() {
        assert_eq!(parse_chord("CMD+SHIFT+R").unwrap().display, "⇧⌘R");
        assert_eq!(parse_chord("CTRL ALT CMD R").unwrap().display, "⌃⌥⌘R");
    }

    #[test]
    fn rejects_bare_key_and_unknown() {
        assert!(parse_chord("R").is_err());
        assert!(parse_chord("CMD+€").is_err());
        assert!(parse_chord("HYPER+R").is_err());
    }
}
