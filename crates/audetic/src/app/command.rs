/// Commands serialized by the daemon's single owner loop.
pub enum DaemonCommand {
    SettledDeviceSwitch(crate::audio::SettledSwitch),
    CaptureStreamDied(crate::audio::stream_event::StreamDeath),
    AudioNoteStart {
        options: Option<crate::audio_notes::AudioNoteStartOptions>,
        reply:
            tokio::sync::oneshot::Sender<anyhow::Result<crate::audio_notes::AudioNoteStartResult>>,
    },
    AudioNoteStop {
        reply:
            tokio::sync::oneshot::Sender<anyhow::Result<crate::audio_notes::AudioNoteStopResult>>,
    },
    AudioNoteCancel {
        reply:
            tokio::sync::oneshot::Sender<anyhow::Result<crate::audio_notes::AudioNoteStopResult>>,
    },
    AudioNoteConfirm {
        start_seconds: Option<f64>,
        end_seconds: Option<f64>,
        reply:
            tokio::sync::oneshot::Sender<anyhow::Result<crate::audio_notes::AudioNoteStopResult>>,
    },
    AudioNoteToggle {
        options: Option<crate::audio_notes::AudioNoteStartOptions>,
        reply: tokio::sync::oneshot::Sender<anyhow::Result<crate::audio_notes::ToggleOutcome>>,
    },
}
