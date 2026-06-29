import Foundation
import SwiftUI
import KeyboardShortcuts

/// Owns the current status snapshot, the daemon client, and the global
/// shortcut listeners. The menu and settings views observe this.
@MainActor
@Observable
final class AppState {
    private(set) var status: AudeticStatus = .offline
    /// Transient banner for the last toggle error, if any.
    private(set) var lastActionError: String?

    private let client = DaemonClient()
    private var poller: StatusPoller?

    init() {
        registerShortcutListeners()
    }

    func startPolling() {
        let poller = StatusPoller { [weak self] in
            await self?.refresh()
        }
        self.poller = poller
        poller.start()
    }

    func refresh() async {
        let next = await client.fetchStatus()
        if next != status {
            status = next
        }
    }

    // MARK: - Actions

    func toggleDictation() {
        Task { await perform { try await self.client.toggleDictation() } }
    }

    func toggleMeeting() {
        Task { await perform { try await self.client.toggleMeeting() } }
    }

    func openWebUI() {
        client.openWebUI()
    }

    private func perform(_ action: @escaping () async throws -> Void) async {
        do {
            try await action()
            lastActionError = nil
            // Give the daemon a beat to flip phase, then refresh.
            try? await Task.sleep(nanoseconds: 150_000_000)
            await refresh()
        } catch {
            lastActionError = error.localizedDescription
        }
    }

    // MARK: - Global shortcuts

    private func registerShortcutListeners() {
        KeyboardShortcuts.onKeyUp(for: .toggleDictation) { [weak self] in
            self?.toggleDictation()
        }
        KeyboardShortcuts.onKeyUp(for: .toggleMeeting) { [weak self] in
            self?.toggleMeeting()
        }
    }
}
