import Foundation

/// Drives periodic status refreshes so the menu bar icon tracks recording /
/// meeting state even when the menu is closed. Loopback polls are cheap, so a
/// single steady cadence keeps the implementation simple and the icon live.
@MainActor
final class StatusPoller {
    private let interval: TimeInterval
    private let onTick: () async -> Void
    private var task: Task<Void, Never>?

    init(interval: TimeInterval = 1.5, onTick: @escaping () async -> Void) {
        self.interval = interval
        self.onTick = onTick
    }

    func start() {
        guard task == nil else { return }
        task = Task { [weak self] in
            while let self, !Task.isCancelled {
                await self.onTick()
                try? await Task.sleep(nanoseconds: UInt64(self.interval * 1_000_000_000))
            }
        }
    }

    func stop() {
        task?.cancel()
        task = nil
    }
}
