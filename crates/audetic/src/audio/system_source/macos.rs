//! macOS System Tap adapter backed by cpal's Default Output loopback.

use anyhow::{anyhow, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::SampleFormat;
use tracing::{error, info};

use std::sync::Arc;

use crate::audio::audio_source::{AudioSource, MeetingSystemSource};
use crate::audio::capture_recovery::CaptureRecovery;
use crate::audio::mic_source::{MonotonicClock, SystemMonotonicClock};
use crate::audio::stream_event::{StreamDeath, StreamEventSink};
use crate::audio::system_tap::{
    ActiveSystemTap, SystemTapAudioSource, SystemTapBackend, SystemTapDataCallback,
    SystemTapErrorCallback,
};

struct CpalSystemTapBackend;

impl SystemTapBackend for CpalSystemTapBackend {
    fn start_default_output(
        &self,
        mut on_data: SystemTapDataCallback,
        mut on_error: SystemTapErrorCallback,
    ) -> Result<ActiveSystemTap> {
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or_else(|| anyhow!("No Default Output available for System Tap"))?;
        let device_name = device
            .description()
            .map(|description| description.name().to_string())
            .unwrap_or_else(|_| "<unknown>".to_string());
        let supported = device
            .default_output_config()
            .with_context(|| format!("Failed to read Default Output config for {device_name}"))?;
        if supported.sample_format() != SampleFormat::F32 {
            return Err(anyhow!(
                "Default Output uses {:?} samples; System Tap requires f32",
                supported.sample_format()
            ));
        }

        let channels = supported.channels() as usize;
        let native_sample_rate = supported.sample_rate();
        let config: cpal::StreamConfig = supported.into();
        info!(
            "System Tap using Default Output: {} ({} ch, {} Hz)",
            device_name, channels, native_sample_rate
        );
        let stream = device
            .build_input_stream(
                &config,
                move |data: &[f32], _: &cpal::InputCallbackInfo| on_data(data, channels),
                move |err| {
                    error!("System Tap stream error: {err}");
                    on_error();
                },
                None,
            )
            .context("Failed to build System Tap on Default Output")?;
        stream.play().context("Failed to start System Tap")?;

        Ok(ActiveSystemTap::new(native_sample_rate, stream))
    }
}

pub struct SystemAudioSource {
    inner: SystemTapAudioSource,
}

impl SystemAudioSource {
    pub fn new(sample_rate: u32) -> Self {
        Self::with_event_sink(sample_rate, Arc::new(|_| {}))
    }

    pub(crate) fn with_event_sink(sample_rate: u32, stream_event_sink: StreamEventSink) -> Self {
        Self::with_backend_and_clock(
            sample_rate,
            Box::new(CpalSystemTapBackend),
            stream_event_sink,
            Arc::new(SystemMonotonicClock::default()),
        )
    }

    fn with_backend_and_clock(
        sample_rate: u32,
        backend: Box<dyn SystemTapBackend>,
        stream_event_sink: StreamEventSink,
        clock: Arc<dyn MonotonicClock>,
    ) -> Self {
        Self {
            inner: SystemTapAudioSource::with_backend_and_clock(
                sample_rate,
                backend,
                stream_event_sink,
                clock,
            ),
        }
    }
}

impl AudioSource for SystemAudioSource {
    fn start(&mut self) -> Result<()> {
        self.inner.start()
    }

    fn stop(&mut self) -> Result<Vec<f32>> {
        self.inner.stop()
    }

    fn is_active(&self) -> bool {
        self.inner.is_active()
    }

    fn has_live_stream(&self) -> bool {
        self.inner.has_live_stream()
    }

    fn sample_rate(&self) -> u32 {
        self.inner.sample_rate()
    }
}

#[async_trait::async_trait(?Send)]
impl MeetingSystemSource for SystemAudioSource {
    fn supports_hot_swap(&self) -> bool {
        self.inner.supports_hot_swap()
    }

    fn mark_meeting_started(&mut self) {
        self.inner.mark_meeting_started();
    }

    fn has_captured_audio(&self) -> bool {
        self.inner.has_captured_audio()
    }

    async fn default_output_switched(&mut self) -> Result<CaptureRecovery> {
        self.inner.default_output_switched().await
    }

    async fn stream_died(&mut self, death: StreamDeath) -> Result<CaptureRecovery> {
        self.inner.stream_died(death).await
    }
}
