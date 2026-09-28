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

    init(session: URLSession) {
        self.session = session
    }

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

    /// Source is acquisition, not classification. Delivery remains opt-in.
    func toggleNote(source: CaptureSource) async throws {
        try await postJSON(Daemon.apiURL("/audio-notes/toggle"), body: [
            "capture_source": source.rawValue,
            "review_before_processing": false,
            "copy_to_clipboard": false,
        ])
    }

    func confirmNote() async throws {
        try await postJSON(Daemon.apiURL("/audio-notes/confirm"))
    }

    func cancelNote() async throws {
        try await postJSON(Daemon.apiURL("/audio-notes/cancel"))
    }

    // MARK: - Status

    /// A single status endpoint owns the capture lifecycle.
    func fetchStatus() async -> AudeticStatus {
        guard let note: AudioNoteStatusResponse = try? await getJSON(Daemon.apiURL("/audio-notes/status")) else {
            return .offline
        }
        return AudeticStatus(daemonUp: true, note: note)
    }

    // MARK: - Web UI

    @MainActor
    func openWebUI() {
        NSWorkspace.shared.open(Daemon.webUIURL)
    }

    // MARK: - Plumbing

    private func postJSON(_ url: URL, body: [String: Any] = [:]) async throws {
        var request = URLRequest(url: url)
        request.httpMethod = "POST"
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        request.httpBody = try JSONSerialization.data(withJSONObject: body)
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
