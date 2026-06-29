import Foundation
import AppKit

/// Thin async HTTP client for the local Audetic daemon.
///
/// The menu app is an independent consumer of the daemon, exactly like the
/// `audetic` CLI: it never reaches into daemon state, only talks to the HTTP
/// API. These constants mirror `crates/audetic-core/src/url.rs`
/// (`HOST` / `DEFAULT_PORT` / `API_PREFIX`) — keep them in sync. The daemon
/// only ever binds loopback, so there is nothing to discover.
enum Daemon {
    static let host = "127.0.0.1"
    static let port = 3737
    static let apiPrefix = "/api"

    static var apiBase: URL {
        URL(string: "http://\(host):\(port)\(apiPrefix)")!
    }

    /// Root URL serving the bundled web UI — `http://127.0.0.1:3737/`.
    /// Mirrors `audetic_core::url::app_url()`.
    static var webUIURL: URL {
        URL(string: "http://\(host):\(port)/")!
    }

    static func apiURL(_ path: String) -> URL {
        apiBase.appendingPathComponent(path.hasPrefix("/") ? String(path.dropFirst()) : path)
    }
}

struct DaemonClient {
    private let session: URLSession

    init() {
        let config = URLSessionConfiguration.ephemeral
        // Loopback calls are fast; fail quickly so an offline daemon flips the
        // menu to its offline state without a long hang.
        config.timeoutIntervalForRequest = 2
        config.timeoutIntervalForResource = 3
        config.waitsForConnectivity = false
        self.session = URLSession(configuration: config)
    }

    // MARK: - Toggles

    /// `POST /api/toggle` — dictation (voice-to-text). Empty body: the daemon
    /// applies its configured defaults for clipboard/auto-paste.
    func toggleDictation() async throws {
        try await postEmpty(Daemon.apiURL("/toggle"))
    }

    /// `POST /api/meetings/toggle` — start/stop a meeting recording.
    func toggleMeeting() async throws {
        try await postEmpty(Daemon.apiURL("/meetings/toggle"))
    }

    // MARK: - Status

    /// Polls both status endpoints concurrently. Any failure (daemon down,
    /// timeout, decode error) collapses to `.offline` so the menu degrades
    /// gracefully.
    func fetchStatus() async -> AudeticStatus {
        async let dictation = getDictationStatus()
        async let meeting = getMeetingStatus()

        let dict = await dictation
        let meet = await meeting

        // If neither endpoint answered, treat the daemon as offline.
        guard dict != nil || meet != nil else {
            return .offline
        }

        return AudeticStatus(
            daemonUp: true,
            dictation: dict ?? .init(recording: false, phase: "idle"),
            meeting: meet ?? .init(active: false, phase: "idle", title: nil)
        )
    }

    private func getDictationStatus() async -> DictationState? {
        guard let resp: RecordingStatusResponse = try? await getJSON(Daemon.apiURL("/status")) else {
            return nil
        }
        return DictationState(recording: resp.recording, phase: resp.phase)
    }

    private func getMeetingStatus() async -> MeetingState? {
        guard let resp: MeetingStatusResponse = try? await getJSON(Daemon.apiURL("/meetings/status")) else {
            return nil
        }
        return MeetingState(active: resp.active, phase: resp.phase, title: resp.title)
    }

    // MARK: - Web UI

    @MainActor
    func openWebUI() {
        NSWorkspace.shared.open(Daemon.webUIURL)
    }

    // MARK: - Plumbing

    private func postEmpty(_ url: URL) async throws {
        var request = URLRequest(url: url)
        request.httpMethod = "POST"
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        request.httpBody = Data("{}".utf8)
        let (_, response) = try await session.data(for: request)
        try Self.ensureOK(response, url: url)
    }

    private func getJSON<T: Decodable>(_ url: URL) async throws -> T {
        let (data, response) = try await session.data(from: url)
        try Self.ensureOK(response, url: url)
        return try JSONDecoder().decode(T.self, from: data)
    }

    private static func ensureOK(_ response: URLResponse, url: URL) throws {
        guard let http = response as? HTTPURLResponse else {
            throw DaemonError.invalidResponse(url)
        }
        guard (200..<300).contains(http.statusCode) else {
            throw DaemonError.httpStatus(http.statusCode, url)
        }
    }
}

enum DaemonError: Error, LocalizedError {
    case invalidResponse(URL)
    case httpStatus(Int, URL)

    var errorDescription: String? {
        switch self {
        case .invalidResponse(let url):
            return "Invalid response from \(url.absoluteString)"
        case .httpStatus(let code, let url):
            return "HTTP \(code) from \(url.absoluteString)"
        }
    }
}
