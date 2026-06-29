import Foundation

// MARK: - Wire types (subset of the daemon responses we care about)

/// Subset of `RecordingStatusResponse` (`crates/audetic/src/api/routes/recording.rs`).
struct RecordingStatusResponse: Decodable {
    let recording: Bool
    let phase: String
}

/// Subset of `MeetingStatusResponse` (`crates/audetic/src/api/routes/meetings.rs`).
struct MeetingStatusResponse: Decodable {
    let active: Bool
    let phase: String
    let title: String?
}

// MARK: - View model

struct DictationState: Equatable {
    var recording: Bool
    var phase: String
}

struct MeetingState: Equatable {
    var active: Bool
    var phase: String
    var title: String?
}

/// Combined snapshot the menu renders from.
struct AudeticStatus: Equatable {
    var daemonUp: Bool
    var dictation: DictationState
    var meeting: MeetingState

    static let offline = AudeticStatus(
        daemonUp: false,
        dictation: .init(recording: false, phase: "idle"),
        meeting: .init(active: false, phase: "idle", title: nil)
    )

    /// Which SF Symbol the menu bar icon should use.
    var iconName: String {
        if !daemonUp { return "waveform.slash" }
        if dictation.recording { return "mic.fill" }
        if meeting.active { return "person.wave.2.fill" }
        return "waveform"
    }

    var summaryLine: String {
        guard daemonUp else { return "Audetic not running" }
        if dictation.recording { return "Dictation: recording" }
        if meeting.active {
            if let title = meeting.title, !title.isEmpty {
                return "Meeting: \(title)"
            }
            return "Meeting: active"
        }
        return "Audetic: idle"
    }
}
