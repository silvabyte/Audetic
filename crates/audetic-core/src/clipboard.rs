//! System clipboard helpers shared by the daemon and the CLI.
//!
//! The daemon's text-injection path (`text_io`) reuses [`CLIPBOARD_BACKENDS`]
//! for its async clipboard fallback; the CLI uses [`copy_to_clipboard_sync`]
//! for `transcribe --copy` / `history --copy`. Keeping the backend table in
//! one place avoids the two diverging.

use anyhow::{anyhow, Result};
use std::io::Write;
use std::process::{Command, Stdio};
use which::which;

/// A system clipboard tool and how to feed it text.
pub struct ClipboardBackend {
    pub name: &'static str,
    pub copy_cmd: &'static str,
    pub copy_args: &'static [&'static str],
    pub use_stdin: bool,
}

/// Clipboard tools tried in order: wl-copy (Wayland) first, then xclip/xsel (X11).
pub const CLIPBOARD_BACKENDS: &[ClipboardBackend] = &[
    ClipboardBackend {
        name: "wl-copy",
        copy_cmd: "wl-copy",
        copy_args: &[],
        use_stdin: true,
    },
    ClipboardBackend {
        name: "xclip",
        copy_cmd: "xclip",
        copy_args: &["-selection", "clipboard"],
        use_stdin: true,
    },
    ClipboardBackend {
        name: "xsel",
        copy_cmd: "xsel",
        copy_args: &["--clipboard", "--input"],
        use_stdin: true,
    },
];

/// Copy text to clipboard using system clipboard tools (synchronous version).
///
/// Uses wl-copy (Wayland), xclip, or xsel (X11) for persistent clipboard
/// storage that survives after the process exits.
///
/// This is a standalone function for use in synchronous contexts (e.g., CLI commands).
pub fn copy_to_clipboard_sync(text: &str) -> Result<()> {
    if text.is_empty() {
        return Ok(());
    }

    for backend in CLIPBOARD_BACKENDS {
        if which(backend.copy_cmd).is_err() {
            continue;
        }

        let mut child = match Command::new(backend.copy_cmd)
            .args(backend.copy_args)
            .stdin(Stdio::piped())
            .spawn()
        {
            Ok(child) => child,
            Err(_) => continue,
        };

        if let Some(stdin) = child.stdin.as_mut() {
            if stdin.write_all(text.as_bytes()).is_err() {
                continue;
            }
        }

        if let Ok(status) = child.wait() {
            if status.success() {
                return Ok(());
            }
        }
    }

    Err(anyhow!(
        "No clipboard tool available. Please install wl-copy (Wayland), xclip, or xsel (X11)."
    ))
}
