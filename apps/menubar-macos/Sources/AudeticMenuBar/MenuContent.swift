import SwiftUI
import KeyboardShortcuts

/// The dropdown contents of the menu bar item.
struct MenuContent: View {
    @Environment(AppState.self) private var state
    @Environment(\.openSettings) private var openSettings

    var body: some View {
        let status = state.status

        // Status header
        Text(status.summaryLine)
        if let error = state.lastActionError {
            Text("⚠︎ \(error)")
        }

        Divider()

        Button(action: { state.toggleDictation() }) {
            Text(dictationLabel(status))
        }
        .disabled(!status.daemonUp)
        .keyboardShortcutHint(.toggleDictation)

        Button(action: { state.toggleMeeting() }) {
            Text(meetingLabel(status))
        }
        .disabled(!status.daemonUp)
        .keyboardShortcutHint(.toggleMeeting)

        Divider()

        Button("Open Audetic") {
            state.openWebUI()
        }

        Button("Settings…") {
            openSettings()
        }

        Divider()

        Button("Quit Audetic Menu Bar") {
            NSApplication.shared.terminate(nil)
        }
        .keyboardShortcut("q")
    }

    private func dictationLabel(_ status: AudeticStatus) -> String {
        status.dictation.recording ? "Stop Dictation" : "Start Dictation"
    }

    private func meetingLabel(_ status: AudeticStatus) -> String {
        status.meeting.active ? "Stop Meeting" : "Start Meeting"
    }
}

private extension View {
    /// Show the recorded global shortcut next to a menu item, when one is set.
    @ViewBuilder
    func keyboardShortcutHint(_ name: KeyboardShortcuts.Name) -> some View {
        if let shortcut = KeyboardShortcuts.getShortcut(for: name),
           let swiftUIShortcut = shortcut.toSwiftUI {
            self.keyboardShortcut(swiftUIShortcut)
        } else {
            self
        }
    }
}
