//! Smoke-test the platform-specific `SystemAudioSource` end-to-end.
//!
//! ```
//! cargo run --example system_audio_smoke --release -- 5 /tmp/sys.wav
//! ```
//!
//! Captures system audio for N seconds (default 5) at 16 kHz mono f32 and
//! writes the result as a WAV. macOS will prompt for Screen Recording /
//! System Audio Recording permission on the first run; subsequent runs use
//! the saved grant. If permission is denied the resulting buffer is silent
//! and the source surfaces a warning — open System Settings → Privacy &
//! Security → Screen Recording and re-enable audetic.
//!
//! Play something audible on this Mac while the capture is running.

use audetic::audio::audio_source::AudioSource;
use audetic::audio::system_source::SystemAudioSource;
use hound::{SampleFormat, WavSpec, WavWriter};
use std::time::Duration;

const TARGET_RATE: u32 = 16_000;

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let mut args = std::env::args().skip(1);
    let secs: u64 = args
        .next()
        .as_deref()
        .unwrap_or("5")
        .parse()
        .expect("first arg must be an integer (seconds)");
    let path = args.next().unwrap_or_else(|| "/tmp/sys.wav".to_string());

    println!("→ capturing system audio for {secs}s → {path}");
    println!("  play something audible NOW.");

    let mut source = SystemAudioSource::new(TARGET_RATE);
    source.start()?;

    std::thread::sleep(Duration::from_secs(secs));

    let samples = source.stop()?;
    println!("✓ captured {} samples @ {} Hz", samples.len(), TARGET_RATE);

    if samples.is_empty() {
        println!("  (empty — see warnings above; usually a permission issue)");
        return Ok(());
    }

    let peak = samples.iter().cloned().fold(0.0_f32, |a, b| a.max(b.abs()));
    let rms = (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt();
    println!("  peak={peak:.4} rms={rms:.4}");

    let spec = WavSpec {
        channels: 1,
        sample_rate: TARGET_RATE,
        bits_per_sample: 32,
        sample_format: SampleFormat::Float,
    };
    let mut writer = WavWriter::create(&path, spec)?;
    for s in &samples {
        writer.write_sample(*s)?;
    }
    writer.finalize()?;
    println!("✓ wrote {path}");
    Ok(())
}
