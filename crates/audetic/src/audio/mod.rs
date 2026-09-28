pub mod audio_mixer;
pub mod audio_source;
pub mod capture_recovery;
pub(crate) mod device_watcher;
pub mod input_device;
pub mod mic_source;
pub mod resample;
pub mod stream_event;
pub mod system_source;
#[cfg(any(target_os = "macos", test))]
pub(crate) mod system_tap;

pub use device_watcher::SettledSwitch;
