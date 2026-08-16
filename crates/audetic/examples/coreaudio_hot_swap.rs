//! Operator-assisted, end-to-end CoreAudio hot-swap harness.
//!
//! See `docs/coreaudio-hot-swap-harness.md` before running this example.

#[cfg(any(target_os = "macos", test))]
mod wav_analysis {
    use std::collections::BTreeMap;
    use std::f64::consts::TAU;
    use std::path::Path;

    use anyhow::{Context, Result};
    use hound::{SampleFormat, WavReader, WavSpec, WavWriter};
    use serde::Serialize;

    #[derive(Debug, Clone, Serialize)]
    pub struct WavMetrics {
        pub sample_rate_hz: u32,
        pub channels: u16,
        pub frames: usize,
        pub duration_seconds: f64,
        pub peak: f32,
        pub rms: f32,
        pub silence_frames: usize,
        pub longest_silence_frames: usize,
        pub silence_gaps: usize,
        pub marker_amplitudes: BTreeMap<String, f64>,
    }

    pub fn write_tone(path: &Path, frequency_hz: f64, seconds: f64) -> Result<()> {
        let sample_rate = 48_000_u32;
        let frames = (seconds * f64::from(sample_rate)).round() as usize;
        let mut writer = WavWriter::create(
            path,
            WavSpec {
                channels: 1,
                sample_rate,
                bits_per_sample: 32,
                sample_format: SampleFormat::Float,
            },
        )
        .with_context(|| format!("failed to create {}", path.display()))?;
        for frame in 0..frames {
            let phase = TAU * frequency_hz * frame as f64 / f64::from(sample_rate);
            writer.write_sample((0.35 * phase.sin()) as f32)?;
        }
        writer.finalize()?;
        Ok(())
    }

    pub fn analyze(path: &Path, markers: &[(&str, f64)]) -> Result<WavMetrics> {
        let mut reader = WavReader::open(path)
            .with_context(|| format!("failed to open WAV {}", path.display()))?;
        let spec = reader.spec();
        let interleaved = match spec.sample_format {
            SampleFormat::Float => reader
                .samples::<f32>()
                .collect::<std::result::Result<Vec<_>, _>>()?,
            SampleFormat::Int => {
                let scale = 2_f32.powi(i32::from(spec.bits_per_sample) - 1);
                reader
                    .samples::<i32>()
                    .map(|sample| sample.map(|value| value as f32 / scale))
                    .collect::<std::result::Result<Vec<_>, _>>()?
            }
        };
        let channels = usize::from(spec.channels);
        anyhow::ensure!(channels > 0, "WAV has zero channels");
        let mono: Vec<f32> = interleaved
            .chunks_exact(channels)
            .map(|frame| frame.iter().sum::<f32>() / channels as f32)
            .collect();

        let peak = mono
            .iter()
            .fold(0.0_f32, |current, sample| current.max(sample.abs()));
        let rms = if mono.is_empty() {
            0.0
        } else {
            (mono.iter().map(|sample| sample * sample).sum::<f32>() / mono.len() as f32).sqrt()
        };
        let silence_threshold = 0.0001_f32;
        let minimum_gap = (spec.sample_rate / 50) as usize;
        let mut silence_frames = 0;
        let mut longest_silence_frames = 0;
        let mut silence_gaps = 0;
        let mut run = 0;
        for sample in &mono {
            if sample.abs() <= silence_threshold {
                run += 1;
                silence_frames += 1;
            } else {
                if run >= minimum_gap {
                    silence_gaps += 1;
                }
                longest_silence_frames = longest_silence_frames.max(run);
                run = 0;
            }
        }
        if run >= minimum_gap {
            silence_gaps += 1;
        }
        longest_silence_frames = longest_silence_frames.max(run);

        let marker_amplitudes = markers
            .iter()
            .map(|(name, frequency)| {
                let (sin_sum, cos_sum) = mono.iter().enumerate().fold(
                    (0.0_f64, 0.0_f64),
                    |(sin_sum, cos_sum), (frame, sample)| {
                        let phase = TAU * frequency * frame as f64 / f64::from(spec.sample_rate);
                        (
                            sin_sum + f64::from(*sample) * phase.sin(),
                            cos_sum + f64::from(*sample) * phase.cos(),
                        )
                    },
                );
                let amplitude = if mono.is_empty() {
                    0.0
                } else {
                    2.0 * (sin_sum * sin_sum + cos_sum * cos_sum).sqrt() / mono.len() as f64
                };
                ((*name).to_string(), amplitude)
            })
            .collect();

        Ok(WavMetrics {
            sample_rate_hz: spec.sample_rate,
            channels: spec.channels,
            frames: mono.len(),
            duration_seconds: mono.len() as f64 / f64::from(spec.sample_rate),
            peak,
            rms,
            silence_frames,
            longest_silence_frames,
            silence_gaps,
            marker_amplitudes,
        })
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn tone_analysis_reports_duration_and_marker() {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("tone.wav");
            write_tone(&path, 697.0, 0.5).unwrap();

            let metrics = analyze(&path, &[("mic", 697.0), ("other", 941.0)]).unwrap();

            assert_eq!(metrics.sample_rate_hz, 48_000);
            assert!((metrics.duration_seconds - 0.5).abs() < 0.001);
            assert!(metrics.marker_amplitudes["mic"] > 0.3);
            assert!(metrics.marker_amplitudes["other"] < 0.001);
        }

        #[test]
        fn silence_analysis_counts_only_durable_gaps() {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("gap.wav");
            let mut writer = WavWriter::create(
                &path,
                WavSpec {
                    channels: 1,
                    sample_rate: 16_000,
                    bits_per_sample: 32,
                    sample_format: SampleFormat::Float,
                },
            )
            .unwrap();
            for sample in std::iter::repeat_n(0.25_f32, 160)
                .chain(std::iter::repeat_n(0.0, 800))
                .chain(std::iter::repeat_n(-0.25, 160))
            {
                writer.write_sample(sample).unwrap();
            }
            writer.finalize().unwrap();

            let metrics = analyze(&path, &[]).unwrap();
            assert_eq!(metrics.silence_frames, 800);
            assert_eq!(metrics.longest_silence_frames, 800);
            assert_eq!(metrics.silence_gaps, 1);
        }
    }
}

#[cfg(any(target_os = "macos", test))]
mod log_analysis {
    pub fn suffix_after(baseline: &[String], current: &[String]) -> Option<Vec<String>> {
        if baseline.is_empty() {
            return Some(current.to_vec());
        }
        let anchor = baseline.last()?;
        let start = current.iter().rposition(|line| line == anchor)? + 1;
        Some(current[start..].to_vec())
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn returns_only_lines_after_the_snapshot_anchor() {
            let baseline = vec!["old-1".to_string(), "old-2".to_string()];
            let current = vec!["old-1".to_string(), "old-2".to_string(), "new".to_string()];

            assert_eq!(
                suffix_after(&baseline, &current),
                Some(vec!["new".to_string()])
            );
        }

        #[test]
        fn missing_anchor_never_reuses_historical_lines() {
            let baseline = vec!["anchor-aged-out".to_string()];
            let current = vec!["matching-but-historical".to_string()];

            assert_eq!(suffix_after(&baseline, &current), None);
        }
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use std::fs::{self, File};
    use std::io::{BufRead, BufReader, Write};
    use std::path::{Path, PathBuf};
    use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use anyhow::{bail, Context, Result};
    use audetic_core::url::{api_url, paths};
    use clap::{Parser, ValueEnum};
    use serde::{Deserialize, Serialize};
    use serde_json::{json, Value};
    use tokio::task::JoinHandle;
    use uuid::Uuid;

    use super::log_analysis::suffix_after;
    use super::wav_analysis::{self, WavMetrics};

    const RECORDING_STATUS_PATH: &str = "/status";
    const MEETING_START_PATH: &str = "/meetings/start";
    const MEETING_STOP_PATH: &str = "/meetings/stop";
    const MEETING_STATUS_PATH: &str = "/meetings/status";
    const MARKER_MINIMUM_AMPLITUDE: f64 = 0.01;

    #[derive(Clone, Copy, Debug, ValueEnum)]
    enum Mode {
        Idle,
        Live,
        Degraded,
    }

    #[derive(Debug, Parser)]
    #[command(about = "Manual macOS CoreAudio default-device churn harness")]
    struct Args {
        #[arg(long, value_enum, default_value = "idle")]
        mode: Mode,
        #[arg(long)]
        input_uid: Option<String>,
        #[arg(long)]
        output_uid: Option<String>,
        #[arg(long, value_delimiter = ',')]
        expect_native_rates: Vec<u32>,
        #[arg(long)]
        expected_duration_secs: Option<f64>,
        #[arg(long, default_value_t = 0.75)]
        duration_tolerance_secs: f64,
        #[arg(long, default_value_t = 697.0)]
        idle_mic_marker_hz: f64,
        #[arg(long, default_value_t = 697.0)]
        live_pre_mic_marker_hz: f64,
        #[arg(long, default_value_t = 770.0)]
        live_post_mic_marker_hz: f64,
        #[arg(long, default_value_t = 941.0)]
        live_pre_system_marker_hz: f64,
        #[arg(long, default_value_t = 1209.0)]
        live_post_system_marker_hz: f64,
        #[arg(long, default_value_t = 3.0)]
        marker_seconds: f64,
        #[arg(long, default_value_t = 20)]
        timeout_secs: u64,
        #[arg(long)]
        list_devices: bool,
    }

    #[derive(Debug, Clone, Deserialize, Serialize)]
    struct DeviceSummary {
        device_id: u32,
        uid: String,
        name: String,
        nominal_rate_hz: f64,
        has_input: bool,
        has_output: bool,
    }

    #[derive(Debug, Clone, Deserialize, Serialize)]
    struct HolderReady {
        event: String,
        protocol: u32,
        uid: String,
        name: String,
        device_id: u32,
        original_default_input_id: u32,
        original_default_output_id: u32,
        original_system_output_id: u32,
        input: DeviceSummary,
        output: DeviceSummary,
    }

    struct AggregateHolder {
        child: Child,
        stdin: ChildStdin,
        stdout: BufReader<ChildStdout>,
        ready: HolderReady,
        destroyed: bool,
    }

    struct MarkerFiles {
        idle_mic: PathBuf,
        live_pre_mic: PathBuf,
        live_post_mic: PathBuf,
        live_pre_system: PathBuf,
        live_post_system: PathBuf,
    }

    impl AggregateHolder {
        fn start(args: &Args, root: &Path, run_dir: &Path, run_id: &str) -> Result<Self> {
            let uid = Uuid::new_v4().hyphenated().to_string();
            let name = format!("Audetic Hot Swap {run_id}");
            let script = root.join("scripts/coreaudio_aggregate_holder.swift");
            let stderr = File::create(run_dir.join("holder.stderr.log"))?;
            let mut command = Command::new("xcrun");
            command
                .arg("swift")
                .arg(script)
                .arg("--name")
                .arg(&name)
                .arg("--uid")
                .arg(&uid)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::from(stderr));
            if let Some(input_uid) = &args.input_uid {
                command.arg("--input-uid").arg(input_uid);
            }
            if let Some(output_uid) = &args.output_uid {
                command.arg("--output-uid").arg(output_uid);
            }
            let mut child = command
                .spawn()
                .context("failed to start Swift aggregate holder")?;
            let stdin = child.stdin.take().context("holder stdin unavailable")?;
            let stdout = child.stdout.take().context("holder stdout unavailable")?;
            let mut stdout = BufReader::new(stdout);
            let mut line = String::new();
            stdout
                .read_line(&mut line)
                .context("failed to read holder readiness")?;
            let value: Value = serde_json::from_str(&line)
                .with_context(|| format!("invalid holder readiness: {line}"))?;
            anyhow::ensure!(
                value["event"].as_str() == Some("ready"),
                "aggregate holder failed: {value}"
            );
            let ready: HolderReady = serde_json::from_value(value)?;
            anyhow::ensure!(
                ready.protocol == 1
                    && ready.uid == uid
                    && ready.name == name
                    && ready.device_id != 0,
                "aggregate holder returned mismatched readiness: {ready:?}"
            );
            Ok(Self {
                child,
                stdin,
                stdout,
                ready,
                destroyed: false,
            })
        }

        fn command(&mut self, value: Value) -> Result<Value> {
            let requested_command = value["command"]
                .as_str()
                .context("holder request omitted command")?
                .to_string();
            serde_json::to_writer(&mut self.stdin, &value)?;
            self.stdin.write_all(b"\n")?;
            self.stdin.flush()?;
            let mut line = String::new();
            anyhow::ensure!(
                self.stdout.read_line(&mut line)? > 0,
                "holder exited while processing {requested_command}"
            );
            let response: Value = serde_json::from_str(&line)
                .with_context(|| format!("invalid holder response: {line}"))?;
            anyhow::ensure!(
                response["event"].as_str() == Some("ok")
                    && response["command"].as_str() == Some(requested_command.as_str()),
                "holder command failed or returned an unexpected response: {response}"
            );
            Ok(response)
        }

        fn set_input(&mut self) -> Result<()> {
            self.set_default("input", self.ready.device_id)
        }

        fn set_output(&mut self) -> Result<()> {
            self.set_default("output", self.ready.device_id)
        }

        fn set_default(&mut self, scope: &str, device_id: u32) -> Result<()> {
            let response = self.command(json!({
                "command": "set_default",
                "scope": scope,
                "device_id": device_id,
            }))?;
            anyhow::ensure!(
                response["scope"].as_str() == Some(scope)
                    && response["device_id"].as_u64() == Some(u64::from(device_id)),
                "holder confirmed the wrong default-device request: {response}"
            );
            Ok(())
        }

        fn restore(&mut self, scope: Option<&str>) -> Result<()> {
            let mut command = json!({ "command": "restore" });
            if let Some(scope) = scope {
                command["scope"] = Value::String(scope.to_string());
            }
            self.command(command)?;
            Ok(())
        }

        fn destroy(&mut self) -> Result<()> {
            if self.destroyed {
                return Ok(());
            }
            serde_json::to_writer(&mut self.stdin, &json!({ "command": "destroy" }))?;
            self.stdin.write_all(b"\n")?;
            self.stdin.flush()?;
            loop {
                let mut line = String::new();
                anyhow::ensure!(
                    self.stdout.read_line(&mut line)? > 0,
                    "holder exited before confirming aggregate destruction"
                );
                let response: Value = serde_json::from_str(&line)
                    .with_context(|| format!("invalid holder teardown response: {line}"))?;
                match response["event"].as_str() {
                    Some("destroyed") => break,
                    Some("warning") => continue,
                    Some("error") => bail!("holder teardown failed: {response}"),
                    _ => bail!("unexpected holder teardown response: {response}"),
                }
            }
            let status = self.child.wait()?;
            anyhow::ensure!(status.success(), "aggregate holder exited with {status}");
            self.destroyed = true;
            Ok(())
        }
    }

    impl Drop for AggregateHolder {
        fn drop(&mut self) {
            if self.destroyed {
                return;
            }
            let _ = serde_json::to_writer(&mut self.stdin, &json!({ "command": "destroy" }));
            let _ = self.stdin.write_all(b"\n");
            let _ = self.stdin.flush();
            // The holder may spend up to five seconds restoring each of three
            // defaults, then five seconds confirming aggregate disappearance.
            for _ in 0..250 {
                if self.child.try_wait().ok().flatten().is_some() {
                    return;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }

    #[derive(Debug, Serialize)]
    struct StatusObservation {
        elapsed_ms: u128,
        source: String,
        status: Value,
    }

    struct StatusMonitor {
        stop: Arc<AtomicBool>,
        task: JoinHandle<Vec<StatusObservation>>,
    }

    impl StatusMonitor {
        fn start(client: reqwest::Client) -> Self {
            let stop = Arc::new(AtomicBool::new(false));
            let task_stop = stop.clone();
            let task = tokio::spawn(async move {
                let started = Instant::now();
                let mut observations = Vec::new();
                let mut previous_recording = Value::Null;
                let mut previous_meeting = Value::Null;
                while !task_stop.load(Ordering::Relaxed) {
                    for (source, path, previous) in [
                        ("dictation", RECORDING_STATUS_PATH, &mut previous_recording),
                        ("meeting", MEETING_STATUS_PATH, &mut previous_meeting),
                    ] {
                        if let Ok(status) = get_json(&client, path).await {
                            if status != *previous {
                                *previous = status.clone();
                                observations.push(StatusObservation {
                                    elapsed_ms: started.elapsed().as_millis(),
                                    source: source.to_string(),
                                    status,
                                });
                            }
                        }
                    }
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
                observations
            });
            Self { stop, task }
        }

        async fn stop(self) -> Vec<StatusObservation> {
            self.stop.store(true, Ordering::Relaxed);
            self.task.await.unwrap_or_default()
        }
    }

    #[derive(Debug, Serialize)]
    struct Assertion {
        name: String,
        passed: bool,
        detail: String,
    }

    #[derive(Debug, Serialize)]
    struct Artifact {
        label: String,
        path: PathBuf,
        metrics: WavMetrics,
    }

    #[derive(Debug, Serialize)]
    struct Report {
        run_id: String,
        mode: String,
        daemon_pid_before: u32,
        daemon_pid_after: Option<u32>,
        aggregate: HolderReady,
        status_transitions: Vec<StatusObservation>,
        capture_telemetry: Vec<String>,
        artifacts: Vec<Artifact>,
        assertions: Vec<Assertion>,
        operator_notes: Vec<String>,
    }

    impl Report {
        fn assertion(&mut self, name: &str, passed: bool, detail: impl Into<String>) {
            self.assertions.push(Assertion {
                name: name.to_string(),
                passed,
                detail: detail.into(),
            });
        }
    }

    pub async fn main() -> Result<()> {
        let args = Args::parse();
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()?;
        let helper = root.join("scripts/coreaudio_aggregate_holder.swift");
        if args.list_devices {
            let status = Command::new("xcrun")
                .arg("swift")
                .arg(helper)
                .arg("--list")
                .status()?;
            anyhow::ensure!(status.success(), "device listing failed");
            return Ok(());
        }
        validate_args(&args)?;

        let run_id = format!(
            "{}-{}",
            chrono::Utc::now().format("%Y%m%dT%H%M%SZ"),
            &Uuid::new_v4().simple().to_string()[..8]
        );
        let run_dir = root.join("target/coreaudio-hot-swap").join(&run_id);
        fs::create_dir_all(&run_dir)?;
        let markers = MarkerFiles {
            idle_mic: run_dir.join(format!("idle-mic-marker-{}hz.wav", args.idle_mic_marker_hz)),
            live_pre_mic: run_dir.join(format!(
                "live-pre-mic-marker-{}hz.wav",
                args.live_pre_mic_marker_hz
            )),
            live_post_mic: run_dir.join(format!(
                "live-post-mic-marker-{}hz.wav",
                args.live_post_mic_marker_hz
            )),
            live_pre_system: run_dir.join(format!(
                "live-pre-system-marker-{}hz.wav",
                args.live_pre_system_marker_hz
            )),
            live_post_system: run_dir.join(format!(
                "live-post-system-marker-{}hz.wav",
                args.live_post_system_marker_hz
            )),
        };
        match args.mode {
            Mode::Idle => wav_analysis::write_tone(
                &markers.idle_mic,
                args.idle_mic_marker_hz,
                args.marker_seconds,
            )?,
            Mode::Live => {
                for (path, frequency) in [
                    (&markers.live_pre_mic, args.live_pre_mic_marker_hz),
                    (&markers.live_post_mic, args.live_post_mic_marker_hz),
                    (&markers.live_pre_system, args.live_pre_system_marker_hz),
                    (&markers.live_post_system, args.live_post_system_marker_hz),
                ] {
                    wav_analysis::write_tone(path, frequency, args.marker_seconds)?;
                }
            }
            Mode::Degraded => {}
        }

        let client = reqwest::Client::new();
        get_json(&client, paths::VERSION)
            .await
            .context("Audetic daemon is not reachable")?;
        let daemon_pid_before = daemon_pid()?;
        let baseline_logs = app_logs(&client).await?;
        let mut holder = AggregateHolder::start(&args, &root, &run_dir, &run_id)?;
        let monitor = StatusMonitor::start(client.clone());
        let mut report = Report {
            run_id: run_id.clone(),
            mode: format!("{:?}", args.mode).to_lowercase(),
            daemon_pid_before,
            daemon_pid_after: None,
            aggregate: holder.ready.clone(),
            status_transitions: Vec::new(),
            capture_telemetry: Vec::new(),
            artifacts: Vec::new(),
            assertions: Vec::new(),
            operator_notes: vec![
                "Marker assertions prove frequency content in the final mix, not electrical isolation; use headphones for the system marker.".to_string(),
            ],
        };

        let flow_result = match args.mode {
            Mode::Idle => {
                run_idle(
                    &args,
                    &client,
                    &mut holder,
                    &run_dir,
                    &markers.idle_mic,
                    &mut report,
                )
                .await
            }
            Mode::Live => {
                run_live(&args, &client, &mut holder, &run_dir, &markers, &mut report).await
            }
            Mode::Degraded => {
                run_degraded(&args, &client, &mut holder, &run_dir, &mut report).await
            }
        };

        if flow_result.is_err() {
            match stop_active_captures(&client).await {
                Ok(()) => report
                    .operator_notes
                    .push("Stopped active captures after the flow failed".to_string()),
                Err(error) => report.operator_notes.push(format!(
                    "Best-effort active-capture cleanup failed: {error:#}"
                )),
            }
        }

        if let Err(error) = holder.restore(None) {
            report.assertion("restore-defaults", false, format!("{error:#}"));
        } else {
            report.assertion(
                "restore-defaults",
                true,
                "original CoreAudio defaults restored",
            );
        }
        if let Err(error) = holder.destroy() {
            report.assertion("aggregate-destroyed", false, format!("{error:#}"));
        } else {
            report.assertion(
                "aggregate-destroyed",
                true,
                "holder confirmed aggregate destruction",
            );
        }
        report.status_transitions = monitor.stop().await;
        let current_logs = app_logs(&client).await.unwrap_or_default();
        let run_logs = match suffix_after(&baseline_logs, &current_logs) {
            Some(logs) => {
                report.assertion(
                    "daemon-log-window",
                    true,
                    "run-start log anchor remained available",
                );
                logs
            }
            None => {
                report.assertion(
                    "daemon-log-window",
                    false,
                    "run-start log anchor aged out; historical logs were not reused",
                );
                Vec::new()
            }
        };
        fs::write(run_dir.join("daemon.log"), run_logs.join("\n") + "\n")?;
        report.capture_telemetry = run_logs
            .iter()
            .filter(|line| {
                line.contains("audio_")
                    || line.contains(" using device: ")
                    || line.contains("System Tap using Default Output")
            })
            .cloned()
            .collect();

        for expected_rate in &args.expect_native_rates {
            let needle = format!("native_rate_hz={expected_rate}");
            report.assertion(
                &format!("native-rate-{expected_rate}"),
                report
                    .capture_telemetry
                    .iter()
                    .any(|line| line.contains(&needle)),
                format!("expected capture telemetry containing {needle}"),
            );
        }
        let daemon_pid_after = daemon_pid().ok();
        report.daemon_pid_after = daemon_pid_after;
        report.assertion(
            "daemon-pid-stable",
            daemon_pid_after == Some(daemon_pid_before),
            format!("before={daemon_pid_before}, after={daemon_pid_after:?}"),
        );
        if let Err(error) = flow_result {
            report.assertion("flow-completed", false, format!("{error:#}"));
        } else {
            report.assertion("flow-completed", true, "operator flow reached cleanup");
        }

        write_report(&run_dir, &report)?;
        println!("Reports written to {}", run_dir.display());
        let failures = report.assertions.iter().filter(|item| !item.passed).count();
        if failures > 0 {
            bail!("{failures} harness assertion(s) failed; inspect report.txt");
        }
        Ok(())
    }

    fn validate_args(args: &Args) -> Result<()> {
        anyhow::ensure!(
            args.marker_seconds.is_finite() && args.marker_seconds > 0.0,
            "marker-seconds must be positive"
        );
        let frequencies = match args.mode {
            Mode::Idle => vec![args.idle_mic_marker_hz],
            Mode::Live => vec![
                args.live_pre_mic_marker_hz,
                args.live_post_mic_marker_hz,
                args.live_pre_system_marker_hz,
                args.live_post_system_marker_hz,
            ],
            Mode::Degraded => Vec::new(),
        };
        anyhow::ensure!(
            frequencies
                .iter()
                .all(|frequency| frequency.is_finite() && *frequency > 0.0 && *frequency < 8_000.0),
            "marker frequencies must be between 0 and 8000 Hz"
        );
        for (index, frequency) in frequencies.iter().enumerate() {
            anyhow::ensure!(
                frequencies[index + 1..]
                    .iter()
                    .all(|other| frequency.to_bits() != other.to_bits()),
                "every live marker frequency must be distinct"
            );
        }
        Ok(())
    }

    async fn run_idle(
        args: &Args,
        client: &reqwest::Client,
        holder: &mut AggregateHolder,
        run_dir: &Path,
        mic_marker: &Path,
        report: &mut Report,
    ) -> Result<()> {
        ensure_dictation_idle(client).await?;
        let switch_logs = app_logs(client).await?;
        holder.set_input()?;
        let settled = wait_for_new_logs(
            client,
            &switch_logs,
            &[
                "audio_device_switch_settled".to_string(),
                "input_changed=true".to_string(),
            ],
            args.timeout_secs,
        )
        .await?
        .is_some();
        report.assertion(
            "idle-switch-settled",
            settled,
            "Default Input changed before dictation",
        );
        anyhow::ensure!(
            settled,
            "Default Input switch did not settle before dictation"
        );

        let before_start = app_logs(client).await?;
        start_dictation(client, args.timeout_secs).await?;
        let aggregate_seen = wait_for_new_logs(
            client,
            &before_start,
            &[format!("Dictation using device: {}", holder.ready.name)],
            args.timeout_secs,
        )
        .await?
        .is_some();
        report.assertion(
            "idle-new-device-opened",
            aggregate_seen,
            format!("dictation log names {}", holder.ready.name),
        );
        anyhow::ensure!(aggregate_seen, "dictation did not open the run aggregate");
        prompt(&format!(
            "Play the {} Hz marker into the physical microphone from an external device, then press Return. Marker file: {}",
            args.idle_mic_marker_hz,
            mic_marker.display()
        ))?;
        let artifact = stop_dictation(
            client,
            run_dir,
            args.timeout_secs,
            "idle-dictation",
            &[("idle_mic", args.idle_mic_marker_hz)],
        )
        .await?;
        marker_assertion(report, &artifact, "idle_mic", args.idle_mic_marker_hz);
        report.artifacts.push(artifact);
        Ok(())
    }

    async fn run_live(
        args: &Args,
        client: &reqwest::Client,
        holder: &mut AggregateHolder,
        run_dir: &Path,
        markers: &MarkerFiles,
        report: &mut Report,
    ) -> Result<()> {
        ensure_dictation_idle(client).await?;
        start_dictation(client, args.timeout_secs).await?;
        let meeting_path = start_meeting(client, &report.run_id, args.timeout_secs).await?;

        capture_live_marker_phase(
            report,
            "pre",
            args.live_pre_mic_marker_hz,
            args.live_pre_system_marker_hz,
            &markers.live_pre_mic,
            &markers.live_pre_system,
        )?;

        let switch_logs = app_logs(client).await?;
        holder.set_input()?;
        holder.set_output()?;
        let opened = wait_for_new_logs(
            client,
            &switch_logs,
            &[
                format!("Dictation using device: {}", holder.ready.name),
                format!("Meeting microphone using device: {}", holder.ready.name),
                format!("System Tap using Default Output: {}", holder.ready.name),
            ],
            args.timeout_secs,
        )
        .await?
        .is_some();
        report.assertion(
            "live-three-capture-legs-switched",
            opened,
            "dictation, meeting microphone, and System Tap opened the aggregate",
        );
        anyhow::ensure!(
            opened,
            "all three replacement capture legs were not observed"
        );

        capture_live_marker_phase(
            report,
            "post",
            args.live_post_mic_marker_hz,
            args.live_post_system_marker_hz,
            &markers.live_post_mic,
            &markers.live_post_system,
        )?;

        let dictation = stop_dictation(
            client,
            run_dir,
            args.timeout_secs,
            "live-dictation",
            &[
                ("mic_pre", args.live_pre_mic_marker_hz),
                ("mic_post", args.live_post_mic_marker_hz),
            ],
        )
        .await?;
        marker_assertion(report, &dictation, "mic_pre", args.live_pre_mic_marker_hz);
        marker_assertion(report, &dictation, "mic_post", args.live_post_mic_marker_hz);
        report.artifacts.push(dictation);
        stop_meeting(client, args.timeout_secs).await?;
        let meeting = copy_and_analyze(
            &meeting_path,
            &run_dir.join("live-meeting.wav"),
            &[
                ("mic_pre", args.live_pre_mic_marker_hz),
                ("mic_post", args.live_post_mic_marker_hz),
                ("system_pre", args.live_pre_system_marker_hz),
                ("system_post", args.live_post_system_marker_hz),
            ],
        )?;
        marker_assertion(report, &meeting, "mic_pre", args.live_pre_mic_marker_hz);
        marker_assertion(report, &meeting, "mic_post", args.live_post_mic_marker_hz);
        marker_assertion(
            report,
            &meeting,
            "system_pre",
            args.live_pre_system_marker_hz,
        );
        marker_assertion(
            report,
            &meeting,
            "system_post",
            args.live_post_system_marker_hz,
        );
        duration_assertion(report, &meeting, args);
        report.artifacts.push(meeting);
        Ok(())
    }

    fn capture_live_marker_phase(
        report: &mut Report,
        phase: &str,
        mic_frequency_hz: f64,
        system_frequency_hz: f64,
        mic_marker: &Path,
        system_marker: &Path,
    ) -> Result<()> {
        prompt(&format!(
            "Start the {phase}-switch external microphone marker at {mic_frequency_hz} Hz, then press Return. Use headphones for Mac output. Marker file: {}",
            mic_marker.display()
        ))?;
        let playback = Command::new("afplay").arg(system_marker).status()?;
        report.assertion(
            &format!("live-{phase}-system-marker-played"),
            playback.success(),
            format!(
                "afplay {} ({system_frequency_hz} Hz)",
                system_marker.display()
            ),
        );
        prompt(&format!(
            "Stop the {phase}-switch external microphone marker and press Return"
        ))?;
        Ok(())
    }

    async fn run_degraded(
        args: &Args,
        client: &reqwest::Client,
        holder: &mut AggregateHolder,
        run_dir: &Path,
        report: &mut Report,
    ) -> Result<()> {
        ensure_dictation_idle(client).await?;
        let switch_logs = app_logs(client).await?;
        holder.set_input()?;
        let settled = wait_for_new_logs(
            client,
            &switch_logs,
            &[
                "audio_device_switch_settled".to_string(),
                "input_changed=true".to_string(),
            ],
            args.timeout_secs,
        )
        .await?
        .is_some();
        anyhow::ensure!(
            settled,
            "Default Input switch did not settle before degraded flow"
        );
        start_dictation(client, args.timeout_secs).await?;
        let meeting_path = start_meeting(client, &report.run_id, args.timeout_secs).await?;
        prompt(&format!(
            "Unplug or power off the aggregate's input subdevice '{}' ({}) while capture is active, then press Return",
            holder.ready.input.name, holder.ready.input.uid
        ))?;
        let dictation_degraded =
            wait_status(client, RECORDING_STATUS_PATH, args.timeout_secs, |status| {
                status["capture_degraded"] == true
            })
            .await?;
        let meeting_degraded =
            wait_status(client, MEETING_STATUS_PATH, args.timeout_secs, |status| {
                status["capture_degraded"] == true
            })
            .await?;
        report.assertion(
            "dictation-entered-degraded-capture",
            dictation_degraded,
            "recording status exposed capture_degraded=true",
        );
        report.assertion(
            "meeting-entered-degraded-capture",
            meeting_degraded,
            "meeting status exposed capture_degraded=true",
        );

        prompt(
            "Reconnect the physical input device, wait for macOS to show it, then press Return",
        )?;
        holder.restore(Some("input"))?;
        let dictation_recovered =
            wait_status(client, RECORDING_STATUS_PATH, args.timeout_secs, |status| {
                status["recording"] == true && status["capture_degraded"] == false
            })
            .await?;
        let meeting_recovered =
            wait_status(client, MEETING_STATUS_PATH, args.timeout_secs, |status| {
                status["active"] == true && status["capture_degraded"] == false
            })
            .await?;
        report.assertion(
            "dictation-recovered",
            dictation_recovered,
            "dictation recovered without daemon restart",
        );
        report.assertion(
            "meeting-recovered",
            meeting_recovered,
            "meeting microphone recovered without daemon restart",
        );

        let dictation = stop_dictation(
            client,
            run_dir,
            args.timeout_secs,
            "degraded-dictation",
            &[],
        )
        .await?;
        report.artifacts.push(dictation);
        stop_meeting(client, args.timeout_secs).await?;
        report.artifacts.push(copy_and_analyze(
            &meeting_path,
            &run_dir.join("degraded-meeting.wav"),
            &[],
        )?);
        Ok(())
    }

    async fn ensure_dictation_idle(client: &reqwest::Client) -> Result<()> {
        let status = get_json(client, RECORDING_STATUS_PATH).await?;
        anyhow::ensure!(
            status["phase"] == "idle" || status["phase"] == "error",
            "dictation must be idle before the harness starts: {status}"
        );
        let meeting = get_json(client, MEETING_STATUS_PATH).await?;
        anyhow::ensure!(
            meeting["active"] == false,
            "a meeting is already active: {meeting}"
        );
        Ok(())
    }

    async fn stop_active_captures(client: &reqwest::Client) -> Result<()> {
        if get_json(client, RECORDING_STATUS_PATH).await?["recording"] == true {
            post_json(
                client,
                paths::TOGGLE,
                json!({ "copy_to_clipboard": false, "auto_paste": false }),
            )
            .await?;
        }
        if get_json(client, MEETING_STATUS_PATH).await?["active"] == true {
            post_json(client, MEETING_STOP_PATH, json!({})).await?;
        }
        Ok(())
    }

    async fn start_dictation(client: &reqwest::Client, timeout_secs: u64) -> Result<()> {
        post_json(
            client,
            paths::TOGGLE,
            json!({ "copy_to_clipboard": false, "auto_paste": false }),
        )
        .await?;
        anyhow::ensure!(
            wait_status(client, RECORDING_STATUS_PATH, timeout_secs, |status| {
                status["recording"] == true
            })
            .await?,
            "dictation did not enter recording phase"
        );
        Ok(())
    }

    async fn stop_dictation(
        client: &reqwest::Client,
        run_dir: &Path,
        timeout_secs: u64,
        label: &str,
        markers: &[(&str, f64)],
    ) -> Result<Artifact> {
        let before_stop = app_logs(client).await?;
        post_json(
            client,
            paths::TOGGLE,
            json!({ "copy_to_clipboard": false, "auto_paste": false }),
        )
        .await?;
        let stop_logs = wait_for_new_logs(
            client,
            &before_stop,
            &["Audio saved to:".to_string()],
            timeout_secs,
        )
        .await?
        .context("dictation WAV path was not logged")?;
        let source = stop_logs
            .iter()
            .rev()
            .find_map(|line| quoted_path_after(line, "Audio saved to: "))
            .context("could not parse dictation WAV path from daemon logs")?;
        copy_and_analyze(&source, &run_dir.join(format!("{label}.wav")), markers)
            .context("copy dictation WAV; set behavior.delete_audio_files=false for harness runs")
    }

    async fn start_meeting(
        client: &reqwest::Client,
        run_id: &str,
        timeout_secs: u64,
    ) -> Result<PathBuf> {
        let response = post_json(
            client,
            MEETING_START_PATH,
            json!({ "title": format!("CoreAudio harness {run_id}") }),
        )
        .await?;
        anyhow::ensure!(
            wait_status(client, MEETING_STATUS_PATH, timeout_secs, |status| {
                status["active"] == true
            })
            .await?,
            "meeting did not enter recording phase"
        );
        response["audio_path"]
            .as_str()
            .map(PathBuf::from)
            .context("meeting start response omitted audio_path")
    }

    async fn stop_meeting(client: &reqwest::Client, timeout_secs: u64) -> Result<()> {
        post_json(client, MEETING_STOP_PATH, json!({})).await?;
        anyhow::ensure!(
            wait_status(client, MEETING_STATUS_PATH, timeout_secs, |status| {
                status["phase"] == "review"
            })
            .await?,
            "meeting did not enter review phase"
        );
        Ok(())
    }

    fn copy_and_analyze(
        source: &Path,
        destination: &Path,
        markers: &[(&str, f64)],
    ) -> Result<Artifact> {
        fs::copy(source, destination).with_context(|| {
            format!(
                "failed to copy {} to {}",
                source.display(),
                destination.display()
            )
        })?;
        Ok(Artifact {
            label: destination
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
            path: destination.to_path_buf(),
            metrics: wav_analysis::analyze(destination, markers)?,
        })
    }

    fn marker_assertion(report: &mut Report, artifact: &Artifact, marker: &str, frequency: f64) {
        let amplitude = artifact
            .metrics
            .marker_amplitudes
            .get(marker)
            .copied()
            .unwrap_or_default();
        report.assertion(
            &format!("{}-{marker}-marker", artifact.label),
            amplitude >= MARKER_MINIMUM_AMPLITUDE,
            format!("{frequency} Hz amplitude={amplitude:.5} (minimum {MARKER_MINIMUM_AMPLITUDE})"),
        );
    }

    fn duration_assertion(report: &mut Report, artifact: &Artifact, args: &Args) {
        if let Some(expected) = args.expected_duration_secs {
            let difference = (artifact.metrics.duration_seconds - expected).abs();
            report.assertion(
                &format!("{}-duration", artifact.label),
                difference <= args.duration_tolerance_secs,
                format!(
                    "expected={expected:.3}s actual={:.3}s tolerance={:.3}s",
                    artifact.metrics.duration_seconds, args.duration_tolerance_secs
                ),
            );
        }
    }

    async fn get_json(client: &reqwest::Client, path: &str) -> Result<Value> {
        Ok(client
            .get(api_url(path))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?)
    }

    async fn post_json(client: &reqwest::Client, path: &str, body: Value) -> Result<Value> {
        Ok(client
            .post(api_url(path))
            .json(&body)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?)
    }

    async fn wait_status(
        client: &reqwest::Client,
        path: &str,
        timeout_secs: u64,
        predicate: impl Fn(&Value) -> bool,
    ) -> Result<bool> {
        let deadline = Instant::now() + Duration::from_secs(timeout_secs);
        while Instant::now() < deadline {
            if let Ok(status) = get_json(client, path).await {
                if predicate(&status) {
                    return Ok(true);
                }
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        Ok(false)
    }

    async fn app_logs(client: &reqwest::Client) -> Result<Vec<String>> {
        let value: Value = client
            .get(api_url(paths::LOGS))
            .query(&[("lines", 10_000_usize)])
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(value["app_logs"]
            .as_array()
            .context("logs response omitted app_logs")?
            .iter()
            .filter_map(|line| line.as_str().map(str::to_owned))
            .collect())
    }

    async fn wait_for_new_logs(
        client: &reqwest::Client,
        baseline: &[String],
        required: &[String],
        timeout_secs: u64,
    ) -> Result<Option<Vec<String>>> {
        let deadline = Instant::now() + Duration::from_secs(timeout_secs);
        while Instant::now() < deadline {
            let current = app_logs(client).await?;
            if let Some(new_logs) = suffix_after(baseline, &current) {
                if required
                    .iter()
                    .all(|needle| new_logs.iter().any(|line| line.contains(needle)))
                {
                    return Ok(Some(new_logs));
                }
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        Ok(None)
    }

    fn quoted_path_after(line: &str, marker: &str) -> Option<PathBuf> {
        let suffix = line.split_once(marker)?.1;
        let start = suffix.find('"')? + 1;
        let end = suffix[start..].find('"')? + start;
        Some(PathBuf::from(&suffix[start..end]))
    }

    fn daemon_pid() -> Result<u32> {
        let output = Command::new("lsof")
            .args(["-nP", "-iTCP:3737", "-sTCP:LISTEN", "-t"])
            .output()
            .context("failed to run lsof")?;
        anyhow::ensure!(
            output.status.success(),
            "no daemon is listening on TCP 3737"
        );
        String::from_utf8(output.stdout)?
            .lines()
            .next()
            .context("lsof returned no PID")?
            .parse()
            .context("invalid daemon PID from lsof")
    }

    fn prompt(message: &str) -> Result<()> {
        println!("\n{message}");
        print!("> ");
        std::io::stdout().flush()?;
        let mut response = String::new();
        std::io::stdin().read_line(&mut response)?;
        Ok(())
    }

    fn write_report(run_dir: &Path, report: &Report) -> Result<()> {
        fs::write(
            run_dir.join("report.json"),
            serde_json::to_vec_pretty(report)?,
        )?;
        let mut text = format!(
            "CoreAudio hot-swap harness\nrun: {}\nmode: {}\ndaemon PID: {} -> {:?}\naggregate: {} ({})\n\nAssertions\n",
            report.run_id,
            report.mode,
            report.daemon_pid_before,
            report.daemon_pid_after,
            report.aggregate.name,
            report.aggregate.uid,
        );
        for assertion in &report.assertions {
            text.push_str(&format!(
                "[{}] {}: {}\n",
                if assertion.passed { "PASS" } else { "FAIL" },
                assertion.name,
                assertion.detail
            ));
        }
        text.push_str("\nArtifacts\n");
        for artifact in &report.artifacts {
            text.push_str(&format!(
                "{}: {} frames @ {} Hz ({:.3}s), peak {:.5}, rms {:.5}, silence gaps {}, longest {} frames, markers {:?}\n",
                artifact.label,
                artifact.metrics.frames,
                artifact.metrics.sample_rate_hz,
                artifact.metrics.duration_seconds,
                artifact.metrics.peak,
                artifact.metrics.rms,
                artifact.metrics.silence_gaps,
                artifact.metrics.longest_silence_frames,
                artifact.metrics.marker_amplitudes,
            ));
        }
        text.push_str(&format!(
            "\nStatus transitions: {} (see report.json)\nCapture telemetry: {} lines (see report.json and daemon.log)\n",
            report.status_transitions.len(),
            report.capture_telemetry.len()
        ));
        fs::write(run_dir.join("report.txt"), text)?;
        Ok(())
    }
}

#[cfg(target_os = "macos")]
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    macos::main().await
}

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!(
        "coreaudio_hot_swap is a manual macOS hardware harness; WAV analysis tests remain available on this platform"
    );
}
