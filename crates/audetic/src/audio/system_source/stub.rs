//! No-op system audio source for unsupported platforms (Windows, BSD, …).
//!
//! Lets the workspace build everywhere while degrading meeting capture to
//! mic-only. Mirrors the Linux fallback path when `pw-cat` is missing.

use anyhow::Result;
use tracing::warn;

use crate::audio::audio_source::{AudioSource, MeetingSystemSource};
use crate::audio::stream_event::StreamEventSink;

pub struct SystemAudioSource {
    active: bool,
    target_sample_rate: u32,
}

#[async_trait::async_trait(?Send)]
impl MeetingSystemSource for SystemAudioSource {
    fn has_captured_audio(&self) -> bool {
        false
    }
}

impl SystemAudioSource {
    pub fn new(sample_rate: u32) -> Self {
        Self {
            active: false,
            target_sample_rate: sample_rate,
        }
    }

    pub(crate) fn with_event_sink(sample_rate: u32, _stream_event_sink: StreamEventSink) -> Self {
        Self::new(sample_rate)
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

    fn has_live_stream(&self) -> bool {
        false
    }

    fn sample_rate(&self) -> u32 {
        self.target_sample_rate
    }
}
