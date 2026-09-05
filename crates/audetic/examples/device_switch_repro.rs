//! Manual, real-CoreAudio reproduction harness for Fizzy #113.
//!
//! This drives an already-installed Audetic launch daemon over HTTP and uses
//! `target/tools/audiodev` to create disposable CoreAudio aggregate devices.
//! It is intentionally a manual harness: unit tests cover signal generation,
//! analysis, redaction, and log parsing without requiring audio hardware.

use anyhow::{anyhow, bail, Context, Result};
use clap::{Parser, ValueEnum};
use regex::Regex;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::time::{sleep, timeout, Duration, Instant};

use audetic_core::config::Config;
use audetic_core::url::{api_url, paths};
use chrono::{SecondsFormat, Utc};
use hound::{SampleFormat, WavReader, WavSpec, WavWriter};
use uuid::Uuid;

use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::Stdio;

const CAPTURE_RATE: u32 = 16_000;
const SOURCE_RATE: u32 = 48_000;
const MARKER_A_HZ: f64 = 697.0;
const MARKER_B_HZ: f64 = 1_009.0;
const REFERENCE_HZ: f64 = 311.0;
const LEADING_SILENCE_SECONDS: f64 = 2.0;
const SWITCH_GAP_SECONDS: f64 = 2.0;
const STATUS_POLL: Duration = Duration::from_millis(100);
const MIN_TONE_AMPLITUDE: f64 = 0.002;
const MIN_CAPTURE_RMS: f64 = 0.004;
const MIN_CAPTURE_PEAK: f64 = 0.02;

#[derive(Debug, Clone, Copy, Serialize, ValueEnum, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
enum Mode {
    Preflight,
    Idle,
    LiveDictation,
    LiveMeetingMic,
    LiveMeetingSystem,
    Degraded,
    Churn,
    All,
}

#[derive(Debug, Parser)]
#[command(about = "Drive Audetic device-switch recovery against real CoreAudio")]
struct Cli {
    #[arg(value_enum, default_value_t = Mode::Preflight)]
    mode: Mode,

    #[arg(long, default_value = "target/device-switch-runs")]
    artifacts_root: PathBuf,

    #[arg(long, default_value = "target/tools/audiodev")]
    helper: PathBuf,

    #[arg(long, default_value_t = 1.5)]
    marker_seconds: f64,

    #[arg(long, default_value_t = 90.0)]
    settle_timeout_seconds: f64,

    #[arg(long)]
    physical_output_uid: Option<String>,

    #[arg(long)]
    physical_input_a_uid: Option<String>,

    #[arg(long)]
    physical_input_b_uid: Option<String>,
}

#[derive(Debug, Serialize)]
struct Manifest {
    run_uid: String,
    mode: Mode,
    started_at: String,
    finished_at: Option<String>,
    outcome: String,
    artifact_dir: String,
    helper_path: String,
    daemon_pid_start: Option<u32>,
    daemon_pid_end: Option<u32>,
    original_devices: Vec<DeviceIdentity>,
    test_devices: Vec<DeviceIdentity>,
    native_rates_hz: Vec<NativeRate>,
    timestamps: Vec<TimestampRecord>,
    assertions: Vec<AssertionRecord>,
    status_transitions: Vec<StatusTransition>,
    capture_events: Vec<CaptureEvent>,
    artifacts: Vec<String>,
    mixed_native_rates_observed: bool,
    error: Option<String>,
}

#[derive(Debug, Serialize)]
struct DeviceIdentity {
    role: String,
    name: String,
    uid_redacted: String,
}

#[derive(Debug, Serialize)]
struct NativeRate {
    scenario: String,
    device: String,
    rate_hz: f64,
}

#[derive(Debug, Serialize)]
struct TimestampRecord {
    scenario: String,
    event: String,
    at: String,
}

#[derive(Debug, Serialize)]
struct AssertionRecord {
    scenario: String,
    assertion: String,
    passed: bool,
    details: String,
}

#[derive(Debug, Serialize)]
struct StatusTransition {
    endpoint: String,
    observed_at: String,
    status: Value,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
struct CaptureEvent {
    scenario: String,
    timestamp: Option<String>,
    event: String,
    source: Option<String>,
    stream_generation: Option<u64>,
    native_samples: Option<u64>,
    native_sample_rate_hz: Option<u32>,
    native_rms: Option<f64>,
    native_peak: Option<f64>,
    canonical_samples: Option<u64>,
    gap_milliseconds: Option<u64>,
    input_changed: Option<bool>,
    output_changed: Option<bool>,
}

impl Manifest {
    fn new(cli: &Cli, run_uid: &str, artifact_dir: &Path, helper: &Path) -> Self {
        Self {
            run_uid: run_uid.to_string(),
            mode: cli.mode,
            started_at: now(),
            finished_at: None,
            outcome: "running".to_string(),
            artifact_dir: artifact_dir.display().to_string(),
            helper_path: helper.display().to_string(),
            daemon_pid_start: None,
            daemon_pid_end: None,
            original_devices: Vec::new(),
            test_devices: Vec::new(),
            native_rates_hz: Vec::new(),
            timestamps: Vec::new(),
            assertions: Vec::new(),
            status_transitions: Vec::new(),
            capture_events: Vec::new(),
            artifacts: Vec::new(),
            mixed_native_rates_observed: false,
            error: None,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Direction {
    Input,
    Output,
}

#[derive(Debug, Default, Serialize)]
struct HelperRequest {
    id: String,
    op: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    uid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    device_uid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    direction: Option<Direction>,
    #[serde(skip_serializing_if = "Option::is_none")]
    rate: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    mono: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    process_pid: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    muted: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct HelperResponse {
    #[serde(rename = "type")]
    response_type: String,
    id: Option<String>,
    op: Option<String>,
    ok: bool,
    result: Option<HelperResult>,
    error: Option<HelperError>,
}

#[derive(Debug, Default, Clone, Deserialize)]
struct HelperResult {
    protocol_version: Option<u32>,
    capabilities: Option<Vec<String>>,
    devices: Option<Vec<DeviceRecord>>,
    default_input_uid: Option<String>,
    default_output_uid: Option<String>,
    resource: Option<ResourceRecord>,
}

#[derive(Debug, Clone, Deserialize)]
struct DeviceRecord {
    uid: String,
    name: String,
    input_channels: u32,
    output_channels: u32,
    nominal_rate: Option<f64>,
    available_rates: Vec<RateRange>,
    alive: bool,
}

#[derive(Debug, Clone, Deserialize)]
struct RateRange {
    minimum: f64,
    maximum: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct OutputVolume {
    level: u8,
    muted: bool,
}

#[derive(Debug, Clone, Deserialize)]
struct ResourceRecord {
    uid: String,
    id: u32,
    nominal_rate: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct HelperError {
    code: String,
    message: String,
    os_status: Option<i32>,
    fourcc: Option<String>,
}

struct Helper {
    child: Child,
    stdin: ChildStdin,
    stdout: tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
    command_timeout: Duration,
}

impl Helper {
    async fn spawn(path: &Path, stderr_path: &Path, command_timeout: Duration) -> Result<Self> {
        let stderr = File::create(stderr_path)
            .with_context(|| format!("create {}", stderr_path.display()))?;
        let mut child = Command::new(path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::from(stderr))
            .kill_on_drop(true)
            .spawn()
            .with_context(|| format!("start helper {}", path.display()))?;
        let stdin = child.stdin.take().context("helper stdin was not piped")?;
        let stdout = child.stdout.take().context("helper stdout was not piped")?;
        Ok(Self {
            child,
            stdin,
            stdout: BufReader::new(stdout).lines(),
            command_timeout,
        })
    }

    async fn call(&mut self, mut request: HelperRequest) -> Result<HelperResult> {
        if request.id.is_empty() {
            request.id = Uuid::new_v4().to_string();
        }
        let expected_id = request.id.clone();
        let expected_op = request.op.clone();
        let mut encoded = serde_json::to_vec(&request).context("encode helper request")?;
        encoded.push(b'\n');
        timeout(self.command_timeout, async {
            self.stdin.write_all(&encoded).await?;
            self.stdin.flush().await
        })
        .await
        .context("helper request write timed out")??;

        let deadline = Instant::now() + self.command_timeout;
        let response = loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                bail!("helper response timed out");
            }
            let line = timeout(remaining, self.stdout.next_line())
                .await
                .context("helper response timed out")??
                .context("helper exited before responding")?;
            let response: HelperResponse =
                serde_json::from_str(&line).context("decode helper JSON response")?;
            if response.response_type != "response" {
                bail!("helper returned an invalid response type for {expected_op}");
            }
            if response.id.as_deref() == Some(expected_id.as_str())
                && response.op.as_deref() == Some(expected_op.as_str())
            {
                break response;
            }
            // A cancelled in-flight command can leave one response queued.
            // Drain it so Ctrl-C cleanup can still match restore/shutdown.
        };
        if !response.ok {
            let error = response
                .error
                .context("helper failure omitted error payload")?;
            bail!(
                "helper {} failed: {}: {} (os_status={:?}, fourcc={:?})",
                expected_op,
                error.code,
                error.message,
                error.os_status,
                error.fourcc
            );
        }
        response
            .result
            .context("helper success omitted result payload")
    }

    async fn op(&mut self, op: &str) -> Result<HelperResult> {
        self.call(HelperRequest {
            op: op.to_string(),
            ..Default::default()
        })
        .await
    }

    async fn cleanup(&mut self) -> Result<()> {
        let mut errors = Vec::new();
        if let Err(error) = self.op("restore").await {
            errors.push(format!("restore: {error:#}"));
        }
        if let Err(error) = self.op("shutdown").await {
            errors.push(format!("shutdown: {error:#}"));
        }
        match timeout(Duration::from_secs(3), self.child.wait()).await {
            Ok(Ok(_)) => {}
            Ok(Err(error)) => errors.push(format!("wait helper: {error}")),
            Err(_) => {
                let _ = self.child.start_kill();
                match timeout(Duration::from_secs(2), self.child.wait()).await {
                    Ok(Ok(_)) => {}
                    Ok(Err(error)) => errors.push(format!("reap helper: {error}")),
                    Err(_) => errors.push("helper did not exit after kill".to_string()),
                }
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            bail!(errors.join("; "))
        }
    }
}

impl Drop for Helper {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
    }
}

struct Player {
    child: Child,
    started: Instant,
    expected_duration: Duration,
    reaped: bool,
}

impl Player {
    async fn stop(&mut self) -> Result<()> {
        if self.reaped {
            return Ok(());
        }
        if self.child.try_wait()?.is_none() {
            self.child.start_kill().context("kill afplay")?;
        }
        timeout(Duration::from_secs(3), self.child.wait())
            .await
            .context("afplay reap timed out")??;
        self.reaped = true;
        Ok(())
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        if !self.reaped {
            let _ = self.child.start_kill();
        }
    }
}

#[derive(Debug)]
struct Timeline {
    samples: Vec<f32>,
    marker_a_start: f64,
    marker_b_start: f64,
    duration: f64,
}

#[derive(Debug, Clone, Serialize)]
struct ToneFinding {
    expected_hz: f64,
    best_offset_seconds: f64,
    amplitude: f64,
    window_rms: f64,
    frequency_estimate_hz: f64,
}

#[derive(Debug, Serialize)]
struct CaptureAnalysis {
    sample_rate: u32,
    channels: u16,
    sample_count: usize,
    duration_seconds: f64,
    rms: f64,
    peak: f64,
    marker_a: ToneFinding,
    marker_b: ToneFinding,
    reference: Option<ToneFinding>,
    reference_min_amplitude: Option<f64>,
    marker_band_gap_max: Option<f64>,
}

#[derive(Debug, Serialize)]
struct SingleCaptureAnalysis {
    sample_rate: u32,
    channels: u16,
    sample_count: usize,
    duration_seconds: f64,
    rms: f64,
    peak: f64,
    marker: ToneFinding,
}

struct Harness {
    cli: Cli,
    run_uid: String,
    artifact_dir: PathBuf,
    manifest_path: PathBuf,
    manifest: Manifest,
    client: Client,
    helper: Option<Helper>,
    players: Vec<Player>,
    snapshot: Option<HelperResult>,
    original_input_uid: Option<String>,
    original_output_uid: Option<String>,
    physical_output_uid: Option<String>,
    physical_input_a_uid: Option<String>,
    physical_input_b_uid: Option<String>,
    physical_input_a_rate: Option<f64>,
    physical_input_b_rate: Option<f64>,
    daemon_log: PathBuf,
    daemon_log_start: u64,
    secrets: Vec<String>,
    original_output_volume: Option<OutputVolume>,
    test_output_volume: Option<OutputVolume>,
    owns_dictation: bool,
    owns_meeting: bool,
}

impl Harness {
    fn create(cli: Cli) -> Result<Self> {
        if !cli.marker_seconds.is_finite() || cli.marker_seconds < 0.5 {
            bail!("--marker-seconds must be finite and at least 0.5");
        }
        if !cli.settle_timeout_seconds.is_finite() || cli.settle_timeout_seconds < 2.0 {
            bail!("--settle-timeout-seconds must be finite and at least 2");
        }
        let cwd = std::env::current_dir().context("resolve current directory")?;
        let root = if cli.artifacts_root.is_absolute() {
            cli.artifacts_root.clone()
        } else {
            cwd.join(&cli.artifacts_root)
        };
        fs::create_dir_all(&root)
            .with_context(|| format!("create artifact root {}", root.display()))?;
        let run_uid = Uuid::new_v4().to_string();
        let stamp = Utc::now().format("%Y%m%dT%H%M%S%.3fZ");
        let artifact_dir = root.join(format!("{stamp}-{run_uid}"));
        fs::create_dir(&artifact_dir)
            .with_context(|| format!("create run directory {}", artifact_dir.display()))?;
        let artifact_dir = fs::canonicalize(&artifact_dir).context("canonicalize run directory")?;
        let helper = if cli.helper.is_absolute() {
            cli.helper.clone()
        } else {
            cwd.join(&cli.helper)
        };
        let manifest_path = artifact_dir.join("manifest.json");
        let manifest = Manifest::new(&cli, &run_uid, &artifact_dir, &helper);
        let client = Client::builder()
            .timeout(Duration::from_secs(8))
            .build()
            .context("build HTTP client")?;
        let daemon_log = dirs::home_dir()
            .context("resolve home directory")?
            .join("Library/Logs/Audetic/audetic.log");
        let harness = Self {
            cli,
            run_uid,
            artifact_dir,
            manifest_path,
            manifest,
            client,
            helper: None,
            players: Vec::new(),
            snapshot: None,
            original_input_uid: None,
            original_output_uid: None,
            physical_output_uid: None,
            physical_input_a_uid: None,
            physical_input_b_uid: None,
            physical_input_a_rate: None,
            physical_input_b_rate: None,
            daemon_log,
            daemon_log_start: 0,
            secrets: Vec::new(),
            original_output_volume: None,
            test_output_volume: None,
            owns_dictation: false,
            owns_meeting: false,
        };
        harness.persist()?;
        Ok(harness)
    }

    fn persist(&self) -> Result<()> {
        let bytes = serde_json::to_vec_pretty(&self.manifest).context("serialize manifest")?;
        fs::write(&self.manifest_path, bytes)
            .with_context(|| format!("write {}", self.manifest_path.display()))
    }

    fn timestamp(&mut self, scenario: &str, event: &str) -> Result<()> {
        self.manifest.timestamps.push(TimestampRecord {
            scenario: scenario.to_string(),
            event: event.to_string(),
            at: now(),
        });
        self.persist()
    }

    fn assertion(
        &mut self,
        scenario: &str,
        assertion: &str,
        passed: bool,
        details: impl Into<String>,
    ) -> Result<()> {
        let details = details.into();
        self.manifest.assertions.push(AssertionRecord {
            scenario: scenario.to_string(),
            assertion: assertion.to_string(),
            passed,
            details: details.clone(),
        });
        self.persist()?;
        if passed {
            Ok(())
        } else {
            bail!("{scenario}: {assertion}: {details}")
        }
    }

    fn add_artifact(&mut self, path: &Path) -> Result<()> {
        let value = path
            .strip_prefix(&self.artifact_dir)
            .unwrap_or(path)
            .display()
            .to_string();
        if !self.manifest.artifacts.contains(&value) {
            self.manifest.artifacts.push(value);
            self.persist()?;
        }
        Ok(())
    }

    async fn execute(&mut self) -> Result<()> {
        #[cfg(not(target_os = "macos"))]
        bail!("device_switch_repro is unsupported on this OS; it requires macOS CoreAudio");

        #[cfg(target_os = "macos")]
        {
            self.preflight().await?;
            match self.cli.mode {
                Mode::Preflight => {}
                Mode::Idle => self.scenario_idle().await?,
                Mode::LiveDictation => self.scenario_live_dictation().await?,
                Mode::LiveMeetingMic => self.scenario_meeting_mic().await?,
                Mode::LiveMeetingSystem => self.scenario_meeting_system().await?,
                Mode::Degraded => self.scenario_degraded().await?,
                Mode::Churn => self.scenario_churn().await?,
                Mode::All => {
                    self.scenario_idle().await?;
                    self.scenario_live_dictation().await?;
                    self.scenario_meeting_mic().await?;
                    self.scenario_meeting_system().await?;
                    self.scenario_degraded().await?;
                    self.scenario_churn().await?;
                    self.assertion(
                        "all",
                        "at least one scenario observed mixed native rates",
                        self.manifest.mixed_native_rates_observed,
                        "requested 44.1 kHz and 48 kHz; topology may reject rate changes",
                    )?;
                }
            }
            Ok(())
        }
    }

    async fn preflight(&mut self) -> Result<()> {
        let scenario = "preflight";
        self.timestamp(scenario, "started")?;
        self.assertion(
            scenario,
            "afplay exists",
            Path::new("/usr/bin/afplay").is_file(),
            "/usr/bin/afplay is required",
        )?;
        let helper_path = PathBuf::from(&self.manifest.helper_path);
        self.assertion(
            scenario,
            "audiodev helper exists",
            helper_path.is_file(),
            helper_path.display().to_string(),
        )?;
        self.assertion(
            scenario,
            "daemon log exists",
            self.daemon_log.is_file(),
            self.daemon_log.display().to_string(),
        )?;
        self.daemon_log_start = fs::metadata(&self.daemon_log)?.len();

        let output_volume = read_output_volume(self.settle_timeout()).await?;
        self.assertion(
            scenario,
            "system output volume is readable",
            true,
            format!(
                "volume={}, muted={}",
                output_volume.level, output_volume.muted
            ),
        )?;
        if self.cli.mode != Mode::Preflight {
            let test_volume = OutputVolume {
                level: 40,
                muted: false,
            };
            self.original_output_volume = Some(output_volume);
            self.test_output_volume = Some(test_volume);
            if output_volume != test_volume {
                set_output_volume(test_volume, self.settle_timeout()).await?;
            }
            self.assertion(
                scenario,
                "acoustic marker output is 40% and unmuted",
                read_output_volume(self.settle_timeout()).await? == test_volume,
                "the original output volume is restored during cleanup",
            )?;
        }

        let config = Config::load().context("load installed Audetic config")?;
        self.assertion(
            scenario,
            "installed configuration is readable",
            true,
            format!(
                "delete_audio_files={} (the harness copies dictation audio concurrently with stop)",
                config.behavior.delete_audio_files
            ),
        )?;

        let recording = self
            .get_status(paths::RECORDING_STATUS, "recording")
            .await?;
        let meeting = self.get_status(paths::MEETINGS_STATUS, "meeting").await?;
        self.assertion(
            scenario,
            "recording status exposes capture_degraded",
            recording
                .get("capture_degraded")
                .and_then(Value::as_bool)
                .is_some(),
            recording.to_string(),
        )?;
        self.assertion(
            scenario,
            "meeting status exposes capture_degraded",
            meeting
                .get("capture_degraded")
                .and_then(Value::as_bool)
                .is_some(),
            meeting.to_string(),
        )?;
        self.assertion(
            scenario,
            "dictation is not active or processing",
            matches!(
                recording.get("phase").and_then(Value::as_str),
                Some("idle" | "error")
            ),
            recording.to_string(),
        )?;
        self.assertion(
            scenario,
            "meeting is idle",
            meeting.get("phase").and_then(Value::as_str) == Some("idle"),
            meeting.to_string(),
        )?;

        let pid = launchd_daemon_pid(self.settle_timeout()).await?;
        self.manifest.daemon_pid_start = Some(pid);
        self.persist()?;

        let stderr_path = self.artifact_dir.join("helper.stderr.log");
        self.helper = Some(
            Helper::spawn(&helper_path, &stderr_path, self.settle_timeout())
                .await
                .context("start long-lived audiodev helper")?,
        );
        self.add_artifact(&stderr_path)?;
        let hello = self.helper_mut()?.op("hello").await?;
        let capabilities: BTreeSet<_> =
            hello.capabilities.unwrap_or_default().into_iter().collect();
        let required = [
            "snapshot",
            "create_tap_aggregate",
            "create_subdevice_aggregate",
            "set_default",
            "hog_aggregate",
            "release_hog",
            "destroy_tap",
            "restore",
            "shutdown",
        ];
        self.assertion(
            scenario,
            "helper protocol and capabilities",
            hello.protocol_version == Some(1)
                && required.iter().all(|item| capabilities.contains(*item)),
            format!(
                "protocol={:?}, capabilities={capabilities:?}",
                hello.protocol_version
            ),
        )?;
        let snapshot = self.helper_mut()?.op("snapshot").await?;
        let input_uid = snapshot
            .default_input_uid
            .clone()
            .context("snapshot has no Default Input UID")?;
        let output_uid = snapshot
            .default_output_uid
            .clone()
            .context("snapshot has no Default Output UID")?;
        let devices = snapshot.devices.as_deref().unwrap_or_default();
        let input =
            device_by_uid(devices, &input_uid).context("Default Input absent from snapshot")?;
        let output =
            device_by_uid(devices, &output_uid).context("Default Output absent from snapshot")?;
        self.assertion(
            scenario,
            "default devices are alive and directional",
            input.alive && input.input_channels > 0 && output.alive && output.output_channels > 0,
            format!("input={}, output={}", input.name, output.name),
        )?;
        self.record_identity("original input", input);
        self.record_identity("original output", output);
        for (device, role) in [(input, "original input"), (output, "original output")] {
            if let Some(rate) = device.nominal_rate {
                self.manifest.native_rates_hz.push(NativeRate {
                    scenario: scenario.to_string(),
                    device: role.to_string(),
                    rate_hz: rate,
                });
            }
        }
        let physical_uid = self
            .cli
            .physical_output_uid
            .clone()
            .unwrap_or_else(|| output_uid.clone());
        let physical = device_by_uid(devices, &physical_uid)
            .context("--physical-output-uid is absent from helper snapshot")?;
        self.assertion(
            scenario,
            "selected physical output is alive and has output channels",
            physical.alive && physical.output_channels > 0,
            physical.name.clone(),
        )?;
        self.record_identity("selected physical output", physical);

        let input_a_uid = self
            .cli
            .physical_input_a_uid
            .clone()
            .unwrap_or_else(|| input_uid.clone());
        let input_a = device_by_uid(devices, &input_a_uid)
            .context("--physical-input-a-uid is absent from helper snapshot")?
            .clone();
        self.assertion(
            scenario,
            "physical input A is alive and has input channels",
            input_a.alive && input_a.input_channels > 0,
            input_a.name.clone(),
        )?;
        let input_b_uid = self
            .cli
            .physical_input_b_uid
            .clone()
            .unwrap_or_else(|| input_a_uid.clone());
        let input_b = device_by_uid(devices, &input_b_uid)
            .context("--physical-input-b-uid is absent from helper snapshot")?
            .clone();
        let input_b_rate = if self.cli.physical_input_b_uid.is_some() {
            input_b.nominal_rate
        } else {
            Some(
                input_a
                    .available_rates
                    .iter()
                    .filter(|range| {
                        (range.maximum - range.minimum).abs() < 0.01
                            && Some(range.maximum) != input_a.nominal_rate
                    })
                    .map(|range| range.maximum)
                    .max_by(f64::total_cmp)
                    .context(
                        "physical input A has no second discrete native rate; pass --physical-input-b-uid",
                    )?,
            )
        };
        self.assertion(
            scenario,
            "physical input B is alive and has input channels",
            input_b.alive && input_b.input_channels > 0,
            input_b.name.clone(),
        )?;
        self.assertion(
            scenario,
            "physical input rates differ",
            input_a.nominal_rate.is_some()
                && input_b_rate.is_some()
                && input_a.nominal_rate != input_b_rate,
            format!(
                "A={} {:?} Hz, B={} {:?} Hz",
                input_a.name, input_a.nominal_rate, input_b.name, input_b_rate
            ),
        )?;
        self.record_identity("selected physical input A", &input_a);
        self.record_identity("selected physical input B", &input_b);
        self.original_input_uid = Some(input_uid.clone());
        self.original_output_uid = Some(output_uid.clone());
        self.physical_output_uid = Some(physical_uid.clone());
        self.physical_input_a_uid = Some(input_a_uid.clone());
        self.physical_input_b_uid = Some(input_b_uid.clone());
        self.physical_input_a_rate = input_a.nominal_rate;
        self.physical_input_b_rate = input_b_rate;
        self.secrets.extend([
            input_uid,
            output_uid,
            physical_uid,
            input_a_uid,
            input_b_uid,
        ]);
        self.snapshot = Some(snapshot);
        self.persist()?;
        self.assert_daemon_unchanged(scenario).await?;
        self.timestamp(scenario, "passed")
    }

    fn record_identity(&mut self, role: &str, device: &DeviceRecord) {
        self.manifest.original_devices.push(DeviceIdentity {
            role: role.to_string(),
            name: device.name.clone(),
            uid_redacted: redact_uid(&device.uid),
        });
    }

    fn helper_mut(&mut self) -> Result<&mut Helper> {
        self.helper
            .as_mut()
            .context("audiodev helper is not running")
    }

    fn settle_timeout(&self) -> Duration {
        Duration::from_secs_f64(self.cli.settle_timeout_seconds)
    }

    async fn request_json(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<Value> {
        let mut request = self.client.request(method, api_url(path));
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request
            .send()
            .await
            .context("connect to installed Audetic daemon")?;
        let status = response.status();
        let text = response.text().await.context("read daemon response")?;
        if !status.is_success() {
            bail!("{} returned HTTP {}: {}", path, status, text);
        }
        serde_json::from_str(&text).with_context(|| format!("parse {path} response: {text}"))
    }

    async fn get_status(&mut self, path: &str, endpoint: &str) -> Result<Value> {
        let value = self.request_json(reqwest::Method::GET, path, None).await?;
        let evidence = status_evidence(endpoint, &value);
        let changed = self
            .manifest
            .status_transitions
            .iter()
            .rev()
            .find(|item| item.endpoint == endpoint)
            .is_none_or(|last| last.status != evidence);
        if changed {
            self.manifest.status_transitions.push(StatusTransition {
                endpoint: endpoint.to_string(),
                observed_at: now(),
                status: evidence,
            });
            self.persist()?;
        }
        Ok(value)
    }

    async fn wait_status<F>(
        &mut self,
        path: &str,
        endpoint: &str,
        description: &str,
        predicate: F,
    ) -> Result<Value>
    where
        F: Fn(&Value) -> bool,
    {
        let deadline = Instant::now() + self.settle_timeout();
        loop {
            let value = self.get_status(path, endpoint).await?;
            if predicate(&value) {
                return Ok(value);
            }
            if Instant::now() >= deadline {
                bail!("timed out waiting for {description}; last status: {value}");
            }
            sleep(STATUS_POLL).await;
        }
    }

    async fn poll_for(&mut self, duration: Duration, meeting: bool) -> Result<()> {
        let deadline = Instant::now() + duration;
        while Instant::now() < deadline {
            if meeting {
                self.get_status(paths::MEETINGS_STATUS, "meeting").await?;
            } else {
                self.get_status(paths::RECORDING_STATUS, "recording")
                    .await?;
            }
            sleep(STATUS_POLL.min(deadline.saturating_duration_since(Instant::now()))).await;
        }
        Ok(())
    }

    async fn post_toggle(&mut self) -> Result<Value> {
        self.request_json(
            reqwest::Method::POST,
            paths::TOGGLE,
            Some(json!({"copy_to_clipboard": false, "auto_paste": false})),
        )
        .await
    }

    async fn start_dictation(&mut self) -> Result<Value> {
        let result = self.post_toggle().await?;
        self.owns_dictation = true;
        Ok(result)
    }

    async fn meeting_start(&mut self, title: &str) -> Result<Value> {
        let result = self
            .request_json(
                reqwest::Method::POST,
                paths::MEETINGS_START,
                Some(json!({"title": title})),
            )
            .await?;
        self.owns_meeting = true;
        Ok(result)
    }

    async fn meeting_stop(&mut self) -> Result<Value> {
        self.request_json(reqwest::Method::POST, paths::MEETINGS_STOP, None)
            .await
    }

    async fn meeting_cancel(&mut self) -> Result<Value> {
        let result = self
            .request_json(reqwest::Method::POST, paths::MEETINGS_CANCEL, None)
            .await?;
        self.owns_meeting = false;
        Ok(result)
    }

    async fn spawn_player(&mut self, wav: &Path, duration_seconds: f64) -> Result<usize> {
        let child = Command::new("/usr/bin/afplay")
            .args(["-v", "2.0"])
            .arg(wav)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .with_context(|| format!("start afplay for {}", wav.display()))?;
        self.players.push(Player {
            child,
            started: Instant::now(),
            expected_duration: Duration::from_secs_f64(duration_seconds),
            reaped: false,
        });
        sleep(Duration::from_millis(150)).await;
        Ok(self.players.len() - 1)
    }

    async fn wait_until_player(
        &mut self,
        player: usize,
        offset_seconds: f64,
        meeting: bool,
    ) -> Result<()> {
        let target = self.players[player].started + Duration::from_secs_f64(offset_seconds);
        if Instant::now() >= target {
            return Ok(());
        }
        self.poll_for(target.saturating_duration_since(Instant::now()), meeting)
            .await
    }

    async fn finish_player(&mut self, player: usize, meeting: bool) -> Result<()> {
        let target = self.players[player].started + self.players[player].expected_duration;
        if Instant::now() < target {
            self.poll_for(target.saturating_duration_since(Instant::now()), meeting)
                .await?;
        }
        timeout(Duration::from_secs(4), self.players[player].child.wait())
            .await
            .context("afplay completion timed out")??;
        self.players[player].reaped = true;
        Ok(())
    }

    async fn stop_player(&mut self, player: usize) -> Result<()> {
        self.players[player].stop().await
    }

    fn uid(&mut self, scenario: &str, label: &str) -> String {
        let scenario = scenario.replace('-', "_");
        let uid = format!("com.audetic.repro.{}.{}.{}", self.run_uid, scenario, label);
        self.secrets.push(uid.clone());
        uid
    }

    fn resource_name(&self, scenario: &str, suffix: &str) -> String {
        let nonce = self.run_uid.split('-').next().unwrap_or(&self.run_uid);
        format!("Audetic Repro {nonce} {scenario} {suffix}")
    }

    async fn create_input_pair(
        &mut self,
        scenario: &str,
    ) -> Result<(ResourceRecord, ResourceRecord)> {
        let uid_a = self.uid(scenario, "a");
        let uid_b = self.uid(scenario, "b");
        let physical_a = self
            .physical_input_a_uid
            .clone()
            .context("physical input A was not selected")?;
        let physical_b = self
            .physical_input_b_uid
            .clone()
            .context("physical input B was not selected")?;
        let a = self
            .create_input(
                scenario,
                "A",
                uid_a,
                &physical_a,
                self.physical_input_a_rate,
            )
            .await?;
        let b = self
            .create_input(
                scenario,
                "B",
                uid_b,
                &physical_b,
                self.physical_input_b_rate,
            )
            .await?;
        self.record_rate_pair(scenario, &a, &b)?;
        Ok((a, b))
    }

    async fn create_input(
        &mut self,
        scenario: &str,
        label: &str,
        uid: String,
        physical_input_uid: &str,
        rate: Option<f64>,
    ) -> Result<ResourceRecord> {
        let name = self.resource_name(scenario, &format!("Input {label}"));
        let request = HelperRequest {
            op: "create_subdevice_aggregate".to_string(),
            name: Some(name.clone()),
            uid: Some(uid.clone()),
            device_uid: Some(physical_input_uid.to_string()),
            direction: Some(Direction::Input),
            rate,
            ..Default::default()
        };
        let result = match self.helper_mut()?.call(request).await {
            Ok(result) => result,
            Err(rate_error) if rate.is_some() => {
                // Failed creations remain in the helper's ownership ledger,
                // so a no-rate retry needs a fresh aggregate identity.
                let fallback_uid = format!("{uid}.native");
                self.secrets.push(fallback_uid.clone());
                self.helper_mut()?
                    .call(HelperRequest {
                        op: "create_subdevice_aggregate".to_string(),
                        name: Some(format!("{name} Native")),
                        uid: Some(fallback_uid),
                        device_uid: Some(physical_input_uid.to_string()),
                        direction: Some(Direction::Input),
                        ..Default::default()
                    })
                    .await
                    .with_context(|| {
                        format!("rate request failed ({rate_error:#}); fallback failed")
                    })?
            }
            Err(error) => return Err(error),
        };
        let resource = result
            .resource
            .context("tap creation returned no resource")?;
        self.assertion(
            scenario,
            &format!("physical input aggregate {label} was created"),
            resource.id != 0,
            format!("aggregate_id={}", resource.id),
        )?;
        self.manifest.test_devices.push(DeviceIdentity {
            role: format!("{scenario} physical input {label}"),
            name,
            uid_redacted: redact_uid(&resource.uid),
        });
        self.persist()?;
        Ok(resource)
    }

    async fn create_output_pair(
        &mut self,
        scenario: &str,
    ) -> Result<(ResourceRecord, ResourceRecord)> {
        let physical = self
            .physical_output_uid
            .clone()
            .context("physical output was not selected")?;
        let uid_a = self.uid(scenario, "output_a");
        let uid_b = self.uid(scenario, "output_b");
        let a = self
            .create_output(scenario, "A", uid_a, &physical, Some(44_100.0))
            .await?;
        let b = self
            .create_output(scenario, "B", uid_b, &physical, Some(48_000.0))
            .await?;
        self.record_rate_pair(scenario, &a, &b)?;
        Ok((a, b))
    }

    async fn create_output(
        &mut self,
        scenario: &str,
        label: &str,
        uid: String,
        physical: &str,
        rate: Option<f64>,
    ) -> Result<ResourceRecord> {
        let name = self.resource_name(scenario, &format!("Output {label}"));
        let request = HelperRequest {
            op: "create_subdevice_aggregate".to_string(),
            name: Some(name.clone()),
            uid: Some(uid.clone()),
            device_uid: Some(physical.to_string()),
            direction: Some(Direction::Output),
            rate,
            ..Default::default()
        };
        let result = match self.helper_mut()?.call(request).await {
            Ok(result) => result,
            Err(rate_error) if rate.is_some() => {
                let fallback_uid = format!("{uid}.native");
                self.secrets.push(fallback_uid.clone());
                self.helper_mut()?
                    .call(HelperRequest {
                        op: "create_subdevice_aggregate".to_string(),
                        name: Some(format!("{name} Native")),
                        uid: Some(fallback_uid),
                        device_uid: Some(physical.to_string()),
                        direction: Some(Direction::Output),
                        ..Default::default()
                    })
                    .await
                    .with_context(|| {
                        format!("rate request failed ({rate_error:#}); fallback failed")
                    })?
            }
            Err(error) => return Err(error),
        };
        let resource = result
            .resource
            .context("output aggregate returned no resource")?;
        self.manifest.test_devices.push(DeviceIdentity {
            role: format!("{scenario} output {label}"),
            name,
            uid_redacted: redact_uid(&resource.uid),
        });
        self.persist()?;
        Ok(resource)
    }

    fn record_rate_pair(
        &mut self,
        scenario: &str,
        a: &ResourceRecord,
        b: &ResourceRecord,
    ) -> Result<()> {
        for (label, resource) in [("A", a), ("B", b)] {
            if let Some(rate) = resource.nominal_rate {
                self.manifest.native_rates_hz.push(NativeRate {
                    scenario: scenario.to_string(),
                    device: label.to_string(),
                    rate_hz: rate,
                });
            }
        }
        self.persist()
    }

    async fn set_default(&mut self, uid: &str, direction: Direction) -> Result<()> {
        self.helper_mut()?
            .call(HelperRequest {
                op: "set_default".to_string(),
                uid: Some(uid.to_string()),
                direction: Some(direction),
                ..Default::default()
            })
            .await?;
        Ok(())
    }

    async fn wait_for_default_settle(&mut self, meeting: bool) -> Result<()> {
        self.poll_for(Duration::from_millis(1_200), meeting).await
    }

    async fn set_rate(&mut self, uid: &str, rate: f64) -> Result<()> {
        self.helper_mut()?
            .call(HelperRequest {
                op: "set_rate".to_string(),
                uid: Some(uid.to_string()),
                rate: Some(rate),
                ..Default::default()
            })
            .await?;
        Ok(())
    }

    async fn activate_input(&mut self, resource: &ResourceRecord) -> Result<()> {
        if let Some(rate) = resource.nominal_rate {
            self.set_rate(&resource.uid, rate).await?;
        }
        self.set_default(&resource.uid, Direction::Input).await
    }

    async fn hog_aggregate(&mut self, uid: &str) -> Result<()> {
        self.helper_mut()?
            .call(HelperRequest {
                op: "hog_aggregate".to_string(),
                uid: Some(uid.to_string()),
                ..Default::default()
            })
            .await?;
        Ok(())
    }

    async fn release_hog(&mut self, uid: &str) -> Result<()> {
        self.helper_mut()?
            .call(HelperRequest {
                op: "release_hog".to_string(),
                uid: Some(uid.to_string()),
                ..Default::default()
            })
            .await?;
        Ok(())
    }

    fn write_timeline(&mut self, scenario: &str, gap: f64) -> Result<(PathBuf, Timeline)> {
        let timeline = marker_timeline(self.cli.marker_seconds, gap);
        let path = self.artifact_dir.join(format!("{scenario}-markers.wav"));
        write_stereo_wav(&path, &timeline.samples, SOURCE_RATE)?;
        self.add_artifact(&path)?;
        Ok((path, timeline))
    }

    fn write_reference(&mut self, scenario: &str, duration: f64) -> Result<PathBuf> {
        let path = self.artifact_dir.join(format!("{scenario}-reference.wav"));
        let samples = continuous_tone(REFERENCE_HZ, duration, SOURCE_RATE);
        write_stereo_wav(&path, &samples, SOURCE_RATE)?;
        self.add_artifact(&path)?;
        Ok(path)
    }

    fn write_single_marker(&mut self, scenario: &str, frequency: f64) -> Result<(PathBuf, f64)> {
        let duration = LEADING_SILENCE_SECONDS + self.cli.marker_seconds + 0.5;
        let mut samples = vec![0.0; (duration * SOURCE_RATE as f64) as usize * 2];
        add_tone_stereo(
            &mut samples,
            SOURCE_RATE,
            LEADING_SILENCE_SECONDS,
            self.cli.marker_seconds,
            frequency,
            0.75,
        );
        let path = self.artifact_dir.join(format!("{scenario}-marker.wav"));
        write_stereo_wav(&path, &samples, SOURCE_RATE)?;
        self.add_artifact(&path)?;
        Ok((path, duration))
    }

    async fn stop_dictation_and_copy(
        &mut self,
        scenario: &str,
        log_offset: u64,
    ) -> Result<PathBuf> {
        let log = self.daemon_log.clone();
        let destination = self.artifact_dir.join(format!("{scenario}-captured.wav"));
        let copy_destination = destination.clone();
        let wait = self.settle_timeout();
        let (stop_result, copy_result) = tokio::join!(
            self.post_toggle(),
            wait_for_and_copy_audio_saved(log, log_offset, copy_destination, wait)
        );
        stop_result.context("stop dictation")?;
        self.owns_dictation = false;
        copy_result?;
        self.add_artifact(&destination)?;
        Ok(destination)
    }

    fn copy_meeting_capture(&mut self, scenario: &str, source: &Path) -> Result<PathBuf> {
        let destination = self.artifact_dir.join(format!("{scenario}-captured.wav"));
        fs::copy(source, &destination).with_context(|| {
            format!(
                "copy meeting WAV {} to {}",
                source.display(),
                destination.display()
            )
        })?;
        self.add_artifact(&destination)?;
        Ok(destination)
    }

    fn collect_events(&mut self, scenario: &str, offset: u64) -> Result<Vec<CaptureEvent>> {
        let suffix = read_log_suffix(&self.daemon_log, offset)?;
        let events = parse_capture_events(&suffix, scenario);
        self.manifest.capture_events.extend(events.clone());
        self.persist()?;
        Ok(events)
    }

    async fn assert_daemon_unchanged(&mut self, scenario: &str) -> Result<()> {
        let pid = launchd_daemon_pid(self.settle_timeout()).await?;
        self.manifest.daemon_pid_end = Some(pid);
        self.persist()?;
        self.assertion(
            scenario,
            "launchd daemon PID is unchanged",
            self.manifest.daemon_pid_start == Some(pid),
            format!("start={:?}, current={pid}", self.manifest.daemon_pid_start),
        )
    }

    fn assert_capture_basics(
        &mut self,
        scenario: &str,
        analysis: &CaptureAnalysis,
        wall_seconds: f64,
        wall_tolerance_fraction: f64,
    ) -> Result<()> {
        self.assertion(
            scenario,
            "capture is mono 16 kHz",
            analysis.channels == 1 && analysis.sample_rate == CAPTURE_RATE,
            format!(
                "{} channel(s), {} Hz",
                analysis.channels, analysis.sample_rate
            ),
        )?;
        self.assertion(
            scenario,
            "capture contains nontrivial audio",
            analysis.rms >= MIN_CAPTURE_RMS && analysis.peak >= MIN_CAPTURE_PEAK,
            format!("rms={:.5}, peak={:.5}", analysis.rms, analysis.peak),
        )?;
        let tolerance = 1.25_f64.max(wall_seconds * wall_tolerance_fraction);
        self.assertion(
            scenario,
            "sample count matches capture wall duration",
            (analysis.duration_seconds - wall_seconds).abs() <= tolerance,
            format!(
                "wav={:.3}s, wall={wall_seconds:.3}s, tolerance={tolerance:.3}s",
                analysis.duration_seconds
            ),
        )
    }

    fn assert_markers(&mut self, scenario: &str, analysis: &CaptureAnalysis) -> Result<()> {
        for (name, finding) in [("A", &analysis.marker_a), ("B", &analysis.marker_b)] {
            self.assertion(
                scenario,
                &format!("marker {name} exceeds explicit energy threshold"),
                finding.amplitude >= MIN_TONE_AMPLITUDE,
                format!(
                    "amplitude={:.5}, threshold={MIN_TONE_AMPLITUDE:.5}, offset={:.3}s",
                    finding.amplitude, finding.best_offset_seconds
                ),
            )?;
            self.assertion(
                scenario,
                &format!("marker {name} pitch is within 2%"),
                ((finding.frequency_estimate_hz - finding.expected_hz) / finding.expected_hz).abs()
                    <= 0.02,
                format!(
                    "expected={:.1}Hz, estimated={:.1}Hz",
                    finding.expected_hz, finding.frequency_estimate_hz
                ),
            )?;
        }
        self.assertion(
            scenario,
            "markers occur in A then B order",
            analysis.marker_a.best_offset_seconds < analysis.marker_b.best_offset_seconds,
            format!(
                "A={:.3}s, B={:.3}s",
                analysis.marker_a.best_offset_seconds, analysis.marker_b.best_offset_seconds
            ),
        )
    }

    fn assert_segment_evidence(
        &mut self,
        scenario: &str,
        events: &[CaptureEvent],
        source: &str,
        exact_opened: Option<usize>,
    ) -> Result<()> {
        let opened: Vec<_> = events
            .iter()
            .filter(|event| {
                event.event == "capture_segment_opened" && event.source.as_deref() == Some(source)
            })
            .collect();
        let generations: BTreeSet<_> = opened
            .iter()
            .filter_map(|event| event.stream_generation)
            .collect();
        let expected = exact_opened.unwrap_or(2);
        let count_ok = exact_opened.map_or(opened.len() >= expected, |count| opened.len() == count);
        self.assertion(
            scenario,
            &format!("{source} opened expected Segment generations"),
            count_ok && generations.len() == opened.len(),
            format!("opened={}, generations={generations:?}", opened.len()),
        )?;
        let rates: BTreeSet<_> = opened
            .iter()
            .filter_map(|event| event.native_sample_rate_hz)
            .collect();
        if rates.len() >= 2 {
            self.manifest.mixed_native_rates_observed = true;
            self.persist()?;
        }
        Ok(())
    }

    fn assert_source_segment_energy(
        &mut self,
        scenario: &str,
        events: &[CaptureEvent],
        source: &str,
    ) -> Result<()> {
        let energetic: Vec<_> = events
            .iter()
            .filter(|event| {
                event.event == "capture_segment_closed"
                    && event.source.as_deref() == Some(source)
                    && event.native_samples.unwrap_or(0) > 0
                    && event.native_rms.unwrap_or(0.0) >= 0.001
                    && event.native_peak.unwrap_or(0.0) >= 0.005
            })
            .collect();
        self.assertion(
            scenario,
            &format!("{source} contributed nontrivial audio on both sides of the switch"),
            energetic.len() >= 2,
            format!(
                "energetic_segments={:?}",
                energetic
                    .iter()
                    .map(|event| (event.stream_generation, event.native_rms, event.native_peak))
                    .collect::<Vec<_>>()
            ),
        )
    }

    fn assert_canonical_count(
        &mut self,
        scenario: &str,
        events: &[CaptureEvent],
        source: &str,
        wav_samples: usize,
    ) -> Result<()> {
        let canonical: u64 = events
            .iter()
            .filter(|event| {
                event.event == "capture_segment_closed" && event.source.as_deref() == Some(source)
            })
            .filter_map(|event| event.canonical_samples)
            .sum();
        let tolerance = (CAPTURE_RATE / 4) as i64;
        self.assertion(
            scenario,
            "closed Segment canonical counts agree with WAV sample count",
            canonical > 0 && (canonical as i64 - wav_samples as i64).abs() <= tolerance,
            format!("closed={canonical}, wav={wav_samples}, tolerance={tolerance}"),
        )
    }

    async fn scenario_live_dictation(&mut self) -> Result<()> {
        let scenario = "live-dictation";
        self.timestamp(scenario, "started")?;
        let log_offset = fs::metadata(&self.daemon_log)?.len();
        let (wav, timeline) = self.write_timeline(scenario, SWITCH_GAP_SECONDS)?;
        let (a, b) = self.create_input_pair(scenario).await?;
        let player = self.spawn_player(&wav, timeline.duration).await?;
        self.activate_input(&a).await?;
        self.wait_for_default_settle(false).await?;
        let player_elapsed_at_start = self.players[player].started.elapsed().as_secs_f64();
        self.start_dictation().await?;
        let capture_start = Instant::now();
        self.assertion(
            scenario,
            "capture started before marker A",
            player_elapsed_at_start < timeline.marker_a_start,
            format!("player elapsed={player_elapsed_at_start:.3}s"),
        )?;
        self.wait_until_player(
            player,
            timeline.marker_a_start + self.cli.marker_seconds + 0.1,
            false,
        )
        .await?;
        self.timestamp(scenario, "default-input-switch")?;
        self.activate_input(&b).await?;
        self.finish_player(player, false).await?;
        self.poll_for(Duration::from_millis(400), false).await?;
        let wall = capture_start.elapsed().as_secs_f64();
        let captured = self.stop_dictation_and_copy(scenario, log_offset).await?;
        let analysis = analyze_capture(&captured, false, self.cli.marker_seconds)?;
        write_analysis(&self.artifact_dir, scenario, &analysis)?;
        self.add_artifact(&self.artifact_dir.join(format!("{scenario}-analysis.json")))?;
        // Dictation omits the variable CoreAudio replacement gap rather than Silence Filling it.
        self.assert_capture_basics(scenario, &analysis, wall, 0.35)?;
        self.assert_markers(scenario, &analysis)?;
        let expected_b = timeline.marker_b_start - player_elapsed_at_start;
        self.assertion(
            scenario,
            "marker B remains aligned with the playback timeline",
            (analysis.marker_b.best_offset_seconds - expected_b).abs() <= 2.0,
            format!(
                "observed={:.3}s, expected={expected_b:.3}s",
                analysis.marker_b.best_offset_seconds
            ),
        )?;
        let events = self.collect_events(scenario, log_offset)?;
        self.assert_segment_evidence(scenario, &events, "dictation", None)?;
        self.assert_source_segment_energy(scenario, &events, "dictation")?;
        self.assert_canonical_count(scenario, &events, "dictation", analysis.sample_count)?;
        self.assert_daemon_unchanged(scenario).await?;
        self.wait_status(
            paths::RECORDING_STATUS,
            "recording",
            "dictation ready for another recording",
            |v| {
                matches!(
                    v.get("phase").and_then(Value::as_str),
                    Some("idle" | "error")
                )
            },
        )
        .await?;
        self.timestamp(scenario, "passed")
    }

    async fn scenario_idle(&mut self) -> Result<()> {
        let scenario = "idle";
        self.timestamp(scenario, "started")?;
        let log_offset = fs::metadata(&self.daemon_log)?.len();
        let first = self.idle_dictation("idle-a", MARKER_A_HZ, false).await?;
        self.wait_status(
            paths::RECORDING_STATUS,
            "recording",
            "first dictation processing to finish",
            |v| {
                matches!(
                    v.get("phase").and_then(Value::as_str),
                    Some("idle" | "error")
                )
            },
        )
        .await?;
        self.timestamp(scenario, "switch-while-idle")?;
        let second = self.idle_dictation("idle-b", MARKER_B_HZ, true).await?;
        let first_analysis = analyze_single_capture(&first, MARKER_A_HZ)?;
        let second_analysis = analyze_single_capture(&second, MARKER_B_HZ)?;
        for (label, analysis) in [("idle-a", &first_analysis), ("idle-b", &second_analysis)] {
            let path = self.artifact_dir.join(format!("{label}-analysis.json"));
            fs::write(&path, serde_json::to_vec_pretty(analysis)?)
                .with_context(|| format!("write {label} analysis"))?;
            self.add_artifact(&path)?;
        }
        self.assertion(
            scenario,
            "first dictation contains marker A",
            first_analysis.marker.amplitude >= MIN_TONE_AMPLITUDE,
            format!(
                "amplitude={:.5}, pitch={:.1}Hz, samples={}, duration={:.3}s",
                first_analysis.marker.amplitude,
                first_analysis.marker.frequency_estimate_hz,
                first_analysis.sample_count,
                first_analysis.duration_seconds
            ),
        )?;
        self.assertion(
            scenario,
            "fresh post-switch dictation contains marker B",
            second_analysis.marker.amplitude >= MIN_TONE_AMPLITUDE,
            format!(
                "amplitude={:.5}, pitch={:.1}Hz, samples={}, duration={:.3}s",
                second_analysis.marker.amplitude,
                second_analysis.marker.frequency_estimate_hz,
                second_analysis.sample_count,
                second_analysis.duration_seconds
            ),
        )?;
        let events = self.collect_events(scenario, log_offset)?;
        let opened = events
            .iter()
            .filter(|event| {
                event.event == "capture_segment_opened"
                    && event.source.as_deref() == Some("dictation")
            })
            .count();
        self.assertion(
            scenario,
            "idle switch produced two fresh dictation captures",
            opened == 2,
            format!("dictation capture_segment_opened count={opened}"),
        )?;
        self.assert_daemon_unchanged(scenario).await?;
        self.wait_status(
            paths::RECORDING_STATUS,
            "recording",
            "second idle dictation processing to finish",
            |v| {
                matches!(
                    v.get("phase").and_then(Value::as_str),
                    Some("idle" | "error")
                )
            },
        )
        .await?;
        self.timestamp(scenario, "passed")
    }

    async fn idle_dictation(
        &mut self,
        label: &str,
        frequency: f64,
        select_b: bool,
    ) -> Result<PathBuf> {
        let log_offset = fs::metadata(&self.daemon_log)?.len();
        let (wav, duration) = self.write_single_marker(label, frequency)?;
        let (a, b) = self.create_input_pair(label).await?;
        let player = self.spawn_player(&wav, duration).await?;
        let selected = if select_b { &b } else { &a };
        self.activate_input(selected).await?;
        self.wait_for_default_settle(false).await?;
        self.start_dictation().await?;
        self.finish_player(player, false).await?;
        self.stop_dictation_and_copy(label, log_offset).await
    }

    async fn scenario_meeting_mic(&mut self) -> Result<()> {
        let scenario = "live-meeting-mic";
        self.timestamp(scenario, "started")?;
        let log_offset = fs::metadata(&self.daemon_log)?.len();
        let (markers, timeline) = self.write_timeline(scenario, SWITCH_GAP_SECONDS)?;
        let reference = self.write_reference(scenario, timeline.duration + 2.0)?;
        let (a, b) = self.create_input_pair(scenario).await?;
        self.activate_input(&a).await?;
        let original_output = self
            .original_output_uid
            .clone()
            .context("missing original output")?;
        self.set_default(&original_output, Direction::Output)
            .await?;
        self.wait_for_default_settle(false).await?;
        let started = self
            .meeting_start("Audetic device switch repro: mic")
            .await?;
        let audio_path = PathBuf::from(
            started
                .get("audio_path")
                .and_then(Value::as_str)
                .context("meeting start omitted audio_path")?,
        );
        let reference_player = self
            .spawn_player(&reference, timeline.duration + 2.0)
            .await?;
        let marker_player = self.spawn_player(&markers, timeline.duration).await?;
        let capture_start = Instant::now();
        let player_elapsed_at_start = self.players[marker_player].started.elapsed().as_secs_f64();
        self.assertion(
            scenario,
            "meeting started before marker A",
            player_elapsed_at_start < timeline.marker_a_start,
            format!("player elapsed={player_elapsed_at_start:.3}s"),
        )?;
        self.wait_until_player(
            marker_player,
            timeline.marker_a_start + self.cli.marker_seconds + 0.1,
            true,
        )
        .await?;
        self.activate_input(&b).await?;
        self.finish_player(marker_player, true).await?;
        let wall = capture_start.elapsed().as_secs_f64();
        self.meeting_stop().await?;
        let captured = self.copy_meeting_capture(scenario, &audio_path)?;
        self.meeting_cancel().await?;
        self.stop_player(reference_player).await?;
        let analysis = analyze_capture(&captured, true, self.cli.marker_seconds)?;
        write_analysis(&self.artifact_dir, scenario, &analysis)?;
        self.add_artifact(&self.artifact_dir.join(format!("{scenario}-analysis.json")))?;
        self.assert_capture_basics(scenario, &analysis, wall, 0.20)?;
        self.assert_markers(scenario, &analysis)?;
        self.assert_meeting_mix(scenario, &analysis)?;
        let expected_b = timeline.marker_b_start - player_elapsed_at_start;
        self.assertion(
            scenario,
            "later mic marker was not shifted before its reference timeline",
            analysis.marker_b.best_offset_seconds + 1.0 >= expected_b
                && (analysis.marker_b.best_offset_seconds - expected_b).abs() <= 2.0,
            format!(
                "observed={:.3}s, expected={expected_b:.3}s",
                analysis.marker_b.best_offset_seconds
            ),
        )?;
        let events = self.collect_events(scenario, log_offset)?;
        self.assert_segment_evidence(scenario, &events, "meeting_microphone", None)?;
        self.assert_source_segment_energy(scenario, &events, "meeting_microphone")?;
        self.assert_silence_fill(scenario, &events, "meeting_microphone")?;
        self.assert_daemon_unchanged(scenario).await?;
        self.timestamp(scenario, "passed")
    }

    async fn scenario_meeting_system(&mut self) -> Result<()> {
        let scenario = "live-meeting-system";
        self.timestamp(scenario, "started")?;
        let log_offset = fs::metadata(&self.daemon_log)?.len();
        let (markers, timeline) = self.write_timeline(scenario, SWITCH_GAP_SECONDS)?;
        let reference_uid = self.uid(scenario, "reference_input");
        let physical_input = self
            .physical_input_a_uid
            .clone()
            .context("physical input A was not selected")?;
        let reference_input = self
            .create_input(
                scenario,
                "Reference",
                reference_uid,
                &physical_input,
                self.physical_input_a_rate,
            )
            .await?;
        self.activate_input(&reference_input).await?;
        let (a, b) = self.create_output_pair(scenario).await?;
        let _ = self.set_rate(&a.uid, 44_100.0).await;
        self.set_default(&a.uid, Direction::Output).await?;
        self.wait_for_default_settle(false).await?;
        let started = self
            .meeting_start("Audetic device switch repro: system")
            .await?;
        let audio_path = PathBuf::from(
            started
                .get("audio_path")
                .and_then(Value::as_str)
                .context("meeting start omitted audio_path")?,
        );
        let marker_player = self.spawn_player(&markers, timeline.duration).await?;
        let capture_start = Instant::now();
        self.wait_until_player(
            marker_player,
            timeline.marker_a_start + self.cli.marker_seconds + 0.1,
            true,
        )
        .await?;
        let _ = self.set_rate(&b.uid, 48_000.0).await;
        self.timestamp(scenario, "default-output-switch")?;
        self.set_default(&b.uid, Direction::Output).await?;
        self.finish_player(marker_player, true).await?;
        let wall = capture_start.elapsed().as_secs_f64();
        self.meeting_stop().await?;
        let captured = self.copy_meeting_capture(scenario, &audio_path)?;
        self.meeting_cancel().await?;
        let analysis = analyze_capture(&captured, false, self.cli.marker_seconds)?;
        write_analysis(&self.artifact_dir, scenario, &analysis)?;
        self.add_artifact(&self.artifact_dir.join(format!("{scenario}-analysis.json")))?;
        self.assert_capture_basics(scenario, &analysis, wall, 0.20)?;
        self.assert_markers(scenario, &analysis)?;
        let expected_b = timeline.marker_b_start;
        self.assertion(
            scenario,
            "later system marker was not shifted before its playback timeline",
            analysis.marker_b.best_offset_seconds + 1.0 >= expected_b
                && (analysis.marker_b.best_offset_seconds - expected_b).abs() <= 2.0,
            format!(
                "observed={:.3}s, expected={expected_b:.3}s",
                analysis.marker_b.best_offset_seconds
            ),
        )?;
        let events = self.collect_events(scenario, log_offset)?;
        self.assert_segment_evidence(scenario, &events, "system_tap", None)?;
        self.assert_source_segment_energy(scenario, &events, "system_tap")?;
        self.assert_silence_fill(scenario, &events, "system_tap")?;
        self.assert_segment_evidence(scenario, &events, "meeting_microphone", Some(1))?;
        self.assert_canonical_count(
            scenario,
            &events,
            "meeting_microphone",
            analysis.sample_count,
        )?;
        self.assert_daemon_unchanged(scenario).await?;
        self.timestamp(scenario, "passed")
    }

    fn assert_meeting_mix(&mut self, scenario: &str, analysis: &CaptureAnalysis) -> Result<()> {
        let reference = analysis
            .reference
            .as_ref()
            .context("meeting analysis omitted reference")?;
        self.assertion(
            scenario,
            "continuous 311 Hz reference is present",
            reference.amplitude >= MIN_TONE_AMPLITUDE,
            format!("amplitude={:.5}", reference.amplitude),
        )?;
        let reference_min = analysis.reference_min_amplitude.unwrap_or(0.0);
        self.assertion(
            scenario,
            "311 Hz reference remains continuous across the switch",
            reference_min >= 0.004,
            format!("minimum sliding amplitude={reference_min:.5}"),
        )?;
        let gap = analysis.marker_band_gap_max.unwrap_or(f64::INFINITY);
        let marker_floor = analysis.marker_a.amplitude.min(analysis.marker_b.amplitude);
        self.assertion(
            scenario,
            "marker-band gap is near zero while reference remains continuous",
            gap < 0.01 && gap < marker_floor * 0.75,
            format!("gap={gap:.5}, marker_floor={marker_floor:.5}"),
        )
    }

    fn assert_silence_fill(
        &mut self,
        scenario: &str,
        events: &[CaptureEvent],
        source: &str,
    ) -> Result<()> {
        let fill_events: Vec<_> = events
            .iter()
            .filter(|event| {
                event.event == "capture_silence_fill" && event.source.as_deref() == Some(source)
            })
            .collect();
        let fills: u64 = fill_events
            .iter()
            .filter_map(|event| event.canonical_samples)
            .sum();
        let lengths_match = fill_events.iter().all(|event| {
            let Some(samples) = event.canonical_samples else {
                return false;
            };
            let Some(milliseconds) = event.gap_milliseconds else {
                return false;
            };
            let lower = milliseconds * u64::from(CAPTURE_RATE) / 1_000;
            let upper = (milliseconds + 1) * u64::from(CAPTURE_RATE) / 1_000;
            samples >= lower && samples <= upper
        });
        self.assertion(
            scenario,
            &format!("{source} logged exact nonzero Silence Fill"),
            fills > 0 && lengths_match,
            format!("canonical_samples={fills}, events={fill_events:?}"),
        )
    }

    async fn scenario_degraded(&mut self) -> Result<()> {
        let scenario = "degraded";
        self.timestamp(scenario, "started")?;
        let log_offset = fs::metadata(&self.daemon_log)?.len();
        let gap = self.cli.settle_timeout_seconds + 2.0;
        let (wav, timeline) = self.write_timeline(scenario, gap)?;
        let (a, b) = self.create_input_pair(scenario).await?;
        let player = self.spawn_player(&wav, timeline.duration).await?;
        self.activate_input(&a).await?;
        self.wait_for_default_settle(false).await?;
        self.assertion(
            scenario,
            "capture started before marker A",
            self.players[player].started.elapsed().as_secs_f64() < timeline.marker_a_start,
            format!(
                "player elapsed={:.3}s",
                self.players[player].started.elapsed().as_secs_f64()
            ),
        )?;
        self.start_dictation().await?;
        let capture_start = Instant::now();
        self.wait_until_player(
            player,
            timeline.marker_a_start + self.cli.marker_seconds + 0.1,
            false,
        )
        .await?;
        self.timestamp(scenario, "switch-to-usable-b")?;
        self.activate_input(&b).await?;
        self.poll_for(Duration::from_secs(1), false).await?;
        self.timestamp(scenario, "aggregate-a-hogged")?;
        self.hog_aggregate(&a.uid).await?;
        self.timestamp(scenario, "unopenable-a-selected")?;
        self.activate_input(&a).await?;
        let degradation_started = Instant::now();
        let degraded = self
            .wait_status(
                paths::RECORDING_STATUS,
                "recording",
                "capture_degraded=true",
                |v| v.get("capture_degraded").and_then(Value::as_bool) == Some(true),
            )
            .await;
        self.assertion(
            scenario,
            "hogged Default Input exposes Degraded Capture",
            degraded.is_ok(),
            degraded
                .as_ref()
                .map(Value::to_string)
                .unwrap_or_else(|error| format!("{error:#}")),
        )?;
        self.timestamp(scenario, "replacement-selected")?;
        self.release_hog(&a.uid).await?;
        self.activate_input(&b).await?;
        self.wait_status(
            paths::RECORDING_STATUS,
            "recording",
            "capture_degraded=false",
            |v| {
                v.get("capture_degraded").and_then(Value::as_bool) == Some(false)
                    && v.get("phase").and_then(Value::as_str) == Some("recording")
            },
        )
        .await?;
        let degraded_seconds = degradation_started.elapsed().as_secs_f64();
        self.finish_player(player, false).await?;
        let wall = capture_start.elapsed().as_secs_f64();
        let captured = self.stop_dictation_and_copy(scenario, log_offset).await?;
        let analysis = analyze_capture(&captured, false, self.cli.marker_seconds)?;
        write_analysis(&self.artifact_dir, scenario, &analysis)?;
        self.add_artifact(&self.artifact_dir.join(format!("{scenario}-analysis.json")))?;
        // Dictation intentionally has no Silence Fill. Its canonical duration
        // therefore tracks wall time minus the measured degraded interval.
        self.assert_capture_basics(
            scenario,
            &analysis,
            (wall - degraded_seconds).max(0.0),
            0.35,
        )?;
        self.assertion(
            scenario,
            "degraded gap duration was measured",
            degraded_seconds > 0.0,
            format!("wall={wall:.3}s, degraded_gap={degraded_seconds:.3}s"),
        )?;
        self.assert_markers(scenario, &analysis)?;
        let events = self.collect_events(scenario, log_offset)?;
        self.assert_segment_evidence(scenario, &events, "dictation", None)?;
        self.assert_canonical_count(scenario, &events, "dictation", analysis.sample_count)?;
        self.assert_daemon_unchanged(scenario).await?;
        self.wait_status(
            paths::RECORDING_STATUS,
            "recording",
            "degraded proof dictation processing to finish",
            |v| {
                matches!(
                    v.get("phase").and_then(Value::as_str),
                    Some("idle" | "error")
                )
            },
        )
        .await?;
        self.timestamp(scenario, "passed")
    }

    async fn scenario_churn(&mut self) -> Result<()> {
        let scenario = "churn";
        self.timestamp(scenario, "started")?;
        let (wav, timeline) = self.write_timeline(scenario, SWITCH_GAP_SECONDS)?;
        let (a, b) = self.create_input_pair(scenario).await?;
        let player = self.spawn_player(&wav, timeline.duration).await?;
        self.activate_input(&a).await?;
        // Keep the initial A selection outside the churn evidence window.
        sleep(Duration::from_millis(650)).await;
        self.assertion(
            scenario,
            "baseline capture started before marker A",
            self.players[player].started.elapsed().as_secs_f64() < timeline.marker_a_start,
            format!(
                "player elapsed={:.3}s",
                self.players[player].started.elapsed().as_secs_f64()
            ),
        )?;
        let baseline_search_offset = fs::metadata(&self.daemon_log)?.len();
        self.start_dictation().await?;
        self.wait_status(
            paths::RECORDING_STATUS,
            "recording",
            "baseline dictation Segment",
            |v| v.get("phase").and_then(Value::as_str) == Some("recording"),
        )
        .await?;
        wait_for_log_text(
            &self.daemon_log,
            baseline_search_offset,
            "event=\"capture_segment_opened\" source=\"dictation\"",
            self.settle_timeout(),
        )
        .await?;
        let capture_start = Instant::now();
        let log_offset = fs::metadata(&self.daemon_log)?.len();
        self.wait_until_player(
            player,
            timeline.marker_a_start + self.cli.marker_seconds + 0.05,
            false,
        )
        .await?;
        for resource in [&b, &a, &b, &a, &b] {
            self.activate_input(resource).await?;
            sleep(Duration::from_millis(70)).await;
        }
        self.finish_player(player, false).await?;
        let wall = capture_start.elapsed().as_secs_f64();
        let captured = self.stop_dictation_and_copy(scenario, log_offset).await?;
        let analysis = analyze_capture(&captured, false, self.cli.marker_seconds)?;
        write_analysis(&self.artifact_dir, scenario, &analysis)?;
        self.add_artifact(&self.artifact_dir.join(format!("{scenario}-analysis.json")))?;
        self.assert_capture_basics(scenario, &analysis, wall, 0.35)?;
        self.assert_markers(scenario, &analysis)?;
        let events = self.collect_events(scenario, log_offset)?;
        let settled = events
            .iter()
            .filter(|event| {
                event.event == "settled_device_switch"
                    && event.input_changed == Some(true)
                    && event.output_changed != Some(true)
            })
            .count();
        self.assertion(
            scenario,
            "churn emits exactly one input Settled Switch",
            settled == 1,
            format!("count={settled}"),
        )?;
        self.assert_segment_evidence(scenario, &events, "dictation", Some(1))?;
        self.assert_canonical_count(scenario, &events, "dictation", analysis.sample_count)?;
        self.assert_daemon_unchanged(scenario).await?;
        self.timestamp(scenario, "passed")
    }

    async fn cleanup(&mut self) -> Result<()> {
        let mut errors = Vec::new();
        let recording_is_active = self.owns_dictation
            && self
                .request_json(reqwest::Method::GET, paths::RECORDING_STATUS, None)
                .await
                .is_ok_and(|status| {
                    status.get("phase").and_then(Value::as_str) == Some("recording")
                });
        if recording_is_active {
            if let Err(error) = self.post_toggle().await {
                errors.push(format!("stop owned dictation: {error:#}"));
            }
        }
        let meeting_needs_cancel = self.owns_meeting
            && self
                .request_json(reqwest::Method::GET, paths::MEETINGS_STATUS, None)
                .await
                .is_ok_and(|status| {
                    matches!(
                        status.get("phase").and_then(Value::as_str),
                        Some("recording" | "review")
                    )
                });
        if meeting_needs_cancel {
            if let Err(error) = self.meeting_cancel().await {
                errors.push(format!("cancel owned meeting: {error:#}"));
            }
        }
        for player in &mut self.players {
            if let Err(error) = player.stop().await {
                errors.push(format!("afplay cleanup: {error:#}"));
            }
        }
        if let Some(helper) = &mut self.helper {
            if let Err(error) = helper.cleanup().await {
                errors.push(format!("helper cleanup: {error:#}"));
            }
        }
        if let (Some(original), Some(test)) = (self.original_output_volume, self.test_output_volume)
        {
            match read_output_volume(self.settle_timeout()).await {
                Ok(current) if current == test => {
                    if let Err(error) = set_output_volume(original, self.settle_timeout()).await {
                        errors.push(format!("restore output volume: {error:#}"));
                    }
                }
                Ok(_) => {}
                Err(error) => errors.push(format!("read output volume during cleanup: {error:#}")),
            }
        }
        if self.daemon_log.is_file() {
            match read_log_suffix(&self.daemon_log, self.daemon_log_start) {
                Ok(suffix) => {
                    let path = self.artifact_dir.join("daemon.log");
                    if let Err(error) = fs::write(&path, suffix) {
                        errors.push(format!("write daemon suffix: {error}"));
                    } else if let Err(error) = self.add_artifact(&path) {
                        errors.push(format!("record daemon suffix: {error:#}"));
                    }
                }
                Err(error) => errors.push(format!("read daemon suffix: {error:#}")),
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            bail!(errors.join("; "))
        }
    }

    fn redact_error(&self, error: &str) -> String {
        self.secrets.iter().fold(error.to_string(), |text, secret| {
            text.replace(secret, &redact_uid(secret))
        })
    }
}

fn now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn status_evidence(endpoint: &str, status: &Value) -> Value {
    match endpoint {
        "recording" => json!({
            "recording": status.get("recording"),
            "capture_degraded": status.get("capture_degraded"),
            "phase": status.get("phase"),
            "job_id": status.get("job_id"),
        }),
        "meeting" => json!({
            "active": status.get("active"),
            "capture_degraded": status.get("capture_degraded"),
            "phase": status.get("phase"),
            "meeting_id": status.get("meeting_id"),
            "duration_seconds": status.get("duration_seconds"),
        }),
        _ => json!({"phase": status.get("phase")}),
    }
}

fn redact_uid(uid: &str) -> String {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in uid.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("uid#{}", &format!("{hash:016x}")[..12])
}

fn device_by_uid<'a>(devices: &'a [DeviceRecord], uid: &str) -> Option<&'a DeviceRecord> {
    devices.iter().find(|device| device.uid == uid)
}

async fn launchd_daemon_pid(wait: Duration) -> Result<u32> {
    let uid_output = timeout(wait, Command::new("/usr/bin/id").arg("-u").output())
        .await
        .context("id -u timed out")??;
    if !uid_output.status.success() {
        bail!("id -u failed");
    }
    let uid = String::from_utf8(uid_output.stdout)?.trim().to_string();
    let target = format!("gui/{uid}/ai.audetic.daemon");
    let output = timeout(
        wait,
        Command::new("/bin/launchctl")
            .args(["print", &target])
            .output(),
    )
    .await
    .context("launchctl print timed out")??;
    if !output.status.success() {
        bail!(
            "installed launchd daemon is unavailable: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let text = String::from_utf8(output.stdout)?;
    let regex = Regex::new(r"(?m)^\s*pid\s*=\s*(\d+)\s*$")?;
    regex
        .captures(&text)
        .and_then(|captures| captures[1].parse().ok())
        .context("launchctl service is installed but has no running PID")
}

async fn read_output_volume(wait: Duration) -> Result<OutputVolume> {
    let output = timeout(
        wait,
        Command::new("/usr/bin/osascript")
            .args(["-e", "get volume settings"])
            .output(),
    )
    .await
    .context("reading macOS output volume timed out")??;
    if !output.status.success() {
        bail!(
            "read macOS output volume failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let text = String::from_utf8(output.stdout).context("output volume was not UTF-8")?;
    let level = Regex::new(r"output volume:(\d+)")?
        .captures(&text)
        .and_then(|captures| captures[1].parse::<u8>().ok())
        .context("macOS volume settings omitted output volume")?;
    let muted = Regex::new(r"output muted:(true|false)")?
        .captures(&text)
        .and_then(|captures| captures[1].parse::<bool>().ok())
        .context("macOS volume settings omitted output muted state")?;
    Ok(OutputVolume { level, muted })
}

async fn set_output_volume(volume: OutputVolume, wait: Duration) -> Result<()> {
    let mute_clause = if volume.muted {
        "with output muted"
    } else {
        "without output muted"
    };
    let script = format!("set volume output volume {} {}", volume.level, mute_clause);
    let output = timeout(
        wait,
        Command::new("/usr/bin/osascript")
            .args(["-e", &script])
            .output(),
    )
    .await
    .context("setting macOS output volume timed out")??;
    if !output.status.success() {
        bail!(
            "set macOS output volume failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

fn marker_timeline(marker_seconds: f64, gap_seconds: f64) -> Timeline {
    let marker_a_start = LEADING_SILENCE_SECONDS;
    let marker_b_start = marker_a_start + marker_seconds + gap_seconds;
    let duration = marker_b_start + marker_seconds + 0.75;
    let mut samples = vec![0.0; (duration * SOURCE_RATE as f64).ceil() as usize * 2];
    add_tone_stereo(
        &mut samples,
        SOURCE_RATE,
        marker_a_start,
        marker_seconds,
        MARKER_A_HZ,
        0.75,
    );
    add_tone_stereo(
        &mut samples,
        SOURCE_RATE,
        marker_b_start,
        marker_seconds,
        MARKER_B_HZ,
        0.75,
    );
    Timeline {
        samples,
        marker_a_start,
        marker_b_start,
        duration,
    }
}

fn continuous_tone(frequency: f64, duration: f64, sample_rate: u32) -> Vec<f32> {
    let mut samples = vec![0.0; (duration * sample_rate as f64).ceil() as usize * 2];
    add_tone_stereo(&mut samples, sample_rate, 0.0, duration, frequency, 0.22);
    samples
}

fn add_tone_stereo(
    samples: &mut [f32],
    sample_rate: u32,
    start_seconds: f64,
    duration_seconds: f64,
    frequency: f64,
    amplitude: f64,
) {
    let start = (start_seconds * sample_rate as f64).round() as usize;
    let frames = (duration_seconds * sample_rate as f64).round() as usize;
    let fade = (0.02 * sample_rate as f64).round() as usize;
    for frame in 0..frames {
        let envelope = if frame < fade {
            frame as f64 / fade.max(1) as f64
        } else if frame + fade > frames {
            (frames - frame) as f64 / fade.max(1) as f64
        } else {
            1.0
        };
        let value = (amplitude
            * envelope
            * (std::f64::consts::TAU * frequency * frame as f64 / sample_rate as f64).sin())
            as f32;
        let index = (start + frame) * 2;
        if index + 1 < samples.len() {
            samples[index] += value;
            samples[index + 1] += value;
        }
    }
}

fn write_stereo_wav(path: &Path, samples: &[f32], sample_rate: u32) -> Result<()> {
    let spec = WavSpec {
        channels: 2,
        sample_rate,
        bits_per_sample: 16,
        sample_format: SampleFormat::Int,
    };
    let mut writer = WavWriter::create(path, spec)
        .with_context(|| format!("create marker WAV {}", path.display()))?;
    for sample in samples {
        writer
            .write_sample((sample.clamp(-1.0, 1.0) * i16::MAX as f32).round() as i16)
            .with_context(|| format!("write marker WAV {}", path.display()))?;
    }
    writer
        .finalize()
        .with_context(|| format!("finalize marker WAV {}", path.display()))?;
    Ok(())
}

fn read_wav(path: &Path) -> Result<(WavSpec, Vec<f32>)> {
    let mut reader =
        WavReader::open(path).with_context(|| format!("open WAV {}", path.display()))?;
    let spec = reader.spec();
    let samples = match (spec.sample_format, spec.bits_per_sample) {
        (SampleFormat::Float, 32) => reader
            .samples::<f32>()
            .collect::<std::result::Result<Vec<_>, _>>()
            .with_context(|| format!("read float samples from {}", path.display()))?,
        (SampleFormat::Int, bits) if bits <= 16 => reader
            .samples::<i16>()
            .map(|sample| sample.map(|value| value as f32 / i16::MAX as f32))
            .collect::<std::result::Result<Vec<_>, _>>()
            .with_context(|| format!("read 16-bit samples from {}", path.display()))?,
        (SampleFormat::Int, bits) if bits <= 32 => {
            let scale = ((1_i64 << (bits - 1)) - 1) as f32;
            reader
                .samples::<i32>()
                .map(|sample| sample.map(|value| value as f32 / scale))
                .collect::<std::result::Result<Vec<_>, _>>()
                .with_context(|| format!("read integer samples from {}", path.display()))?
        }
        _ => bail!(
            "unsupported WAV encoding: {:?}/{} bit",
            spec.sample_format,
            spec.bits_per_sample
        ),
    };
    Ok((spec, samples))
}

fn analyze_capture(path: &Path, reference: bool, marker_seconds: f64) -> Result<CaptureAnalysis> {
    let (spec, samples) = read_wav(path)?;
    if spec.channels != 1 || spec.sample_rate != CAPTURE_RATE {
        bail!(
            "capture must be mono 16 kHz, got {} channel(s) at {} Hz",
            spec.channels,
            spec.sample_rate
        );
    }
    if samples.is_empty() {
        bail!("capture WAV is empty");
    }
    let rms = (samples
        .iter()
        .map(|sample| f64::from(*sample).powi(2))
        .sum::<f64>()
        / samples.len() as f64)
        .sqrt();
    let peak = samples
        .iter()
        .map(|sample| f64::from(sample.abs()))
        .fold(0.0, f64::max);
    let marker_a = find_tone(&samples, spec.sample_rate, MARKER_A_HZ)?;
    let marker_b = find_tone(&samples, spec.sample_rate, MARKER_B_HZ)?;
    let reference = reference
        .then(|| find_tone(&samples, spec.sample_rate, REFERENCE_HZ))
        .transpose()?;
    let reference_min_amplitude = reference.as_ref().map(|_| {
        let edge = (0.5 * spec.sample_rate as f64) as usize;
        let interior = if samples.len() > edge * 2 {
            &samples[edge..samples.len() - edge]
        } else {
            &samples[..]
        };
        min_sliding_amplitude(interior, spec.sample_rate, REFERENCE_HZ, 0.30)
    });
    let gap_start =
        ((marker_a.best_offset_seconds + marker_seconds + 0.10) * spec.sample_rate as f64) as usize;
    let gap_end =
        ((marker_b.best_offset_seconds - 0.10).max(0.0) * spec.sample_rate as f64) as usize;
    let marker_band_gap_max = (gap_end > gap_start + spec.sample_rate as usize / 8).then(|| {
        max_sliding_amplitude(
            &samples[gap_start..gap_end],
            spec.sample_rate,
            &[MARKER_A_HZ, MARKER_B_HZ],
            0.12,
        )
    });
    Ok(CaptureAnalysis {
        sample_rate: spec.sample_rate,
        channels: spec.channels,
        sample_count: samples.len(),
        duration_seconds: samples.len() as f64 / spec.sample_rate as f64,
        rms,
        peak,
        marker_a,
        marker_b,
        reference,
        reference_min_amplitude,
        marker_band_gap_max,
    })
}

fn analyze_single_capture(path: &Path, frequency: f64) -> Result<SingleCaptureAnalysis> {
    let (spec, samples) = read_wav(path)?;
    if spec.channels != 1 || spec.sample_rate != CAPTURE_RATE {
        bail!("dictation capture is not mono 16 kHz");
    }
    let rms = (samples
        .iter()
        .map(|sample| f64::from(*sample).powi(2))
        .sum::<f64>()
        / samples.len().max(1) as f64)
        .sqrt();
    let peak = samples
        .iter()
        .map(|sample| f64::from(sample.abs()))
        .fold(0.0, f64::max);
    Ok(SingleCaptureAnalysis {
        sample_rate: spec.sample_rate,
        channels: spec.channels,
        sample_count: samples.len(),
        duration_seconds: samples.len() as f64 / spec.sample_rate as f64,
        rms,
        peak,
        marker: find_tone(&samples, spec.sample_rate, frequency)?,
    })
}

fn find_tone(samples: &[f32], sample_rate: u32, frequency: f64) -> Result<ToneFinding> {
    let window = (0.35 * sample_rate as f64).round() as usize;
    let hop = (0.04 * sample_rate as f64).round() as usize;
    if samples.len() < window {
        bail!("capture is too short for tone analysis");
    }
    let mut best_start = 0;
    let mut best_amplitude = -1.0;
    let mut best_rms = 0.0;
    for start in (0..=samples.len() - window).step_by(hop.max(1)) {
        let slice = &samples[start..start + window];
        let amplitude = tone_amplitude(slice, sample_rate, frequency);
        if amplitude > best_amplitude {
            best_amplitude = amplitude;
            best_start = start;
            best_rms = (slice
                .iter()
                .map(|sample| f64::from(*sample).powi(2))
                .sum::<f64>()
                / slice.len() as f64)
                .sqrt();
        }
    }
    let slice = &samples[best_start..best_start + window];
    let mut estimate = frequency;
    let mut estimate_amplitude = -1.0;
    let min = (frequency * 0.96).floor() as u32;
    let max = (frequency * 1.04).ceil() as u32;
    for candidate in min..=max {
        let amplitude = tone_amplitude(slice, sample_rate, f64::from(candidate));
        if amplitude > estimate_amplitude {
            estimate_amplitude = amplitude;
            estimate = f64::from(candidate);
        }
    }
    Ok(ToneFinding {
        expected_hz: frequency,
        best_offset_seconds: best_start as f64 / sample_rate as f64,
        amplitude: best_amplitude,
        window_rms: best_rms,
        frequency_estimate_hz: estimate,
    })
}

fn tone_amplitude(samples: &[f32], sample_rate: u32, frequency: f64) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }
    let omega = std::f64::consts::TAU * frequency / sample_rate as f64;
    let (sin_sum, cos_sum) = samples
        .iter()
        .enumerate()
        .fold((0.0, 0.0), |acc, (index, sample)| {
            let phase = omega * index as f64;
            (
                acc.0 + f64::from(*sample) * phase.sin(),
                acc.1 + f64::from(*sample) * phase.cos(),
            )
        });
    2.0 * sin_sum.hypot(cos_sum) / samples.len() as f64
}

fn max_sliding_amplitude(
    samples: &[f32],
    sample_rate: u32,
    frequencies: &[f64],
    seconds: f64,
) -> f64 {
    let window = (seconds * sample_rate as f64).round() as usize;
    if samples.len() < window || window == 0 {
        return 0.0;
    }
    let hop = (window / 3).max(1);
    (0..=samples.len() - window)
        .step_by(hop)
        .flat_map(|start| {
            frequencies.iter().map(move |frequency| {
                tone_amplitude(&samples[start..start + window], sample_rate, *frequency)
            })
        })
        .fold(0.0, f64::max)
}

fn min_sliding_amplitude(samples: &[f32], sample_rate: u32, frequency: f64, seconds: f64) -> f64 {
    let window = (seconds * sample_rate as f64).round() as usize;
    if samples.len() < window || window == 0 {
        return 0.0;
    }
    let hop = (window / 2).max(1);
    (0..=samples.len() - window)
        .step_by(hop)
        .map(|start| tone_amplitude(&samples[start..start + window], sample_rate, frequency))
        .fold(f64::INFINITY, f64::min)
}

fn write_analysis(root: &Path, scenario: &str, analysis: &CaptureAnalysis) -> Result<()> {
    fs::write(
        root.join(format!("{scenario}-analysis.json")),
        serde_json::to_vec_pretty(analysis)?,
    )
    .with_context(|| format!("write {scenario} marker analysis"))?;
    Ok(())
}

fn read_log_suffix(path: &Path, offset: u64) -> Result<String> {
    let mut file =
        File::open(path).with_context(|| format!("open daemon log {}", path.display()))?;
    let len = file.metadata()?.len();
    if len < offset {
        bail!("daemon log was truncated during the run");
    }
    file.seek(SeekFrom::Start(offset))?;
    let mut suffix = String::new();
    file.read_to_string(&mut suffix)?;
    Ok(suffix)
}

async fn wait_for_and_copy_audio_saved(
    log: PathBuf,
    offset: u64,
    destination: PathBuf,
    wait: Duration,
) -> Result<()> {
    let deadline = Instant::now() + wait;
    loop {
        let suffix = read_log_suffix(&log, offset)?;
        if let Some(path) = parse_audio_saved_path(&suffix) {
            if path.is_file() {
                fs::copy(&path, &destination).with_context(|| {
                    format!(
                        "copy dictation WAV {} to {} before retention cleanup",
                        path.display(),
                        destination.display()
                    )
                })?;
                return Ok(());
            }
        }
        if Instant::now() >= deadline {
            bail!("timed out waiting for `Audio saved to:` in daemon log");
        }
        sleep(STATUS_POLL).await;
    }
}

async fn wait_for_log_text(log: &Path, offset: u64, needle: &str, wait: Duration) -> Result<()> {
    let deadline = Instant::now() + wait;
    loop {
        if strip_ansi(&read_log_suffix(log, offset)?).contains(needle) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            bail!("timed out waiting for `{needle}` in daemon log");
        }
        sleep(STATUS_POLL).await;
    }
}

fn parse_audio_saved_path(log: &str) -> Option<PathBuf> {
    let regex = Regex::new(r#"Audio saved to:\s*\"([^\"]+\.wav)\""#).ok()?;
    regex
        .captures_iter(log)
        .last()
        .map(|captures| PathBuf::from(&captures[1]))
}

fn parse_capture_events(log: &str, scenario: &str) -> Vec<CaptureEvent> {
    log.lines()
        .filter_map(|line| {
            let line = strip_ansi(line);
            let line = line.as_str();
            let event = capture_field(line, "event")?;
            if !matches!(
                event.as_str(),
                "capture_segment_opened"
                    | "capture_segment_closed"
                    | "capture_silence_fill"
                    | "settled_device_switch"
            ) {
                return None;
            }
            Some(CaptureEvent {
                scenario: scenario.to_string(),
                timestamp: line.split_whitespace().next().map(str::to_string),
                event,
                source: capture_field(line, "source"),
                stream_generation: capture_u64(line, "stream_generation"),
                native_samples: capture_u64(line, "native_samples"),
                native_sample_rate_hz: capture_u64(line, "native_sample_rate_hz")
                    .and_then(|value| u32::try_from(value).ok()),
                native_rms: capture_f64(line, "native_rms"),
                native_peak: capture_f64(line, "native_peak"),
                canonical_samples: capture_u64(line, "canonical_samples"),
                gap_milliseconds: capture_u64(line, "gap_milliseconds"),
                input_changed: capture_bool(line, "input_changed"),
                output_changed: capture_bool(line, "output_changed"),
            })
        })
        .collect()
}

fn strip_ansi(text: &str) -> String {
    Regex::new(r"\x1b\[[0-9;]*m")
        .map(|regex| regex.replace_all(text, "").into_owned())
        .unwrap_or_else(|_| text.to_string())
}

fn capture_field(line: &str, field: &str) -> Option<String> {
    let regex = Regex::new(&format!(
        r#"\b{}=(?:\"([^\"]+)\"|([^\s]+))"#,
        regex::escape(field)
    ))
    .ok()?;
    let captures = regex.captures(line)?;
    Some(
        captures
            .get(1)
            .or_else(|| captures.get(2))?
            .as_str()
            .trim_matches(',')
            .to_string(),
    )
}

fn capture_u64(line: &str, field: &str) -> Option<u64> {
    capture_field(line, field)?.parse().ok()
}

fn capture_f64(line: &str, field: &str) -> Option<f64> {
    capture_field(line, field)?.parse().ok()
}

fn capture_bool(line: &str, field: &str) -> Option<bool> {
    capture_field(line, field)?.parse().ok()
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let mut harness = Harness::create(cli)?;
    println!("{}", harness.artifact_dir.display());

    let run_result = tokio::select! {
        result = harness.execute() => result,
        signal = shutdown_signal() => {
            let signal = signal?;
            Err(anyhow!("interrupted by {signal}"))
        }
    };
    let cleanup_result = harness.cleanup().await;
    let final_result = match (run_result, cleanup_result) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(run), Ok(())) => Err(run),
        (Ok(()), Err(cleanup)) => Err(cleanup.context("cleanup failed")),
        (Err(run), Err(cleanup)) => Err(anyhow!("{run:#}; cleanup failed: {cleanup:#}")),
    };
    harness.manifest.finished_at = Some(now());
    match &final_result {
        Ok(()) => harness.manifest.outcome = "pass".to_string(),
        Err(error) => {
            harness.manifest.outcome = "fail".to_string();
            harness.manifest.error = Some(harness.redact_error(&format!("{error:#}")));
        }
    }
    harness.persist()?;
    println!("{}", harness.artifact_dir.display());
    final_result
}

#[cfg(unix)]
async fn shutdown_signal() -> Result<&'static str> {
    use tokio::signal::unix::{signal, SignalKind};

    let mut terminate = signal(SignalKind::terminate()).context("install SIGTERM handler")?;
    let mut hangup = signal(SignalKind::hangup()).context("install SIGHUP handler")?;
    tokio::select! {
        result = tokio::signal::ctrl_c() => {
            result.context("install Ctrl-C handler")?;
            Ok("Ctrl-C")
        }
        _ = terminate.recv() => Ok("SIGTERM"),
        _ = hangup.recv() => Ok("SIGHUP"),
    }
}

#[cfg(not(unix))]
async fn shutdown_signal() -> Result<&'static str> {
    tokio::signal::ctrl_c()
        .await
        .context("install Ctrl-C handler")?;
    Ok("Ctrl-C")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_wav(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("audetic-{name}-{}.wav", Uuid::new_v4()))
    }

    #[test]
    fn marker_generation_is_deterministic_and_faded() {
        let first = marker_timeline(1.0, 1.0);
        let second = marker_timeline(1.0, 1.0);
        assert_eq!(first.samples, second.samples);
        assert!(first.samples[..SOURCE_RATE as usize * 2]
            .iter()
            .all(|sample| *sample == 0.0));
        let start = (first.marker_a_start * SOURCE_RATE as f64) as usize * 2;
        assert_eq!(first.samples[start], 0.0);
        assert!(first.samples[start + 2000].abs() > first.samples[start + 2].abs());
    }

    #[test]
    fn analysis_finds_ordered_markers_and_reference() {
        let path = temp_wav("analysis");
        let timeline = marker_timeline(1.0, 1.0);
        let frames = timeline.samples.len() / 2;
        let mut mono = Vec::with_capacity(frames);
        for frame in 0..frames {
            let marker = timeline.samples[frame * 2];
            let reference = (0.08
                * (std::f64::consts::TAU * REFERENCE_HZ * frame as f64 / SOURCE_RATE as f64).sin())
                as f32;
            mono.push(marker + reference);
        }
        let spec = WavSpec {
            channels: 1,
            sample_rate: CAPTURE_RATE,
            bits_per_sample: 32,
            sample_format: SampleFormat::Float,
        };
        let mut writer = WavWriter::create(&path, spec).unwrap();
        for output_index in 0..(timeline.duration * CAPTURE_RATE as f64) as usize {
            let source_index = output_index * 3;
            writer.write_sample(mono[source_index]).unwrap();
        }
        writer.finalize().unwrap();
        let analysis = analyze_capture(&path, true, 1.0).unwrap();
        assert!(analysis.marker_a.amplitude > 0.1);
        assert!(analysis.marker_b.amplitude > 0.1);
        assert!(analysis.marker_a.best_offset_seconds < analysis.marker_b.best_offset_seconds);
        assert!(analysis.reference.unwrap().amplitude > 0.05);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn redaction_is_stable_and_does_not_retain_uid() {
        let uid = "BuiltInMicrophoneDevice-very-secret";
        let first = redact_uid(uid);
        assert_eq!(first, redact_uid(uid));
        assert!(first.starts_with("uid#"));
        assert!(!first.contains("BuiltIn"));
        assert_ne!(first, redact_uid("another-device"));
    }

    #[test]
    fn log_parser_extracts_capture_and_settled_switch_fields() {
        let log = "\u{1b}[2m2026-08-15T10:00:00Z\u{1b}[0m INFO event=\u{1b}[0m\"capture_segment_opened\" source=\"dictation\" stream_generation=7 native_sample_rate_hz=44100 Capture Segment opened\n\
2026-08-15T10:00:01Z INFO event=\"capture_segment_closed\" source=\"dictation\" stream_generation=7 native_samples=44100 native_rms=0.25 native_peak=0.5 canonical_samples=16000\n\
2026-08-15T10:00:02Z INFO event=\"capture_silence_fill\" source=\"meeting_microphone\" canonical_samples=8000 gap_milliseconds=500\n\
2026-08-15T10:00:03Z INFO event=\"settled_device_switch\" input_changed=true output_changed=false\n";
        let events = parse_capture_events(log, "test");
        assert_eq!(events.len(), 4);
        assert_eq!(events[0].stream_generation, Some(7));
        assert_eq!(events[0].native_sample_rate_hz, Some(44_100));
        assert_eq!(events[1].canonical_samples, Some(16_000));
        assert_eq!(events[1].native_samples, Some(44_100));
        assert_eq!(events[1].native_rms, Some(0.25));
        assert_eq!(events[1].native_peak, Some(0.5));
        assert_eq!(events[2].canonical_samples, Some(8_000));
        assert_eq!(events[2].gap_milliseconds, Some(500));
        assert_eq!(events[3].input_changed, Some(true));
        assert_eq!(events[3].output_changed, Some(false));
    }

    #[test]
    fn audio_saved_parser_handles_quoted_paths() {
        let log = r#"INFO Audio saved to: "/tmp/path with spaces/a.wav""#;
        assert_eq!(
            parse_audio_saved_path(log),
            Some(PathBuf::from("/tmp/path with spaces/a.wav"))
        );
    }

    #[test]
    fn status_evidence_omits_transcripts_paths_and_errors() {
        let recording = json!({
            "recording": false,
            "capture_degraded": false,
            "phase": "idle",
            "job_id": "job",
            "last_completed_job": {"text": "private transcript"},
            "last_error": "/Users/private/path",
        });
        let evidence = status_evidence("recording", &recording);
        let encoded = evidence.to_string();
        assert!(!encoded.contains("private transcript"));
        assert!(!encoded.contains("/Users/private/path"));
        assert_eq!(evidence["phase"], "idle");
    }
}
