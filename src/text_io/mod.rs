use anyhow::{anyhow, Context, Result};
use arboard::Clipboard;
use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{debug, info, warn};
use which::which;

#[derive(Clone)]
pub struct TextIoService {
    inner: Arc<TextIoInner>,
}

struct TextIoInner {
    clipboard: Mutex<Option<Clipboard>>,
    preserve_previous: bool,
    injection_method: InjectionMethod,
}

impl TextIoService {
    pub fn new(preferred_method: Option<&str>, preserve_previous: bool) -> Result<Self> {
        let clipboard = match Clipboard::new() {
            Ok(cb) => Some(cb),
            Err(err) => {
                warn!(
                    "System clipboard backend unavailable ({}); falling back to CLI-only mode",
                    err
                );
                None
            }
        };
        let injection_method = InjectionMethod::detect(preferred_method);

        Ok(Self {
            inner: Arc::new(TextIoInner {
                clipboard: Mutex::new(clipboard),
                preserve_previous,
                injection_method,
            }),
        })
    }

    pub fn injection_method(&self) -> InjectionMethod {
        self.inner.injection_method
    }

    pub async fn copy_to_clipboard(&self, text: &str) -> Result<()> {
        if text.is_empty() {
            return Ok(());
        }

        info!("Copying {} chars to clipboard", text.len());
        debug!("Text to copy: {}", text);

        let preserve_previous = self.inner.preserve_previous;
        let mut previous: Option<String> = None;
        let mut used_native = false;

        {
            let mut clipboard_guard = self.inner.clipboard.lock().await;
            if let Some(clipboard) = clipboard_guard.as_mut() {
                if preserve_previous {
                    previous = clipboard.get_text().ok();
                }

                match clipboard.set_text(text) {
                    Ok(_) => {
                        used_native = true;
                    }
                    Err(err) => {
                        warn!(
                            "Primary clipboard backend failed ({}), disabling until restart",
                            err
                        );
                        *clipboard_guard = None;
                    }
                }
            } else {
                debug!("Native clipboard backend unavailable; using system clipboard tools");
            }
        }

        if !used_native {
            self.copy_with_system_backends(text).await?;
        }

        if let Some(prev) = previous {
            debug!("Previous clipboard content preserved: {} chars", prev.len());
        }

        Ok(())
    }

    pub async fn inject_text(&self, text: &str) -> Result<()> {
        if text.is_empty() {
            return Ok(());
        }

        info!("Injecting text: {} chars", text.len());
        debug!("Text to inject: {}", text);

        match self.inner.injection_method {
            InjectionMethod::Wtype => {
                self.try_with_clipboard_fallback(text, Self::inject_with_wtype)
                    .await
            }
            InjectionMethod::Ydotool => {
                self.try_with_clipboard_fallback(text, Self::inject_with_ydotool)
                    .await
            }
            InjectionMethod::Clipboard => self.simulate_paste().await,
        }
    }

    pub async fn paste_from_clipboard(&self) -> Result<()> {
        self.simulate_paste().await
    }

    async fn try_with_clipboard_fallback<F>(&self, text: &str, inject_fn: F) -> Result<()>
    where
        F: Fn(&str) -> Result<()>,
    {
        if let Err(err) = inject_fn(text) {
            warn!(
                "Direct text injection failed with {} – falling back to clipboard paste",
                err
            );
            self.copy_to_clipboard(text).await?;
            self.simulate_paste().await
        } else {
            Ok(())
        }
    }

    async fn copy_with_system_backends(&self, text: &str) -> Result<()> {
        for backend in CLIPBOARD_BACKENDS {
            if which(backend.copy_cmd).is_err() {
                continue;
            }

            let mut cmd = Command::new(backend.copy_cmd);
            cmd.args(backend.copy_args);

            if backend.use_stdin {
                cmd.stdin(Stdio::piped());
            }

            if let Ok(mut child) = cmd.spawn() {
                if backend.use_stdin {
                    if let Some(stdin) = child.stdin.as_mut() {
                        if stdin.write_all(text.as_bytes()).is_err() {
                            continue;
                        }
                    }
                }

                if let Ok(status) = child.wait() {
                    if status.success() {
                        debug!("Text copied to clipboard with {}", backend.name);
                        return Ok(());
                    }
                }
            }
        }

        Err(anyhow!(
            "No clipboard tool (wl-copy/xclip/xsel) available for fallback"
        ))
    }

    fn inject_with_wtype(text: &str) -> Result<()> {
        let output = Command::new("wtype")
            .arg(text)
            .output()
            .context("Failed to execute wtype")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow!("wtype failed: {}", stderr));
        }

        Ok(())
    }

    fn inject_with_ydotool(text: &str) -> Result<()> {
        let output = Command::new("ydotool")
            .arg("type")
            .arg(text)
            .output()
            .context("Failed to execute ydotool")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            warn!("ydotool failed: {}", stderr);
            return Err(anyhow!(
                "ydotool failed: {}. Make sure ydotoold is running",
                stderr
            ));
        }

        Ok(())
    }

    async fn simulate_paste(&self) -> Result<()> {
        info!("Simulating paste from clipboard");

        if which("ydotool").is_ok() {
            if let Ok(output) = Command::new("ydotool")
                .args(["key", "29:1", "47:1", "47:0", "29:0"])
                .output()
            {
                if output.status.success() {
                    debug!("Successfully pasted with ydotool");
                    return Ok(());
                }
            }
        }

        if which("wtype").is_ok() {
            if let Ok(output) = Command::new("wtype")
                .args(["-M", "ctrl", "-P", "v", "-m", "ctrl", "-p", "v"])
                .output()
            {
                if output.status.success() {
                    debug!("Successfully pasted with wtype");
                    return Ok(());
                } else {
                    debug!("wtype paste failed, trying other methods");
                }
            }
        }

        if which("xdotool").is_ok() {
            if let Ok(output) = Command::new("xdotool").args(["key", "ctrl+v"]).output() {
                if output.status.success() {
                    debug!("Successfully pasted with xdotool");
                    return Ok(());
                }
            }
        }

        if let Ok(desktop) = std::env::var("XDG_CURRENT_DESKTOP") {
            if desktop == "KDE" {
                if let Ok(output) = Command::new("qdbus")
                    .args([
                        "org.kde.klipper",
                        "/klipper",
                        "org.kde.klipper.klipper.invokeAction",
                        "paste",
                    ])
                    .output()
                {
                    if output.status.success() {
                        debug!("Successfully pasted with KDE Klipper");
                        return Ok(());
                    }
                }
            }
        }

        warn!("All paste methods failed - text remains in clipboard for manual paste");
        Ok(())
    }
}

#[derive(Debug, Clone, Copy)]
pub enum InjectionMethod {
    Wtype,
    Ydotool,
    Clipboard,
}

impl InjectionMethod {
    fn detect(preferred: Option<&str>) -> Self {
        if let Some(choice) = preferred {
            match choice {
                "ydotool" if which("ydotool").is_ok() => {
                    info!("Using ydotool for text injection (per config)");
                    return InjectionMethod::Ydotool;
                }
                "wtype" if which("wtype").is_ok() => {
                    info!("Using wtype for text injection (per config)");
                    return InjectionMethod::Wtype;
                }
                other => {
                    warn!(
                        "Unknown or unavailable input_method '{}', falling back to auto-detect",
                        other
                    );
                }
            }
        }

        if which("ydotool").is_ok() {
            info!("Using ydotool for text injection (auto-detected)");
            return InjectionMethod::Ydotool;
        }

        if std::env::var("WAYLAND_DISPLAY").is_ok() && which("wl-copy").is_ok() {
            info!("Using clipboard-based injection (Wayland detected)");
            return InjectionMethod::Clipboard;
        }

        if which("wtype").is_ok() {
            info!("Using wtype for text injection (auto-detected)");
            return InjectionMethod::Wtype;
        }

        info!("Falling back to clipboard-based injection");
        InjectionMethod::Clipboard
    }
}

struct ClipboardBackend {
    name: &'static str,
    copy_cmd: &'static str,
    copy_args: &'static [&'static str],
    use_stdin: bool,
}

const CLIPBOARD_BACKENDS: &[ClipboardBackend] = &[
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
