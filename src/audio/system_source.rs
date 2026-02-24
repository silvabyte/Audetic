//! System audio capture (what others say on Zoom/Meet/etc.).
//!
//! Captures audio from PipeWire monitor sources by spawning `pw-cat --record`
//! and reading raw f32 PCM samples from its stdout.

use anyhow::Result;
use std::io::Read as _;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use tracing::{debug, info, warn};
use which::which;

use super::audio_source::AudioSource;

pub struct SystemAudioSource {
    child: Option<Child>,
    reader_thread: Option<std::thread::JoinHandle<()>>,
    samples: Arc<Mutex<Vec<f32>>>,
    active: bool,
    target_sample_rate: u32,
}

impl SystemAudioSource {
    pub fn new(sample_rate: u32) -> Self {
        Self {
            child: None,
            reader_thread: None,
            samples: Arc::new(Mutex::new(Vec::new())),
            active: false,
            target_sample_rate: sample_rate,
        }
    }

    /// Get the default PipeWire monitor source name via pactl.
    fn get_monitor_source() -> Option<String> {
        let output = Command::new("pactl")
            .args(["get-default-sink"])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .output()
            .ok()?;

        if !output.status.success() {
            return None;
        }

        let sink = String::from_utf8(output.stdout).ok()?.trim().to_string();
        if sink.is_empty() {
            return None;
        }

        Some(format!("{}.monitor", sink))
    }
}

impl AudioSource for SystemAudioSource {
    fn start(&mut self) -> Result<()> {
        if self.active {
            return Err(anyhow::anyhow!("System audio source already recording"));
        }

        // Clear previous samples
        {
            let mut samples = self.samples.lock().unwrap();
            samples.clear();
            samples.shrink_to_fit();
        }

        // Check pw-cat is available
        if which("pw-cat").is_err() {
            warn!(
                "pw-cat not found. Meeting will record mic only. \
                 Install PipeWire to capture system audio."
            );
            self.active = true;
            return Ok(());
        }

        // Get monitor source name
        let monitor = match Self::get_monitor_source() {
            Some(m) => {
                info!("Using PipeWire monitor source: {}", m);
                m
            }
            None => {
                warn!(
                    "Could not determine default audio sink. \
                     Meeting will record mic only."
                );
                self.active = true;
                return Ok(());
            }
        };

        // Spawn pw-cat to capture system audio
        let child = match Command::new("pw-cat")
            .args([
                "--record",
                "--target",
                &monitor,
                "--rate",
                &self.target_sample_rate.to_string(),
                "--channels",
                "1",
                "--format",
                "f32",
                "-", // write to stdout
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(c) => c,
            Err(e) => {
                warn!("Failed to spawn pw-cat: {}. Meeting will record mic only.", e);
                self.active = true;
                return Ok(());
            }
        };

        let mut child = child;
        let stdout = match child.stdout.take() {
            Some(s) => s,
            None => {
                warn!("Failed to capture pw-cat stdout. Meeting will record mic only.");
                let _ = child.kill();
                self.active = true;
                return Ok(());
            }
        };

        // Spawn reader thread to consume stdout
        let samples_clone = self.samples.clone();
        let reader_thread = std::thread::spawn(move || {
            Self::read_samples(stdout, samples_clone);
        });

        self.child = Some(child);
        self.reader_thread = Some(reader_thread);
        self.active = true;
        info!("System audio capture started via pw-cat");
        Ok(())
    }

    fn stop(&mut self) -> Result<Vec<f32>> {
        if !self.active {
            return Err(anyhow::anyhow!("System audio source not recording"));
        }

        // Kill the pw-cat process
        if let Some(mut child) = self.child.take() {
            debug!("Killing pw-cat process");
            let _ = child.kill();
            let _ = child.wait();
        }

        // Wait for reader thread to finish (it exits on EOF after kill)
        if let Some(thread) = self.reader_thread.take() {
            let _ = thread.join();
        }

        self.active = false;

        let samples = {
            let mut guard = self.samples.lock().unwrap();
            let s = guard.clone();
            guard.clear();
            guard.shrink_to_fit();
            s
        };

        info!(
            "System audio stopped, {} samples captured",
            samples.len()
        );
        Ok(samples)
    }

    fn is_active(&self) -> bool {
        self.active
    }

    fn sample_rate(&self) -> u32 {
        self.target_sample_rate
    }
}

impl SystemAudioSource {
    /// Read f32 samples from pw-cat stdout into the shared buffer.
    ///
    /// pw-cat writes a 24-byte AU header followed by raw f32 LE PCM.
    /// We detect the AU magic to skip the header; if absent, treat
    /// the entire stream as raw f32 data.
    fn read_samples(
        mut stdout: std::process::ChildStdout,
        samples: Arc<Mutex<Vec<f32>>>,
    ) {
        // Try to read AU header magic (4 bytes: 0x2e736e64 big-endian, aka ".snd")
        let mut magic = [0u8; 4];
        if stdout.read_exact(&mut magic).is_err() {
            warn!("pw-cat stdout closed before any data");
            return;
        }

        // AU magic is ".snd" = 0x2e736e64 in big-endian
        let is_au = magic == [0x2e, 0x73, 0x6e, 0x64];

        if is_au {
            // Read data offset (bytes 4-8, big-endian u32)
            let mut offset_bytes = [0u8; 4];
            if stdout.read_exact(&mut offset_bytes).is_err() {
                warn!("pw-cat stdout closed reading AU header offset");
                return;
            }
            let data_offset = u32::from_be_bytes(offset_bytes) as usize;

            // Skip remaining header bytes (we already read 8)
            if data_offset > 8 {
                let remaining = data_offset - 8;
                let mut skip = vec![0u8; remaining];
                if stdout.read_exact(&mut skip).is_err() {
                    warn!("pw-cat stdout closed skipping AU header");
                    return;
                }
            }
            debug!("Skipped AU header ({} bytes)", data_offset);
        } else {
            // No AU header — the 4 bytes we read are the start of a sample
            let sample = f32::from_le_bytes(magic);
            if let Ok(mut guard) = samples.lock() {
                guard.push(sample);
            }
        }

        // Read f32 LE samples in chunks
        let mut buf = [0u8; 4096];
        loop {
            match stdout.read(&mut buf) {
                Ok(0) => break, // EOF
                Ok(n) => {
                    // Process complete f32 samples (4 bytes each)
                    let complete = n - (n % 4);
                    if complete > 0 {
                        let mut new_samples = Vec::with_capacity(complete / 4);
                        for chunk in buf[..complete].chunks_exact(4) {
                            new_samples.push(f32::from_le_bytes([
                                chunk[0], chunk[1], chunk[2], chunk[3],
                            ]));
                        }
                        if let Ok(mut guard) = samples.lock() {
                            guard.extend_from_slice(&new_samples);
                        }
                    }
                    // Note: trailing bytes (n % 4 != 0) are discarded.
                    // This is fine — pw-cat writes complete samples.
                }
                Err(e) => {
                    debug!("pw-cat stdout read error (expected on kill): {}", e);
                    break;
                }
            }
        }
    }
}

impl Drop for SystemAudioSource {
    fn drop(&mut self) {
        if self.active {
            debug!("Dropping active SystemAudioSource, cleaning up");
            let _ = self.stop();
        }
    }
}
