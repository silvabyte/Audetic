//! Recoverable meeting System Tap capture.
//!
//! Each Default Output contributes one native-rate Segment. Completed Segments
//! are normalized independently and capture gaps become canonical Silence Fill
//! so the System Tap remains aligned with the meeting microphone.

use anyhow::{anyhow, Context, Result};
use tracing::{debug, info, warn};

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::audio_source::{AudioSource, MeetingSystemSource};
use super::capture_recovery::{start_capture_with_retries, CaptureRecovery};
use super::mic_source::MonotonicClock;
use super::resample::{push_mono_f32, resample_mono_f32};
use super::stream_event::{CaptureSource, StreamDeath, StreamEventSink, StreamGeneration};

pub(crate) type SystemTapDataCallback = Box<dyn FnMut(&[f32], usize) + Send + 'static>;
pub(crate) type SystemTapErrorCallback = Box<dyn FnMut() + Send + 'static>;

pub(crate) trait SystemTapBackend: Send + Sync {
    fn start_default_output(
        &self,
        on_data: SystemTapDataCallback,
        on_error: SystemTapErrorCallback,
    ) -> Result<ActiveSystemTap>;
}

trait SystemTapStream: Send + Sync {}

impl<T: Send + Sync + 'static> SystemTapStream for T {}

/// One active native-rate System Tap Segment. Dropping it tears down the
/// underlying stream and its private CoreAudio tap resources.
pub(crate) struct ActiveSystemTap {
    native_sample_rate: u32,
    _stream: Box<dyn SystemTapStream>,
}

impl ActiveSystemTap {
    pub(crate) fn new(native_sample_rate: u32, stream: impl Send + Sync + 'static) -> Self {
        Self {
            native_sample_rate,
            _stream: Box::new(stream),
        }
    }

    fn native_sample_rate(&self) -> u32 {
        self.native_sample_rate
    }
}

struct ActiveSegment {
    tap: ActiveSystemTap,
    native_samples: Arc<Mutex<Vec<f32>>>,
    generation: StreamGeneration,
    first_data: Arc<Mutex<Option<(Duration, usize)>>>,
    attempt_started_at: Duration,
    death_started_at: Arc<Mutex<Option<Duration>>>,
    gap_before: Option<Duration>,
}

struct ClosedSegment {
    native_samples: Vec<f32>,
    native_sample_rate: u32,
    first_sample_at: Option<Duration>,
    ended_at: Duration,
    death_started_at: Option<Duration>,
    gap_before: Option<Duration>,
}

pub(crate) struct SystemTapAudioSource {
    backend: Box<dyn SystemTapBackend>,
    active_segment: Option<ActiveSegment>,
    canonical_samples: Vec<f32>,
    stream_generation: StreamGeneration,
    live_generation: Arc<AtomicU64>,
    stream_event_sink: StreamEventSink,
    clock: Arc<dyn MonotonicClock>,
    meeting_started_at: Option<Duration>,
    pending_gap_started_at: Option<Duration>,
    session_active: bool,
    captured_audio: bool,
    captured_nonzero_audio: bool,
    target_sample_rate: u32,
}

impl SystemTapAudioSource {
    pub(crate) fn with_backend_and_clock(
        sample_rate: u32,
        backend: Box<dyn SystemTapBackend>,
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
            meeting_started_at: None,
            pending_gap_started_at: None,
            session_active: false,
            captured_audio: false,
            captured_nonzero_audio: false,
            target_sample_rate: sample_rate,
        }
    }

    fn start_segment(&mut self) -> Result<ActiveSegment> {
        let generation = self
            .stream_generation
            .next()
            .context("Failed to allocate System Tap Stream Generation")?;
        self.stream_generation = generation;
        let native_samples = Arc::new(Mutex::new(Vec::new()));
        let callback_samples = native_samples.clone();
        let first_data = Arc::new(Mutex::new(None));
        let callback_first_data = first_data.clone();
        let data_clock = self.clock.clone();
        let on_data: SystemTapDataCallback = Box::new(move |data, channels| {
            callback_first_data.lock().unwrap().get_or_insert_with(|| {
                let frames = if channels == 0 {
                    0
                } else {
                    data.len() / channels
                };
                (data_clock.now(), frames)
            });
            push_mono_f32(data, channels, &callback_samples);
        });

        let live_generation = self.live_generation.clone();
        let death_started_at = Arc::new(Mutex::new(None));
        let callback_death_started_at = death_started_at.clone();
        let clock = self.clock.clone();
        let stream_event_sink = self.stream_event_sink.clone();
        let on_error: SystemTapErrorCallback = Box::new(move || {
            if live_generation
                .compare_exchange(generation.0, 0, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
            {
                *callback_death_started_at.lock().unwrap() = Some(clock.now());
                stream_event_sink(StreamDeath {
                    source: CaptureSource::SystemTap,
                    generation,
                });
            }
        });

        self.live_generation.store(generation.0, Ordering::SeqCst);
        let attempt_started_at = self.clock.now();
        let tap = match self.backend.start_default_output(on_data, on_error) {
            Ok(tap) => tap,
            Err(error) => {
                let _ = self.live_generation.compare_exchange(
                    generation.0,
                    0,
                    Ordering::SeqCst,
                    Ordering::SeqCst,
                );
                return Err(error).context("Failed to start System Tap Segment");
            }
        };
        if self.live_generation.load(Ordering::SeqCst) != generation.0 {
            drop(tap);
            return Err(anyhow!("System Tap died while its Segment was starting"));
        }

        Ok(ActiveSegment {
            tap,
            native_samples,
            generation,
            first_data,
            attempt_started_at,
            death_started_at,
            gap_before: None,
        })
    }

    fn install_segment(&mut self, segment: ActiveSegment) {
        self.active_segment = Some(segment);
    }

    fn begin_gap(&mut self) {
        self.set_pending_gap(self.clock.now());
    }

    fn set_pending_gap(&mut self, started_at: Duration) {
        self.pending_gap_started_at = Some(
            self.pending_gap_started_at
                .map(|pending| pending.min(started_at))
                .unwrap_or(started_at),
        );
    }

    fn append_silence_fill(&mut self) -> Result<()> {
        let started_at = self.pending_gap_started_at.take();
        let Some(started_at) = started_at else {
            return Ok(());
        };
        self.append_silence_fill_between(started_at, self.clock.now())
    }

    fn append_silence_fill_between(
        &mut self,
        started_at: Duration,
        ended_at: Duration,
    ) -> Result<()> {
        let elapsed = ended_at.saturating_sub(started_at);
        let fill_samples = elapsed.as_nanos() * u128::from(self.target_sample_rate)
            / Duration::from_secs(1).as_nanos();
        let fill_samples = usize::try_from(fill_samples).context("Silence Fill is too long")?;
        self.canonical_samples
            .resize(self.canonical_samples.len() + fill_samples, 0.0);
        debug!(
            "Inserted {} samples of System Tap Silence Fill for {:?}",
            fill_samples, elapsed
        );
        Ok(())
    }

    fn duration_for_frames(frames: usize, sample_rate: u32) -> Result<Duration> {
        if sample_rate == 0 {
            return Err(anyhow!("Segment sample rate cannot be zero"));
        }
        let nanos = frames as u128 * Duration::from_secs(1).as_nanos() / u128::from(sample_rate);
        Ok(Duration::from_nanos(
            u64::try_from(nanos).context("Segment duration overflowed")?,
        ))
    }

    fn append_closed_segment(&mut self, segment: ClosedSegment) -> Result<()> {
        if segment.native_samples.is_empty() {
            return Ok(());
        }

        let first_sample_at = segment
            .first_sample_at
            .expect("non-empty Segment has first-sample timing");
        if let Some(gap_before) = segment.gap_before {
            self.append_silence_fill_between(gap_before, first_sample_at)
                .context("Failed to align System Tap Segment")?;
        }
        let contains_nonzero = segment.native_samples.iter().any(|sample| *sample != 0.0);
        let canonical = resample_mono_f32(
            &segment.native_samples,
            segment.native_sample_rate,
            self.target_sample_rate,
        )
        .context("Failed to normalize System Tap Segment")?;
        self.captured_audio |= !canonical.is_empty();
        self.captured_nonzero_audio |= contains_nonzero && !canonical.is_empty();
        debug!(
            "Closed System Tap Segment: {} native @ {} Hz -> {} canonical @ {} Hz",
            segment.native_samples.len(),
            segment.native_sample_rate,
            canonical.len(),
            self.target_sample_rate
        );
        self.canonical_samples.extend(canonical);
        Ok(())
    }

    fn close_current_segment(&mut self) -> Result<Option<ClosedSegment>> {
        self.live_generation.store(0, Ordering::SeqCst);
        let Some(segment) = self.active_segment.take() else {
            return Ok(None);
        };
        let native_sample_rate = segment.tap.native_sample_rate();
        // CoreAudio needs the old stream and its private tap resources gone
        // before a replacement Default Output can be opened.
        drop(segment.tap);
        let death_started_at = *segment.death_started_at.lock().unwrap();
        let native = std::mem::take(&mut *segment.native_samples.lock().unwrap());
        if native.is_empty() {
            let ended_at = segment.gap_before.unwrap_or_else(|| {
                self.meeting_started_at
                    .map(|started_at| started_at.max(segment.attempt_started_at))
                    .unwrap_or(segment.attempt_started_at)
            });
            return Ok(Some(ClosedSegment {
                native_samples: native,
                native_sample_rate,
                first_sample_at: None,
                ended_at,
                death_started_at,
                gap_before: segment.gap_before,
            }));
        }

        let (callback_at, first_buffer_frames) = segment
            .first_data
            .lock()
            .unwrap()
            .context("Captured System Tap samples have no first callback timing")?;
        let first_sample_at = callback_at
            .saturating_sub(
                Self::duration_for_frames(first_buffer_frames, native_sample_rate)
                    .context("Failed to calculate first System Tap buffer duration")?,
            )
            .max(segment.attempt_started_at);
        let native_len = native.len();
        Ok(Some(ClosedSegment {
            native_samples: native,
            native_sample_rate,
            first_sample_at: Some(first_sample_at),
            ended_at: first_sample_at.saturating_add(
                Self::duration_for_frames(native_len, native_sample_rate)
                    .context("Failed to calculate System Tap Segment duration")?,
            ),
            death_started_at,
            gap_before: segment.gap_before,
        }))
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
            if death.source != CaptureSource::SystemTap
                || current_generation != Some(death.generation)
            {
                return Ok(CaptureRecovery::Ignored);
            }
        }

        if death.is_none() {
            tokio::task::yield_now().await;
        }

        let mut closed_segments = Vec::new();
        let closed = self
            .close_current_segment()
            .context("Failed to close System Tap Segment for replacement")?;
        match closed {
            Some(closed) => {
                let gap_started_at = if death.is_some() || closed.native_samples.is_empty() {
                    closed.ended_at
                } else {
                    // A proactive switch keeps the old tap authoritative until
                    // it has closed and drained. Deaths instead gap from the
                    // final captured sample because the stream was already lost.
                    self.clock.now()
                };
                self.set_pending_gap(gap_started_at);
                closed_segments.push(closed);
            }
            None if self.pending_gap_started_at.is_none() => self.begin_gap(),
            _ => {}
        }

        let recovery = match start_capture_with_retries(
            "replacement meeting Default Output",
            || self.start_segment(),
        )
        .await
        {
            Ok(mut segment) => {
                segment.gap_before = self.pending_gap_started_at.take();
                self.install_segment(segment);
                if !self.has_live_stream() {
                    let closed = self.close_current_segment().context(
                        "Failed to close replacement System Tap Segment that died while being installed",
                    )?;
                    if let Some(closed) = closed {
                        self.set_pending_gap(closed.ended_at);
                        closed_segments.push(closed);
                    }
                    warn!("System Tap remains in Degraded Capture");
                    CaptureRecovery::Degraded
                } else {
                    info!("Recovered System Tap on the current Default Output");
                    CaptureRecovery::Capturing
                }
            }
            Err(_) => {
                warn!("System Tap remains in Degraded Capture");
                CaptureRecovery::Degraded
            }
        };

        for segment in closed_segments {
            self.append_closed_segment(segment)
                .context("Failed to preserve replaced System Tap Segment")?;
        }
        Ok(recovery)
    }
}

impl AudioSource for SystemTapAudioSource {
    fn start(&mut self) -> Result<()> {
        if self.session_active {
            return Err(anyhow!("System Tap already recording"));
        }

        self.close_current_segment()
            .context("Failed to reset previous System Tap Segment")?;
        self.canonical_samples.clear();
        self.meeting_started_at = None;
        self.pending_gap_started_at = None;
        self.session_active = true;
        self.captured_audio = false;
        self.captured_nonzero_audio = false;

        match self.start_segment() {
            Ok(segment) => {
                self.install_segment(segment);
                info!("System Tap capture started");
                Ok(())
            }
            Err(error) => Err(error).context("Failed to start System Tap"),
        }
    }

    fn stop(&mut self) -> Result<Vec<f32>> {
        if !self.session_active {
            return Err(anyhow!("System Tap not recording"));
        }
        self.session_active = false;
        let closed = self
            .close_current_segment()
            .context("Failed to close final System Tap Segment")?;
        if let Some(closed) = closed {
            if closed.native_samples.is_empty() || closed.death_started_at.is_some() {
                self.set_pending_gap(closed.ended_at);
            }
            self.append_closed_segment(closed)
                .context("Failed to preserve final System Tap Segment")?;
        }
        if !self.captured_audio || !self.captured_nonzero_audio {
            self.pending_gap_started_at = None;
            self.canonical_samples.clear();
            self.captured_audio = false;
            warn!(
                "System Tap stopped without non-silent captured audio; check System Audio Recording permission"
            );
            return Ok(Vec::new());
        }
        self.append_silence_fill()
            .context("Failed to close final System Tap Silence Fill")?;
        let canonical = std::mem::take(&mut self.canonical_samples);
        info!(
            "System Tap stopped: {} canonical samples @ {} Hz",
            canonical.len(),
            self.target_sample_rate
        );
        Ok(canonical)
    }

    fn is_active(&self) -> bool {
        self.session_active
    }

    fn has_live_stream(&self) -> bool {
        self.active_segment.is_some() && self.live_generation.load(Ordering::SeqCst) != 0
    }

    fn sample_rate(&self) -> u32 {
        self.target_sample_rate
    }
}

#[async_trait::async_trait(?Send)]
impl MeetingSystemSource for SystemTapAudioSource {
    fn supports_hot_swap(&self) -> bool {
        true
    }

    fn mark_meeting_started(&mut self) {
        let started_at = self.clock.now();
        self.meeting_started_at = Some(started_at);
        if self.session_active && !self.has_live_stream() {
            self.pending_gap_started_at = Some(started_at);
        }
    }

    fn has_captured_audio(&self) -> bool {
        self.captured_audio
    }

    async fn default_output_switched(&mut self) -> Result<CaptureRecovery> {
        self.replace_capture(None).await
    }

    async fn stream_died(&mut self, death: StreamDeath) -> Result<CaptureRecovery> {
        self.replace_capture(Some(death)).await
    }
}

impl Drop for SystemTapAudioSource {
    fn drop(&mut self) {
        if self.session_active {
            debug!("Dropping active System Tap, cleaning up");
            let _ = self.stop();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

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

    struct PlannedTap {
        sample_rate: u32,
        samples: Vec<f32>,
        open_duration: Duration,
    }

    enum Plan {
        Tap(PlannedTap),
        Fail(Duration),
    }

    struct PlannedBackend {
        plans: Mutex<VecDeque<Plan>>,
        clock: Arc<FakeClock>,
        drops: Arc<AtomicUsize>,
        errors: Arc<Mutex<Vec<SystemTapErrorCallback>>>,
    }

    struct TeardownOrderedBackend {
        starts: AtomicUsize,
        drops: Arc<AtomicUsize>,
    }

    struct AdvancingTap {
        clock: Arc<FakeClock>,
        drops: Arc<AtomicUsize>,
    }

    impl Drop for AdvancingTap {
        fn drop(&mut self) {
            self.clock.advance(Duration::from_millis(100));
            self.drops.fetch_add(1, Ordering::SeqCst);
        }
    }

    impl SystemTapBackend for TeardownOrderedBackend {
        fn start_default_output(
            &self,
            mut on_data: SystemTapDataCallback,
            _on_error: SystemTapErrorCallback,
        ) -> Result<ActiveSystemTap> {
            let start = self.starts.fetch_add(1, Ordering::SeqCst);
            assert_eq!(
                self.drops.load(Ordering::SeqCst),
                start,
                "previous System Tap must be dropped before opening replacement"
            );
            on_data(&[0.25; 160], 1);
            Ok(ActiveSystemTap::new(
                16_000,
                FakeTap {
                    drops: self.drops.clone(),
                },
            ))
        }
    }

    struct FakeTap {
        drops: Arc<AtomicUsize>,
    }

    impl Drop for FakeTap {
        fn drop(&mut self) {
            self.drops.fetch_add(1, Ordering::SeqCst);
        }
    }

    impl SystemTapBackend for PlannedBackend {
        fn start_default_output(
            &self,
            mut on_data: SystemTapDataCallback,
            on_error: SystemTapErrorCallback,
        ) -> Result<ActiveSystemTap> {
            match self.plans.lock().unwrap().pop_front().unwrap() {
                Plan::Tap(tap) => {
                    self.clock.advance(tap.open_duration);
                    on_data(&tap.samples, 1);
                    self.errors.lock().unwrap().push(on_error);
                    Ok(ActiveSystemTap::new(
                        tap.sample_rate,
                        FakeTap {
                            drops: self.drops.clone(),
                        },
                    ))
                }
                Plan::Fail(duration) => {
                    self.clock.advance(duration);
                    Err(anyhow!("Default Output unavailable"))
                }
            }
        }
    }

    fn source(
        plans: impl IntoIterator<Item = Plan>,
        clock: Arc<FakeClock>,
        drops: Arc<AtomicUsize>,
        errors: Arc<Mutex<Vec<SystemTapErrorCallback>>>,
        events: Arc<Mutex<Vec<StreamDeath>>>,
    ) -> SystemTapAudioSource {
        SystemTapAudioSource::with_backend_and_clock(
            16_000,
            Box::new(PlannedBackend {
                plans: Mutex::new(plans.into_iter().collect()),
                clock: clock.clone(),
                drops,
                errors,
            }),
            Arc::new(move |death| events.lock().unwrap().push(death)),
            clock,
        )
    }

    #[tokio::test]
    async fn output_switch_preserves_segments_with_exact_silence_fill() {
        let clock = Arc::new(FakeClock::default());
        let drops = Arc::new(AtomicUsize::new(0));
        let mut tap = source(
            [
                Plan::Tap(PlannedTap {
                    sample_rate: 48_000,
                    samples: vec![0.25; 480],
                    open_duration: Duration::from_millis(10),
                }),
                Plan::Tap(PlannedTap {
                    sample_rate: 44_100,
                    samples: vec![-0.25; 441],
                    open_duration: Duration::from_millis(260),
                }),
            ],
            clock,
            drops.clone(),
            Arc::new(Mutex::new(Vec::new())),
            Arc::new(Mutex::new(Vec::new())),
        );

        tap.start().unwrap();
        assert_eq!(
            tap.default_output_switched().await.unwrap(),
            CaptureRecovery::Capturing
        );
        let samples = tap.stop().unwrap();

        assert_eq!(drops.load(Ordering::SeqCst), 2);
        assert_eq!(samples.len(), 4_320);
        assert!(samples[..160].iter().all(|sample| *sample > 0.0));
        assert!(samples[160..4_160].iter().all(|sample| *sample == 0.0));
        assert!(samples[4_160..].iter().all(|sample| *sample < 0.0));
    }

    #[tokio::test]
    async fn output_switch_drops_old_tap_before_opening_replacement() {
        let drops = Arc::new(AtomicUsize::new(0));
        let mut tap = SystemTapAudioSource::with_backend_and_clock(
            16_000,
            Box::new(TeardownOrderedBackend {
                starts: AtomicUsize::new(0),
                drops: drops.clone(),
            }),
            Arc::new(|_| {}),
            Arc::new(FakeClock::default()),
        );

        tap.start().unwrap();
        assert_eq!(
            tap.default_output_switched().await.unwrap(),
            CaptureRecovery::Capturing
        );
        tap.stop().unwrap();

        assert_eq!(drops.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn proactive_switch_gap_starts_after_old_tap_teardown() {
        struct Backend {
            starts: AtomicUsize,
            clock: Arc<FakeClock>,
            drops: Arc<AtomicUsize>,
        }

        impl SystemTapBackend for Backend {
            fn start_default_output(
                &self,
                mut on_data: SystemTapDataCallback,
                _on_error: SystemTapErrorCallback,
            ) -> Result<ActiveSystemTap> {
                let start = self.starts.fetch_add(1, Ordering::SeqCst);
                if start == 1 {
                    self.clock.advance(Duration::from_millis(250));
                }
                on_data(&[0.25; 160], 1);
                Ok(ActiveSystemTap::new(
                    16_000,
                    AdvancingTap {
                        clock: self.clock.clone(),
                        drops: self.drops.clone(),
                    },
                ))
            }
        }

        let clock = Arc::new(FakeClock::default());
        let drops = Arc::new(AtomicUsize::new(0));
        let mut tap = SystemTapAudioSource::with_backend_and_clock(
            16_000,
            Box::new(Backend {
                starts: AtomicUsize::new(0),
                clock: clock.clone(),
                drops,
            }),
            Arc::new(|_| {}),
            clock,
        );

        tap.start().unwrap();
        tap.default_output_switched().await.unwrap();
        let samples = tap.stop().unwrap();

        assert_eq!(samples.len(), 4_160);
        assert!(samples[160..4_000].iter().all(|sample| *sample == 0.0));
    }

    #[tokio::test]
    async fn empty_segments_preserve_the_original_system_capture_gap() {
        let clock = Arc::new(FakeClock::default());
        let mut tap = source(
            [
                Plan::Tap(PlannedTap {
                    sample_rate: 16_000,
                    samples: Vec::new(),
                    open_duration: Duration::ZERO,
                }),
                Plan::Tap(PlannedTap {
                    sample_rate: 16_000,
                    samples: Vec::new(),
                    open_duration: Duration::from_millis(200),
                }),
                Plan::Tap(PlannedTap {
                    sample_rate: 16_000,
                    samples: vec![0.25; 160],
                    open_duration: Duration::from_millis(100),
                }),
            ],
            clock,
            Arc::new(AtomicUsize::new(0)),
            Arc::new(Mutex::new(Vec::new())),
            Arc::new(Mutex::new(Vec::new())),
        );

        tap.start().unwrap();
        tap.mark_meeting_started();
        tap.default_output_switched().await.unwrap();
        tap.default_output_switched().await.unwrap();
        let samples = tap.stop().unwrap();

        assert_eq!(samples.len(), 4_800);
        assert!(samples[..4_640].iter().all(|sample| *sample == 0.0));
        assert!(samples[4_640..].iter().all(|sample| *sample > 0.0));
    }

    #[tokio::test]
    async fn current_generation_death_recovers_and_stale_reports_are_ignored() {
        let clock = Arc::new(FakeClock::default());
        let errors = Arc::new(Mutex::new(Vec::new()));
        let events = Arc::new(Mutex::new(Vec::new()));
        let mut tap = source(
            [
                Plan::Tap(PlannedTap {
                    sample_rate: 16_000,
                    samples: vec![0.25; 160],
                    open_duration: Duration::ZERO,
                }),
                Plan::Tap(PlannedTap {
                    sample_rate: 16_000,
                    samples: vec![-0.25; 160],
                    open_duration: Duration::ZERO,
                }),
            ],
            clock.clone(),
            Arc::new(AtomicUsize::new(0)),
            errors.clone(),
            events.clone(),
        );
        tap.start().unwrap();

        errors.lock().unwrap()[0]();
        let death = events.lock().unwrap()[0];
        assert_eq!(death.source, CaptureSource::SystemTap);
        assert_eq!(death.generation, StreamGeneration(1));
        assert_eq!(
            tap.stream_died(death).await.unwrap(),
            CaptureRecovery::Capturing
        );
        assert_eq!(
            tap.stream_died(death).await.unwrap(),
            CaptureRecovery::Ignored
        );
    }

    #[tokio::test(start_paused = true)]
    async fn exhausted_retries_remain_degraded_until_a_later_switch() {
        let clock = Arc::new(FakeClock::default());
        let mut tap = source(
            [
                Plan::Tap(PlannedTap {
                    sample_rate: 16_000,
                    samples: vec![0.25; 160],
                    open_duration: Duration::ZERO,
                }),
                Plan::Fail(Duration::from_millis(10)),
                Plan::Fail(Duration::from_millis(20)),
                Plan::Fail(Duration::from_millis(30)),
                Plan::Tap(PlannedTap {
                    sample_rate: 16_000,
                    samples: vec![-0.25; 160],
                    open_duration: Duration::from_millis(40),
                }),
            ],
            clock.clone(),
            Arc::new(AtomicUsize::new(0)),
            Arc::new(Mutex::new(Vec::new())),
            Arc::new(Mutex::new(Vec::new())),
        );
        tap.start().unwrap();

        assert_eq!(
            tap.default_output_switched().await.unwrap(),
            CaptureRecovery::Degraded
        );
        assert!(!tap.has_live_stream());
        assert_eq!(
            tap.default_output_switched().await.unwrap(),
            CaptureRecovery::Capturing
        );
        assert!(tap.has_live_stream());
    }

    #[tokio::test(start_paused = true)]
    async fn stream_death_exhausts_retries_then_recovers_on_output_switch() {
        let clock = Arc::new(FakeClock::default());
        let errors = Arc::new(Mutex::new(Vec::new()));
        let events = Arc::new(Mutex::new(Vec::new()));
        let mut tap = source(
            [
                Plan::Tap(PlannedTap {
                    sample_rate: 16_000,
                    samples: vec![0.25; 160],
                    open_duration: Duration::ZERO,
                }),
                Plan::Fail(Duration::ZERO),
                Plan::Fail(Duration::ZERO),
                Plan::Fail(Duration::ZERO),
                Plan::Tap(PlannedTap {
                    sample_rate: 16_000,
                    samples: vec![-0.25; 160],
                    open_duration: Duration::ZERO,
                }),
            ],
            clock,
            Arc::new(AtomicUsize::new(0)),
            errors.clone(),
            events.clone(),
        );
        tap.start().unwrap();
        errors.lock().unwrap()[0]();
        let death = events.lock().unwrap()[0];

        assert_eq!(
            tap.stream_died(death).await.unwrap(),
            CaptureRecovery::Degraded
        );
        assert_eq!(
            tap.default_output_switched().await.unwrap(),
            CaptureRecovery::Capturing
        );
        assert!(tap.stop().unwrap().iter().any(|sample| *sample < 0.0));
    }

    #[tokio::test(start_paused = true)]
    async fn stop_while_degraded_preserves_audio_and_closes_the_gap() {
        let clock = Arc::new(FakeClock::default());
        let mut tap = source(
            [
                Plan::Tap(PlannedTap {
                    sample_rate: 16_000,
                    samples: vec![0.25; 160],
                    open_duration: Duration::ZERO,
                }),
                Plan::Fail(Duration::ZERO),
                Plan::Fail(Duration::ZERO),
                Plan::Fail(Duration::ZERO),
            ],
            clock.clone(),
            Arc::new(AtomicUsize::new(0)),
            Arc::new(Mutex::new(Vec::new())),
            Arc::new(Mutex::new(Vec::new())),
        );
        tap.start().unwrap();
        assert_eq!(
            tap.default_output_switched().await.unwrap(),
            CaptureRecovery::Degraded
        );
        clock.advance(Duration::from_millis(300));
        let samples = tap.stop().unwrap();

        assert_eq!(samples.len(), 4_960);
        assert!(samples[..160].iter().all(|sample| *sample > 0.0));
        assert!(samples[160..].iter().all(|sample| *sample == 0.0));
    }

    #[test]
    fn synthetic_silence_does_not_count_as_captured_audio() {
        let clock = Arc::new(FakeClock::default());
        let mut tap = source(
            [Plan::Fail(Duration::ZERO)],
            clock.clone(),
            Arc::new(AtomicUsize::new(0)),
            Arc::new(Mutex::new(Vec::new())),
            Arc::new(Mutex::new(Vec::new())),
        );

        assert!(tap.start().is_err());
        tap.mark_meeting_started();
        clock.advance(Duration::from_secs(1));

        assert!(tap.stop().unwrap().is_empty());
        assert!(!tap.has_captured_audio());
    }

    #[test]
    fn all_zero_callback_audio_is_treated_as_unavailable_system_capture() {
        let clock = Arc::new(FakeClock::default());
        let mut tap = source(
            [Plan::Tap(PlannedTap {
                sample_rate: 16_000,
                samples: vec![0.0; 160],
                open_duration: Duration::ZERO,
            })],
            clock,
            Arc::new(AtomicUsize::new(0)),
            Arc::new(Mutex::new(Vec::new())),
            Arc::new(Mutex::new(Vec::new())),
        );

        tap.start().unwrap();

        assert!(tap.stop().unwrap().is_empty());
        assert!(!tap.has_captured_audio());
    }
}
