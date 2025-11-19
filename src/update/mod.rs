use crate::global;
use anyhow::{anyhow, Context, Result};
use fs2::FileExt;
use reqwest::Client;
use semver::Version;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::cmp::Ordering;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::fs;
use tokio::io::AsyncReadExt;
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};
use walkdir::WalkDir;

const DEFAULT_BASE_URL: &str = "https://install.audetic.ai";
const DEFAULT_CHANNEL: &str = "stable";
const BIN_NAME: &str = "audetic";
const UPDATE_INTERVAL_HOURS: u64 = 6;

#[derive(Clone)]
pub struct UpdateConfig {
    pub base_url: String,
    pub channel: String,
    pub check_interval: Duration,
    pub binary_path: PathBuf,
    pub updates_dir: PathBuf,
    pub state_file: PathBuf,
    pub lock_file: PathBuf,
    pub target_id: Option<String>,
    pub current_version: String,
    pub restart_on_success: bool,
}

impl UpdateConfig {
    pub fn detect(channel_override: Option<String>) -> Result<Self> {
        let base_url =
            std::env::var("AUDETIC_INSTALL_URL").unwrap_or_else(|_| DEFAULT_BASE_URL.to_string());
        let channel = channel_override
            .or_else(|| std::env::var("AUDETIC_CHANNEL").ok())
            .unwrap_or_else(|| DEFAULT_CHANNEL.to_string());
        let binary_path =
            std::env::current_exe().context("Failed to resolve current executable")?;
        let updates_dir = global::updates_dir()?;
        let state_file = global::update_state_file()?;
        let lock_file = global::update_lock_file()?;
        let interval = std::env::var("AUDETIC_UPDATE_INTERVAL_SECS")
            .ok()
            .and_then(|raw| raw.parse::<u64>().ok())
            .map(Duration::from_secs)
            .unwrap_or_else(|| Duration::from_secs(UPDATE_INTERVAL_HOURS * 3600));
        let restart_on_success = std::env::var("AUDETIC_DISABLE_AUTO_RESTART").is_err();
        let target_id = default_target_id().map(|s| s.to_string());
        Ok(Self {
            base_url,
            channel,
            check_interval: interval,
            binary_path,
            updates_dir,
            state_file,
            lock_file,
            target_id,
            current_version: env!("CARGO_PKG_VERSION").to_string(),
            restart_on_success,
        })
    }
}

#[derive(Clone)]
pub struct UpdateEngine {
    inner: Arc<UpdateEngineInner>,
}

struct UpdateEngineInner {
    client: Client,
    config: UpdateConfig,
}

impl UpdateEngine {
    pub fn new(config: UpdateConfig) -> Result<Self> {
        if config.target_id.is_none() {
            warn!("Auto-update disabled: unsupported target triple");
        }
        let client = Client::builder()
            .build()
            .context("Failed to create HTTP client")?;
        Ok(Self {
            inner: Arc::new(UpdateEngineInner { client, config }),
        })
    }

    pub fn spawn_background(self, channel_override: Option<String>) -> Option<JoinHandle<()>> {
        if std::env::var("AUDETIC_DISABLE_AUTO_UPDATE")
            .map(|raw| raw == "1" || raw.eq_ignore_ascii_case("true"))
            .unwrap_or(false)
        {
            info!("Auto-update disabled via AUDETIC_DISABLE_AUTO_UPDATE");
            return None;
        }

        self.inner.config.target_id.as_ref()?;

        let engine = self.clone();
        let channel = channel_override.unwrap_or_else(|| engine.inner.config.channel.clone());
        let interval = engine.inner.config.check_interval;
        Some(tokio::spawn(async move {
            info!(
                "Starting auto-update checks (channel={}, interval={}s)",
                channel,
                interval.as_secs()
            );
            loop {
                if let Err(err) = engine
                    .check_and_update(&channel, UpdateMode::Install { force: false })
                    .await
                {
                    warn!("Auto-update check failed: {err:?}");
                }
                tokio::time::sleep(interval).await;
            }
        }))
    }

    pub async fn run_manual(&self, opts: UpdateOptions) -> Result<UpdateReport> {
        if opts.enable_auto_update {
            let state = self.set_auto_update(true).await?;
            return Ok(UpdateReport::auto_update_changed(true, state.auto_update));
        }
        if opts.disable_auto_update {
            let state = self.set_auto_update(false).await?;
            return Ok(UpdateReport::auto_update_changed(false, state.auto_update));
        }

        let channel = opts
            .channel
            .clone()
            .unwrap_or_else(|| self.inner.config.channel.clone());

        let mode = if opts.check_only {
            UpdateMode::CheckOnly
        } else {
            UpdateMode::Install { force: opts.force }
        };

        self.check_and_update(&channel, mode).await
    }

    async fn check_and_update(&self, channel: &str, mode: UpdateMode) -> Result<UpdateReport> {
        if self.inner.config.target_id.is_none() {
            return Ok(UpdateReport::unsupported(
                self.inner.config.current_version.clone(),
            ));
        }

        let _lock = self.acquire_lock().await?;
        let mut state = self.load_state().await?;
        state.channel = channel.to_string();
        let auto_update_env_disabled = std::env::var("AUDETIC_DISABLE_AUTO_UPDATE")
            .map(|raw| raw == "1" || raw.eq_ignore_ascii_case("true"))
            .unwrap_or(false);

        let remote_version = self.fetch_remote_version(channel).await?;
        let current_version = self.inner.config.current_version.clone();
        let comparison = compare_versions(&remote_version, &current_version);

        let needs_update = match comparison {
            Some(Ordering::Greater) => true,
            Some(Ordering::Equal) => mode.force(),
            Some(Ordering::Less) => mode.force(),
            None => {
                warn!(
                    "Unable to compare versions (remote={}, local={})",
                    remote_version, current_version
                );
                false
            }
        };

        let now = unix_timestamp();
        state.last_check_epoch = Some(now);
        state.last_error = None;
        state.last_known_remote = Some(remote_version.clone());

        if mode.is_check_only() {
            self.save_state(&state).await?;
            return Ok(UpdateReport::checked(
                current_version,
                remote_version,
                needs_update,
            ));
        }

        if !needs_update && !mode.force() {
            self.save_state(&state).await?;
            return Ok(UpdateReport::up_to_date(current_version, remote_version));
        }

        if auto_update_env_disabled || (!state.auto_update && !mode.force()) {
            self.save_state(&state).await?;
            return Ok(UpdateReport::disabled(current_version, remote_version));
        }

        match self.download_and_install(&remote_version, &mut state).await {
            Ok(_) => {
                state.last_downloaded_version = Some(remote_version.clone());
                state.pending_restart = true;
                self.save_state(&state).await?;
                info!(
                    "Update to {} installed. Restart required to take effect.",
                    remote_version
                );
                if self.inner.config.restart_on_success {
                    info!("Exiting to allow supervisor to restart with the new binary.");
                    std::process::exit(0);
                }
                Ok(UpdateReport::installed(current_version, remote_version))
            }
            Err(err) => {
                let message = format!("{err:?}");
                state.last_error = Some(message.clone());
                self.save_state(&state).await?;
                Err(err)
            }
        }
    }

    async fn download_and_install(&self, version: &str, state: &mut UpdateState) -> Result<()> {
        let manifest = self.fetch_manifest(version).await?;
        let target_id = self
            .inner
            .config
            .target_id
            .clone()
            .expect("target id must exist");
        let target = manifest
            .targets
            .get(&target_id)
            .cloned()
            .ok_or_else(|| anyhow!("Target {} not available in manifest", target_id))?;

        let archive_url = format!(
            "{}/cli/releases/{}/{}",
            self.inner.config.base_url, version, target.archive
        );

        fs::create_dir_all(&self.inner.config.updates_dir)
            .await
            .context("Failed to ensure updates dir")?;
        let download_dir = self.inner.config.updates_dir.join(version).join(&target_id);
        if download_dir.exists() {
            fs::remove_dir_all(&download_dir)
                .await
                .context("Failed to clean previous download dir")?;
        }
        fs::create_dir_all(&download_dir)
            .await
            .context("Failed to create download dir")?;

        let archive_path = download_dir.join(&target.archive);
        self.fetch_to_file(&archive_url, &archive_path).await?;
        let checksum = self.compute_sha256(&archive_path).await?;
        if checksum != target.sha256 {
            return Err(anyhow!(
                "Checksum mismatch. expected={} actual={}",
                target.sha256,
                checksum
            ));
        }

        let staging_dir = download_dir.join("staging");
        if staging_dir.exists() {
            fs::remove_dir_all(&staging_dir)
                .await
                .context("Failed to clean staging dir")?;
        }
        fs::create_dir_all(&staging_dir)
            .await
            .context("Failed to create staging dir")?;

        self.extract_archive(&archive_path, &staging_dir).await?;
        let new_binary = self.locate_binary(&staging_dir)?;
        self.install_binary(&new_binary, version)?;

        state.current_version = Some(version.to_string());
        Ok(())
    }

    async fn fetch_remote_version(&self, channel: &str) -> Result<String> {
        let path = if channel == "stable" {
            "version".to_string()
        } else {
            format!("version-{channel}")
        };
        let url = format!("{}/cli/{}", self.inner.config.base_url, path);
        let text = self
            .inner
            .client
            .get(&url)
            .send()
            .await?
            .error_for_status()?
            .text()
            .await?;
        Ok(text.trim().to_string())
    }

    async fn fetch_manifest(&self, version: &str) -> Result<ReleaseManifest> {
        let url = format!(
            "{}/cli/releases/{}/manifest.json",
            self.inner.config.base_url, version
        );
        let text = self
            .inner
            .client
            .get(&url)
            .send()
            .await?
            .error_for_status()?
            .text()
            .await?;
        let manifest: ReleaseManifest = serde_json::from_str(&text)
            .with_context(|| format!("Failed to parse manifest for version {version}"))?;
        Ok(manifest)
    }

    async fn fetch_to_file(&self, url: &str, destination: &Path) -> Result<()> {
        let bytes = self
            .inner
            .client
            .get(url)
            .send()
            .await?
            .error_for_status()?
            .bytes()
            .await?;
        fs::write(destination, &bytes)
            .await
            .with_context(|| format!("Failed to write download {}", destination.display()))?;
        Ok(())
    }

    async fn compute_sha256(&self, path: &Path) -> Result<String> {
        let mut file = fs::File::open(path).await?;
        let mut hasher = Sha256::new();
        let mut buffer = vec![0u8; 64 * 1024];
        loop {
            let read = file.read(&mut buffer).await?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
        }
        Ok(format!("{:x}", hasher.finalize()))
    }

    async fn extract_archive(&self, archive_path: &Path, dest: &Path) -> Result<()> {
        let archive = archive_path.to_path_buf();
        let output = dest.to_path_buf();
        tokio::task::spawn_blocking(move || -> Result<()> {
            let file = File::open(&archive)?;
            let decoder = flate2::read::GzDecoder::new(file);
            let mut archive = tar::Archive::new(decoder);
            archive
                .unpack(&output)
                .context("Failed to unpack update archive")?;
            Ok(())
        })
        .await?
    }

    fn locate_binary(&self, root: &Path) -> Result<PathBuf> {
        for entry in WalkDir::new(root).into_iter().filter_map(Result::ok) {
            if entry.file_type().is_file() && entry.file_name() == BIN_NAME {
                return Ok(entry.into_path());
            }
        }
        Err(anyhow!(
            "Downloaded archive did not contain {} binary",
            BIN_NAME
        ))
    }

    fn install_binary(&self, staged: &Path, version: &str) -> Result<()> {
        let target_path = &self.inner.config.binary_path;
        let parent = target_path
            .parent()
            .context("Binary path missing parent directory")?;
        let tmp_path = parent.join(format!("{BIN_NAME}-{version}.tmp"));
        std::fs::copy(staged, &tmp_path)
            .with_context(|| format!("Failed to copy staged binary to {}", tmp_path.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&tmp_path)?.permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&tmp_path, perms)?;
        }
        let backup_path = parent.join(format!(
            "{BIN_NAME}-{}.bak",
            self.inner.config.current_version
        ));
        if target_path.exists() {
            if let Err(err) = std::fs::copy(target_path, &backup_path) {
                warn!(
                    "Failed to create backup at {}: {err:?}",
                    backup_path.display()
                );
            }
        }
        std::fs::rename(&tmp_path, target_path)
            .with_context(|| format!("Failed to replace {}", target_path.display()))?;
        Ok(())
    }

    async fn acquire_lock(&self) -> Result<UpdateLock> {
        let path = self.inner.config.lock_file.clone();
        tokio::task::spawn_blocking(move || {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let file = std::fs::OpenOptions::new()
                .create(true)
                .read(true)
                .write(true)
                .truncate(false)
                .open(&path)?;
            file.lock_exclusive()
                .with_context(|| "Failed to acquire update lock")?;
            Ok(UpdateLock { file })
        })
        .await?
    }

    async fn load_state(&self) -> Result<UpdateState> {
        let path = self.inner.config.state_file.clone();
        if !path.exists() {
            return Ok(UpdateState {
                channel: self.inner.config.channel.clone(),
                current_version: Some(self.inner.config.current_version.clone()),
                ..Default::default()
            });
        }
        let content = fs::read_to_string(&path).await?;
        let mut state: UpdateState = serde_json::from_str(&content)?;
        state.reconcile_with_running(&self.inner.config.current_version);
        if state.channel.is_empty() {
            state.channel = self.inner.config.channel.clone();
        }
        Ok(state)
    }

    async fn save_state(&self, state: &UpdateState) -> Result<()> {
        if let Some(parent) = self.inner.config.state_file.parent() {
            fs::create_dir_all(parent).await?;
        }
        let content = serde_json::to_string_pretty(state)?;
        fs::write(&self.inner.config.state_file, content).await?;
        Ok(())
    }

    pub async fn set_auto_update(&self, enabled: bool) -> Result<UpdateState> {
        let _lock = self.acquire_lock().await?;
        let mut state = self.load_state().await?;
        state.auto_update = enabled;
        self.save_state(&state).await?;
        Ok(state)
    }
}

#[derive(Debug)]
pub struct UpdateOptions {
    pub channel: Option<String>,
    pub check_only: bool,
    pub force: bool,
    pub enable_auto_update: bool,
    pub disable_auto_update: bool,
}

#[derive(Debug)]
pub enum UpdateMode {
    CheckOnly,
    Install { force: bool },
}

impl UpdateMode {
    fn is_check_only(&self) -> bool {
        matches!(self, UpdateMode::CheckOnly)
    }

    fn force(&self) -> bool {
        matches!(self, UpdateMode::Install { force: true })
    }
}

#[derive(Debug, Clone)]
pub struct UpdateReport {
    pub current_version: String,
    pub remote_version: Option<String>,
    pub message: String,
}

impl UpdateReport {
    fn unsupported(current: String) -> Self {
        Self {
            current_version: current,
            remote_version: None,
            message: "Auto-update not available on this platform".to_string(),
        }
    }

    fn disabled(current: String, remote: String) -> Self {
        Self {
            current_version: current,
            remote_version: Some(remote),
            message: "Auto-update disabled. Enable it to install new versions.".to_string(),
        }
    }

    fn up_to_date(current: String, remote: String) -> Self {
        Self {
            current_version: current,
            remote_version: Some(remote.clone()),
            message: format!("Already on latest version ({remote})."),
        }
    }

    fn checked(current: String, remote: String, needs_update: bool) -> Self {
        let message = if needs_update {
            format!("Update available: {current} → {remote}")
        } else {
            format!("Already on latest version ({remote})")
        };
        Self {
            current_version: current,
            remote_version: Some(remote),
            message,
        }
    }

    fn installed(current: String, remote: String) -> Self {
        Self {
            current_version: current,
            remote_version: Some(remote.clone()),
            message: format!("Update installed. Restart required to run {remote}."),
        }
    }

    fn auto_update_changed(requested: bool, actual: bool) -> Self {
        Self {
            current_version: env!("CARGO_PKG_VERSION").to_string(),
            remote_version: None,
            message: if requested == actual {
                format!(
                    "Auto-update {}",
                    if actual { "enabled" } else { "disabled" }
                )
            } else {
                "Auto-update state unchanged".to_string()
            },
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct ReleaseManifest {
    pub version: String,
    #[serde(default)]
    pub channel: Option<String>,
    #[serde(default)]
    pub release_date: Option<String>,
    #[serde(default)]
    pub notes_url: Option<String>,
    pub targets: std::collections::HashMap<String, ReleaseTarget>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ReleaseTarget {
    pub archive: String,
    pub sha256: String,
    #[serde(default)]
    pub sig: Option<String>,
    #[serde(default)]
    pub size: Option<u64>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct UpdateState {
    pub current_version: Option<String>,
    pub channel: String,
    pub last_check_epoch: Option<u64>,
    pub last_error: Option<String>,
    pub auto_update: bool,
    pub last_downloaded_version: Option<String>,
    pub last_known_remote: Option<String>,
    pub pending_restart: bool,
}

impl Default for UpdateState {
    fn default() -> Self {
        Self {
            current_version: None,
            channel: DEFAULT_CHANNEL.to_string(),
            last_check_epoch: None,
            last_error: None,
            auto_update: true,
            last_downloaded_version: None,
            last_known_remote: None,
            pending_restart: false,
        }
    }
}

impl UpdateState {
    fn reconcile_with_running(&mut self, running_version: &str) {
        if self.pending_restart {
            if let Some(downloaded) = &self.last_downloaded_version {
                if compare_versions(running_version, downloaded)
                    .map(|ordering| ordering != Ordering::Less)
                    .unwrap_or(false)
                {
                    self.pending_restart = false;
                    self.current_version = Some(running_version.to_string());
                }
            } else {
                self.pending_restart = false;
            }
        } else if self.current_version.is_none() {
            self.current_version = Some(running_version.to_string());
        }
    }
}

struct UpdateLock {
    file: File,
}

impl Drop for UpdateLock {
    fn drop(&mut self) {
        if let Err(err) = self.file.unlock() {
            debug!("Failed to release update lock: {err:?}");
        }
    }
}

fn default_target_id() -> Option<&'static str> {
    if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        Some("linux-x86_64-gnu")
    } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        Some("linux-aarch64-gnu")
    } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        Some("macos-aarch64")
    } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        Some("macos-x86_64")
    } else {
        None
    }
}

fn compare_versions(lhs: &str, rhs: &str) -> Option<Ordering> {
    let left = Version::parse(lhs).ok()?;
    let right = Version::parse(rhs).ok()?;
    Some(left.cmp(&right))
}

fn unix_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}
