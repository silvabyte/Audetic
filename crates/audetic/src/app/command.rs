/// Commands serialized by the daemon's single owner loop.
pub enum DaemonCommand {
    ToggleRecording(Option<crate::audio::JobOptions>),
    // The platform watcher is a later slice; S1 injects this typed event in tests.
    #[allow(dead_code)]
    DefaultInputSwitched,
    MeetingStart {
        options: Option<crate::meeting::MeetingStartOptions>,
        reply: tokio::sync::oneshot::Sender<anyhow::Result<crate::meeting::MeetingStartResult>>,
    },
    MeetingStop {
        reply: tokio::sync::oneshot::Sender<anyhow::Result<crate::meeting::MeetingStopResult>>,
    },
    MeetingCancel {
        reply: tokio::sync::oneshot::Sender<anyhow::Result<crate::meeting::MeetingStopResult>>,
    },
    MeetingConfirm {
        start_seconds: Option<f64>,
        end_seconds: Option<f64>,
        reply: tokio::sync::oneshot::Sender<anyhow::Result<crate::meeting::MeetingStopResult>>,
    },
    MeetingToggle {
        options: Option<crate::meeting::MeetingStartOptions>,
        reply: tokio::sync::oneshot::Sender<anyhow::Result<crate::meeting::ToggleOutcome>>,
    },
}
