use std::sync::Arc;

/// Audio Capture source whose live stream ended unexpectedly.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureSource {
    Dictation,
    MeetingMicrophone,
    SystemTap,
}

/// Monotonically increasing identity assigned to a successfully built stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamGeneration(pub u64);

impl From<u64> for StreamGeneration {
    fn from(value: u64) -> Self {
        Self(value)
    }
}

impl StreamGeneration {
    pub(crate) fn next(self) -> anyhow::Result<Self> {
        Ok(Self(self.0.checked_add(1).ok_or_else(|| {
            anyhow::anyhow!("Stream Generation overflowed")
        })?))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamDeath {
    pub source: CaptureSource,
    pub generation: StreamGeneration,
}

/// Receives platform-neutral stream events without exposing daemon transport.
pub(crate) type StreamEventSink = Arc<dyn Fn(StreamDeath) + Send + Sync + 'static>;
