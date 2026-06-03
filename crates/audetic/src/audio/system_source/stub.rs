//! No-op system audio source for unsupported platforms (Windows, BSD, …).
//!
//! Lets the workspace build everywhere while degrading meeting capture to
//! mic-only. Mirrors the Linux fallback path when `pw-cat` is missing.

use anyhow::Result;
use tracing::warn;

use crate::audio::audio_source::AudioSource;

pub struct SystemAudioSource {
    active: bool,
    target_sample_rate: u32,
}

impl SystemAudioSource {
    pub fn new(sample_rate: u32) -> Self {
        Self {
            active: false,
            target_sample_rate: sample_rate,
        }
    }
}

impl AudioSource for SystemAudioSource {
    fn start(&mut self) -> Result<()> {
        warn!("System audio capture is not implemented on this platform; mic only.");
        self.active = true;
        Ok(())
    }

    fn stop(&mut self) -> Result<Vec<f32>> {
        self.active = false;
        Ok(Vec::new())
    }

    fn is_active(&self) -> bool {
        self.active
    }

    fn sample_rate(&self) -> u32 {
        self.target_sample_rate
    }
}
