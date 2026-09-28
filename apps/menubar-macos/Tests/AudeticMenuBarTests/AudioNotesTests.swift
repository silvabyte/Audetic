import XCTest
@testable import AudeticMenuBar

final class AudioNotesTests: XCTestCase {
    private func client() -> DaemonClient {
        let config = URLSessionConfiguration.ephemeral
        config.protocolClasses = [AudioNotesProtocol.self]
        return DaemonClient(session: URLSession(configuration: config))
    }

    func testBothSourcesUseOneCaptureEndpointWithSafeDefaults() async throws {
        for source in [CaptureSource.microphone, .microphoneAndSystem] {
            AudioNotesProtocol.handler = { request in
                XCTAssertEqual(request.url?.path, "/api/audio-notes/toggle")
                XCTAssertEqual(request.httpMethod, "POST")
                let body = try JSONSerialization.jsonObject(with: requestBody(request)) as! [String: Any]
                XCTAssertEqual(body["capture_source"] as? String, source.rawValue)
                XCTAssertEqual(body["copy_to_clipboard"] as? Bool, false)
                XCTAssertEqual(body["review_before_processing"] as? Bool, false)
                XCTAssertNil(body["auto_paste"], "Honor the daemon preference (off by default)")
                return Data("{}".utf8)
            }
            try await client().toggleNote(source: source)
        }
    }

    func testSingleStatusIncludesNoteIDAndReviewPhase() async {
        AudioNotesProtocol.handler = { request in
            XCTAssertEqual(request.url?.path, "/api/audio-notes/status")
            return Data(#"{"active":true,"phase":"review","note_id":42,"title":"Ideas","capture_degraded":false}"#.utf8)
        }
        let status = await client().fetchStatus()
        XCTAssertTrue(status.daemonUp)
        XCTAssertEqual(status.note.note_id, 42)
        XCTAssertEqual(status.summaryLine, "Ideas: review")
    }

    func testStatusFailureIsOffline() async {
        AudioNotesProtocol.handler = { _ in throw URLError(.cannotConnectToHost) }
        let status = await client().fetchStatus()
        XCTAssertEqual(status, .offline)
    }
}

private func requestBody(_ request: URLRequest) throws -> Data {
    if let data = request.httpBody { return data }
    guard let stream = request.httpBodyStream else { return Data() }
    stream.open()
    defer { stream.close() }
    var data = Data()
    var bytes = [UInt8](repeating: 0, count: 1024)
    while stream.hasBytesAvailable {
        let count = stream.read(&bytes, maxLength: bytes.count)
        if count < 0 { throw stream.streamError ?? URLError(.cannotDecodeRawData) }
        if count == 0 { break }
        data.append(contentsOf: bytes.prefix(count))
    }
    return data
}

private final class AudioNotesProtocol: URLProtocol {
    static var handler: ((URLRequest) throws -> Data)?

    override class func canInit(with request: URLRequest) -> Bool { true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }
    override func startLoading() {
        do {
            let data = try Self.handler!(request)
            let response = HTTPURLResponse(url: request.url!, statusCode: 200, httpVersion: nil, headerFields: nil)!
            client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
            client?.urlProtocol(self, didLoad: data)
            client?.urlProtocolDidFinishLoading(self)
        } catch {
            client?.urlProtocol(self, didFailWithError: error)
        }
    }
    override func stopLoading() {}
}
