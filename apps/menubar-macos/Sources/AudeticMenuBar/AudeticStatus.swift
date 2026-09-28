import Foundation

/// Single capture lifecycle returned by GET /api/audio-notes/status.
struct AudioNoteStatusResponse: Decodable, Equatable {
    var active: Bool
    var phase: String
    var title: String?
    var note_id: Int?
    var capture_degraded: Bool?
    var last_error: String?
}

enum CaptureSource: String {
    case microphone
    case microphoneAndSystem = "microphone_and_system"
}

struct AudeticStatus: Equatable {
    var daemonUp: Bool
    var note: AudioNoteStatusResponse

    static let offline = AudeticStatus(
        daemonUp: false,
        note: .init(active: false, phase: "idle")
    )

    var iconName: String {
        if !daemonUp { return "waveform.slash" }
        if note.phase == "recording" { return "mic.fill" }
        return "waveform"
    }

    var summaryLine: String {
        guard daemonUp else { return "Audetic not running" }
        if let error = note.last_error { return "Audio note: \(error)" }
        let title = note.title.flatMap { $0.isEmpty ? nil : $0 } ?? "Audio note"
        let degraded = note.capture_degraded == true ? " (microphone only)" : ""
        return "\(title): \(note.phase)\(degraded)"
    }
}
