//! Microphone audio capture via cpal.
//!
//! Independent from `AudioStreamManager` — this is used exclusively by the
//! meeting recording pipeline. The existing voice-to-text pipeline uses
//! `AudioStreamManager` and is not modified.
//!
//! Each Default Input contributes one native-rate Segment. Completed Segments
//! are normalized independently and capture gaps become canonical Silence Fill
//! so the meeting microphone remains aligned with the System Tap.

use anyhow::{anyhow, Context, Result};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tracing::{debug, info, warn};

use super::audio_source::{AudioSource, MeetingMicSource};
use super::capture_recovery::{start_capture_with_retries, CaptureRecovery};
use super::input_device::{
    ActiveInput, CaptureBackend, CpalCaptureBackend, InputDataCallback, InputErrorCallback,
};
use super::resample::{push_mono_f32, resample_mono_f32};
use super::stream_event::{CaptureSource, StreamDeath, StreamEventSink, StreamGeneration};

pub(crate) trait MonotonicClock: Send + Sync {
    fn now(&self) -> Duration;
}

struct SystemMonotonicClock {
    origin: Instant,
}

impl Default for SystemMonotonicClock {
    fn default() -> Self {
        Self {
            origin: Instant::now(),
        }
    }
}

impl MonotonicClock for SystemMonotonicClock {
    fn now(&self) -> Duration {
        self.origin.elapsed()
    }
}

struct ActiveSegment {
    input: ActiveInput,
    native_samples: Arc<Mutex<Vec<f32>>>,
    generation: StreamGeneration,
}

pub struct MicAudioSource {
    backend: Box<dyn CaptureBackend>,
    active_segment: Option<ActiveSegment>,
    canonical_samples: Vec<f32>,
    stream_generation: StreamGeneration,
    live_generation: Arc<AtomicU64>,
    stream_event_sink: StreamEventSink,
    clock: Arc<dyn MonotonicClock>,
    gap_started_at: Arc<Mutex<Option<Duration>>>,
    session_active: bool,
    target_sample_rate: u32,
}

impl MicAudioSource {
    /// Create a new mic source backed by the default input device.
    ///
    /// Does **not** open the device — that's deferred to the first `start()`
    /// so constructing the meeting pipeline at boot never blocks on the mic
    /// TCC grant. Returns `Result` only to keep the call site stable;
    /// construction itself is infallible.
    ///
    /// # Arguments
    /// * `sample_rate` - Target sample rate after resampling (e.g. 16000 for
    ///   Whisper). The device may capture at a higher native rate; the
    ///   returned buffer from `stop()` is at this target rate.
    pub fn new(sample_rate: u32) -> Result<Self> {
        Ok(Self::with_backend_and_clock(
            sample_rate,
            Box::new(CpalCaptureBackend::new("Meeting microphone")),
            Arc::new(|_| {}),
            Arc::new(SystemMonotonicClock::default()),
        ))
    }

    pub(crate) fn with_event_sink(sample_rate: u32, stream_event_sink: StreamEventSink) -> Self {
        Self::with_backend_and_clock(
            sample_rate,
            Box::new(CpalCaptureBackend::new("Meeting microphone")),
            stream_event_sink,
            Arc::new(SystemMonotonicClock::default()),
        )
    }

    pub(crate) fn with_backend_and_clock(
        sample_rate: u32,
        backend: Box<dyn CaptureBackend>,
        stream_event_sink: StreamEventSink,
        clock: Arc<dyn MonotonicClock>,
    ) -> Self {
        Self {
            backend,
            active_segment: None,
            canonical_samples: Vec::new(),
            stream_generation: StreamGeneration(0),
            live_generation: Arc::new(AtomicU64::new(0)),
            stream_event_sink,
            clock,
            gap_started_at: Arc::new(Mutex::new(None)),
            session_active: false,
            target_sample_rate: sample_rate,
        }
    }

    fn start_segment(&mut self) -> Result<ActiveSegment> {
        let generation = self.stream_generation.next()?;
        let native_samples = Arc::new(Mutex::new(Vec::new()));
        let callback_samples = native_samples.clone();
        let on_data: InputDataCallback = Box::new(move |data, channels| {
            push_mono_f32(data, channels, &callback_samples);
        });

        let live_generation = self.live_generation.clone();
        let gap_started_at = self.gap_started_at.clone();
        let clock = self.clock.clone();
        let stream_event_sink = self.stream_event_sink.clone();
        let on_error: InputErrorCallback = Box::new(move || {
            if live_generation
                .compare_exchange(generation.0, 0, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
            {
                let mut gap = gap_started_at.lock().unwrap();
                gap.get_or_insert_with(|| clock.now());
                stream_event_sink(StreamDeath {
                    source: CaptureSource::MeetingMicrophone,
                    generation,
                });
            }
        });

        let input = self
            .backend
            .start_default_input(on_data, on_error)
            .context("Failed to start meeting microphone Segment")?;
        self.stream_generation = generation;
        Ok(ActiveSegment {
            input,
            native_samples,
            generation,
        })
    }

    fn install_segment(&mut self, segment: ActiveSegment) {
        self.live_generation
            .store(segment.generation.0, Ordering::SeqCst);
        self.active_segment = Some(segment);
    }

    fn begin_gap(&self) {
        let mut gap = self.gap_started_at.lock().unwrap();
        gap.get_or_insert_with(|| self.clock.now());
    }

    fn append_silence_fill(&mut self) -> Result<()> {
        let Some(started_at) = self.gap_started_at.lock().unwrap().take() else {
            return Ok(());
        };
        let elapsed = self.clock.now().saturating_sub(started_at);
        // A fractional canonical sample cannot be represented. Round each gap
        // down so Silence Fill never claims capture beyond the observed gap.
        let fill_samples = elapsed.as_nanos() * u128::from(self.target_sample_rate)
            / Duration::from_secs(1).as_nanos();
        let fill_samples = usize::try_from(fill_samples).context("Silence Fill is too long")?;
        self.canonical_samples
            .resize(self.canonical_samples.len() + fill_samples, 0.0);
        debug!(
            "Inserted {} samples of meeting microphone Silence Fill for {:?}",
            fill_samples, elapsed
        );
        Ok(())
    }

    fn close_current_segment(&mut self) -> Result<bool> {
        self.live_generation.store(0, Ordering::SeqCst);
        let Some(segment) = self.active_segment.take() else {
            return Ok(false);
        };
        let native_sample_rate = segment.input.native_sample_rate();
        drop(segment.input);
        let native = std::mem::take(&mut *segment.native_samples.lock().unwrap());
        let canonical = resample_mono_f32(&native, native_sample_rate, self.target_sample_rate)
            .context("Failed to normalize meeting microphone Segment")?;
        debug!(
            "Closed meeting microphone Segment: {} native @ {} Hz -> {} canonical @ {} Hz",
            native.len(),
            native_sample_rate,
            canonical.len(),
            self.target_sample_rate
        );
        self.canonical_samples.extend(canonical);
        Ok(true)
    }

    async fn replace_capture(&mut self, death: Option<StreamDeath>) -> Result<CaptureRecovery> {
        if !self.session_active {
            return Ok(CaptureRecovery::Ignored);
        }
        if let Some(death) = death {
            let current_generation = self
                .active_segment
                .as_ref()
                .map(|segment| segment.generation);
            if death.source != CaptureSource::MeetingMicrophone
                || current_generation != Some(death.generation)
            {
                return Ok(CaptureRecovery::Ignored);
            }
        }

        self.begin_gap();
        self.close_current_segment()
            .context("Failed to close meeting microphone Segment for replacement")?;

        match start_capture_with_retries("replacement meeting Default Input", || {
            self.start_segment()
        })
        .await
        {
            Ok(segment) => {
                self.append_silence_fill()?;
                self.install_segment(segment);
                info!("Recovered meeting microphone on the current Default Input");
                Ok(CaptureRecovery::Capturing)
            }
            Err(_) => {
                warn!("Meeting microphone remains in Degraded Capture");
                Ok(CaptureRecovery::Degraded)
            }
        }
    }
}

impl AudioSource for MicAudioSource {
    fn start(&mut self) -> Result<()> {
        if self.session_active {
            return Err(anyhow!("Mic source already recording"));
        }

        self.close_current_segment()?;
        self.canonical_samples.clear();
        *self.gap_started_at.lock().unwrap() = None;
        self.session_active = true;
        let session_started_at = self.clock.now();

        match self.start_segment() {
            Ok(segment) => {
                self.install_segment(segment);
                info!("Meeting microphone recording started");
                Ok(())
            }
            Err(error) => {
                *self.gap_started_at.lock().unwrap() = Some(session_started_at);
                Err(error).context("Failed to start meeting microphone")
            }
        }
    }

    fn stop(&mut self) -> Result<Vec<f32>> {
        if !self.session_active {
            return Err(anyhow!("Mic source not recording"));
        }
        self.session_active = false;
        self.close_current_segment()
            .context("Failed to close final meeting microphone Segment")?;
        self.append_silence_fill()?;
        let canonical = std::mem::take(&mut self.canonical_samples);
        info!(
            "Meeting microphone stopped: {} canonical samples @ {} Hz",
            canonical.len(),
            self.target_sample_rate
        );
        Ok(canonical)
    }

    fn is_active(&self) -> bool {
        self.session_active
    }

    fn sample_rate(&self) -> u32 {
        self.target_sample_rate
    }
}

#[async_trait::async_trait(?Send)]
impl MeetingMicSource for MicAudioSource {
    async fn default_input_switched(&mut self) -> Result<CaptureRecovery> {
        self.replace_capture(None).await
    }

    async fn stream_died(&mut self, death: StreamDeath) -> Result<CaptureRecovery> {
        self.replace_capture(Some(death)).await
    }

    fn has_live_stream(&self) -> bool {
        self.active_segment.is_some() && self.live_generation.load(Ordering::SeqCst) != 0
    }
}

impl Drop for MicAudioSource {
    fn drop(&mut self) {
        if self.session_active {
            debug!("Dropping active MicAudioSource, cleaning up");
            let _ = self.stop();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
    use std::time::Duration;

    use super::super::audio_mixer::AudioMixer;
    use super::super::capture_recovery::CaptureRecovery;
    use super::super::input_device::{
        ActiveInput, CaptureBackend, InputDataCallback, InputErrorCallback,
    };
    use super::*;

    #[derive(Default)]
    struct FakeClock {
        nanos: AtomicU64,
    }

    impl FakeClock {
        fn advance(&self, duration: Duration) {
            self.nanos
                .fetch_add(duration.as_nanos().try_into().unwrap(), Ordering::SeqCst);
        }
    }

    impl MonotonicClock for FakeClock {
        fn now(&self) -> Duration {
            Duration::from_nanos(self.nanos.load(Ordering::SeqCst))
        }
    }

    struct PlannedInput {
        sample_rate: u32,
        samples: Vec<f32>,
        open_duration: Duration,
    }

    struct PlannedCaptureBackend {
        inputs: Mutex<VecDeque<PlannedInput>>,
        clock: Arc<FakeClock>,
    }

    enum CapturePlan {
        Input(PlannedInput),
        Fail(&'static str),
    }

    struct RecoveryBackend {
        plans: Mutex<VecDeque<CapturePlan>>,
        errors: Arc<Mutex<Vec<InputErrorCallback>>>,
        starts: Arc<AtomicUsize>,
    }

    impl CaptureBackend for RecoveryBackend {
        fn start_default_input(
            &self,
            mut on_data: InputDataCallback,
            on_error: InputErrorCallback,
        ) -> Result<ActiveInput> {
            self.starts.fetch_add(1, Ordering::SeqCst);
            match self.plans.lock().unwrap().pop_front().unwrap() {
                CapturePlan::Input(input) => {
                    on_data(&input.samples, 1);
                    self.errors.lock().unwrap().push(on_error);
                    Ok(ActiveInput::new(input.sample_rate, ()))
                }
                CapturePlan::Fail(message) => Err(anyhow!(message)),
            }
        }
    }

    struct TokioClock {
        origin: tokio::time::Instant,
    }

    impl MonotonicClock for TokioClock {
        fn now(&self) -> Duration {
            tokio::time::Instant::now().duration_since(self.origin)
        }
    }

    impl CaptureBackend for PlannedCaptureBackend {
        fn start_default_input(
            &self,
            mut on_data: InputDataCallback,
            _on_error: InputErrorCallback,
        ) -> Result<ActiveInput> {
            let input = self.inputs.lock().unwrap().pop_front().unwrap();
            self.clock.advance(input.open_duration);
            on_data(&input.samples, 1);
            Ok(ActiveInput::new(input.sample_rate, ()))
        }
    }

    #[tokio::test]
    async fn silence_fill_keeps_recovered_mic_aligned_with_system_audio() {
        let clock = Arc::new(FakeClock::default());
        let backend = PlannedCaptureBackend {
            inputs: Mutex::new(VecDeque::from([
                PlannedInput {
                    sample_rate: 48_000,
                    samples: vec![0.25; 480],
                    open_duration: Duration::ZERO,
                },
                PlannedInput {
                    sample_rate: 44_100,
                    samples: vec![-0.25; 441],
                    open_duration: Duration::from_millis(250),
                },
            ])),
            clock: clock.clone(),
        };
        let mut mic = MicAudioSource::with_backend_and_clock(
            16_000,
            Box::new(backend),
            Arc::new(|_| {}),
            clock,
        );

        mic.start().unwrap();
        assert_eq!(
            mic.default_input_switched().await.unwrap(),
            CaptureRecovery::Capturing
        );
        let mic_track = mic.stop().unwrap();

        assert_eq!(mic_track.len(), 4_320);
        assert!(mic_track[..160].iter().all(|sample| *sample > 0.0));
        assert!(mic_track[160..4_160].iter().all(|sample| *sample == 0.0));
        assert!(mic_track[4_160..].iter().all(|sample| *sample < 0.0));

        let system_track = vec![0.1; 4_320];
        let mixed = AudioMixer::mix(&[mic_track, system_track]);
        assert_eq!(mixed.len(), 4_320);
        assert!(mixed[160..4_160]
            .iter()
            .all(|sample| (*sample - 0.1).abs() < f32::EPSILON));
    }

    #[tokio::test(start_paused = true)]
    async fn degraded_recovery_fills_retry_and_degraded_time() {
        let clock = Arc::new(TokioClock {
            origin: tokio::time::Instant::now(),
        });
        let errors = Arc::new(Mutex::new(Vec::new()));
        let starts = Arc::new(AtomicUsize::new(0));
        let events = Arc::new(Mutex::new(Vec::new()));
        let backend = RecoveryBackend {
            plans: Mutex::new(VecDeque::from([
                CapturePlan::Input(PlannedInput {
                    sample_rate: 48_000,
                    samples: vec![0.25; 480],
                    open_duration: Duration::ZERO,
                }),
                CapturePlan::Fail("replacement 1 failed"),
                CapturePlan::Fail("replacement 2 failed"),
                CapturePlan::Fail("replacement 3 failed"),
                CapturePlan::Input(PlannedInput {
                    sample_rate: 44_100,
                    samples: vec![-0.25; 441],
                    open_duration: Duration::ZERO,
                }),
            ])),
            errors: errors.clone(),
            starts: starts.clone(),
        };
        let mut mic = MicAudioSource::with_backend_and_clock(
            16_000,
            Box::new(backend),
            {
                let events = events.clone();
                Arc::new(move |event| events.lock().unwrap().push(event))
            },
            clock,
        );

        mic.start().unwrap();
        errors.lock().unwrap()[0]();
        let death = events.lock().unwrap().pop().unwrap();
        assert_eq!(
            mic.stream_died(death).await.unwrap(),
            CaptureRecovery::Degraded
        );
        assert!(
            mic.is_active(),
            "the logical meeting mic leg remains active"
        );
        assert!(!mic.has_live_stream());

        tokio::time::advance(Duration::from_millis(500)).await;
        assert_eq!(
            mic.default_input_switched().await.unwrap(),
            CaptureRecovery::Capturing
        );
        let mic_track = mic.stop().unwrap();

        // Three attempts sleep twice (2s), then capture remains degraded for
        // 500ms: floor(2.5 * 16k) is exactly 40,000 Silence Fill samples.
        assert_eq!(mic_track.len(), 40_320);
        assert!(mic_track[..160].iter().all(|sample| *sample > 0.0));
        assert!(mic_track[160..40_160].iter().all(|sample| *sample == 0.0));
        assert!(mic_track[40_160..].iter().all(|sample| *sample < 0.0));
        assert_eq!(starts.load(Ordering::SeqCst), 5);
    }

    #[tokio::test]
    async fn stale_or_other_source_death_cannot_rebuild_meeting_microphone() {
        let starts = Arc::new(AtomicUsize::new(0));
        let backend = RecoveryBackend {
            plans: Mutex::new(VecDeque::from([CapturePlan::Input(PlannedInput {
                sample_rate: 48_000,
                samples: vec![0.25; 480],
                open_duration: Duration::ZERO,
            })])),
            errors: Arc::new(Mutex::new(Vec::new())),
            starts: starts.clone(),
        };
        let mut mic = MicAudioSource::with_backend_and_clock(
            16_000,
            Box::new(backend),
            Arc::new(|_| {}),
            Arc::new(FakeClock::default()),
        );
        mic.start().unwrap();

        for death in [
            StreamDeath {
                source: CaptureSource::MeetingMicrophone,
                generation: StreamGeneration(0),
            },
            StreamDeath {
                source: CaptureSource::Dictation,
                generation: StreamGeneration(1),
            },
        ] {
            assert_eq!(
                mic.stream_died(death).await.unwrap(),
                CaptureRecovery::Ignored
            );
        }
        assert_eq!(starts.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn fresh_meetings_resolve_the_current_default_input() {
        let backend = RecoveryBackend {
            plans: Mutex::new(VecDeque::from([
                CapturePlan::Input(PlannedInput {
                    sample_rate: 48_000,
                    samples: vec![0.25; 480],
                    open_duration: Duration::ZERO,
                }),
                CapturePlan::Input(PlannedInput {
                    sample_rate: 44_100,
                    samples: vec![-0.25; 441],
                    open_duration: Duration::ZERO,
                }),
            ])),
            errors: Arc::new(Mutex::new(Vec::new())),
            starts: Arc::new(AtomicUsize::new(0)),
        };
        let mut mic = MicAudioSource::with_backend_and_clock(
            16_000,
            Box::new(backend),
            Arc::new(|_| {}),
            Arc::new(FakeClock::default()),
        );

        mic.start().unwrap();
        let first = mic.stop().unwrap();
        mic.start().unwrap();
        let second = mic.stop().unwrap();

        assert_eq!(first.len(), 160);
        assert!(first.iter().all(|sample| *sample > 0.0));
        assert_eq!(second.len(), 160);
        assert!(second.iter().all(|sample| *sample < 0.0));
    }

    #[tokio::test(start_paused = true)]
    async fn stop_while_degraded_closes_the_outstanding_gap() {
        let clock = Arc::new(TokioClock {
            origin: tokio::time::Instant::now(),
        });
        let errors = Arc::new(Mutex::new(Vec::new()));
        let events = Arc::new(Mutex::new(Vec::new()));
        let backend = RecoveryBackend {
            plans: Mutex::new(VecDeque::from([
                CapturePlan::Input(PlannedInput {
                    sample_rate: 48_000,
                    samples: vec![0.25; 480],
                    open_duration: Duration::ZERO,
                }),
                CapturePlan::Fail("replacement 1 failed"),
                CapturePlan::Fail("replacement 2 failed"),
                CapturePlan::Fail("replacement 3 failed"),
            ])),
            errors: errors.clone(),
            starts: Arc::new(AtomicUsize::new(0)),
        };
        let mut mic = MicAudioSource::with_backend_and_clock(
            16_000,
            Box::new(backend),
            {
                let events = events.clone();
                Arc::new(move |event| events.lock().unwrap().push(event))
            },
            clock,
        );

        mic.start().unwrap();
        errors.lock().unwrap()[0]();
        let death = events.lock().unwrap().pop().unwrap();
        assert_eq!(
            mic.stream_died(death).await.unwrap(),
            CaptureRecovery::Degraded
        );
        tokio::time::advance(Duration::from_millis(250)).await;
        let track = mic.stop().unwrap();

        assert_eq!(track.len(), 36_160);
        assert!(track[..160].iter().all(|sample| *sample > 0.0));
        assert!(track[160..].iter().all(|sample| *sample == 0.0));
        assert_eq!(AudioMixer::mix(&[track, vec![0.1; 36_160]]).len(), 36_160);
    }

    #[tokio::test]
    async fn initial_degradation_recovers_with_prefix_silence() {
        let clock = Arc::new(FakeClock::default());
        let backend = RecoveryBackend {
            plans: Mutex::new(VecDeque::from([
                CapturePlan::Fail("initial Default Input unavailable"),
                CapturePlan::Input(PlannedInput {
                    sample_rate: 48_000,
                    samples: vec![0.25; 480],
                    open_duration: Duration::ZERO,
                }),
            ])),
            errors: Arc::new(Mutex::new(Vec::new())),
            starts: Arc::new(AtomicUsize::new(0)),
        };
        let mut mic = MicAudioSource::with_backend_and_clock(
            16_000,
            Box::new(backend),
            Arc::new(|_| {}),
            clock.clone(),
        );

        assert!(mic.start().is_err());
        assert!(mic.is_active());
        assert!(!mic.has_live_stream());
        clock.advance(Duration::from_millis(300));
        assert_eq!(
            mic.default_input_switched().await.unwrap(),
            CaptureRecovery::Capturing
        );
        let track = mic.stop().unwrap();

        assert_eq!(track.len(), 4_960);
        assert!(track[..4_800].iter().all(|sample| *sample == 0.0));
        assert!(track[4_800..].iter().all(|sample| *sample > 0.0));
    }
}
