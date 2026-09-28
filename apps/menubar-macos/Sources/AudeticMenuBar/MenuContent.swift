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

        if status.note.phase == "review" {
            Button("Process Audio Note") { state.confirmNote() }
            Button("Cancel Audio Note") { state.cancelNote() }
        } else if status.note.phase == "recording" {
            Button("Stop Audio Note") { state.toggleNote() }
                .keyboardShortcutHint(.toggleNote)
        } else {
            Button("Start Microphone Note") { state.toggleNote() }
                .disabled(!status.daemonUp || status.note.active)
                .keyboardShortcutHint(.toggleNote)
            Button("Start Microphone + System Audio Note") { state.toggleSystemNote() }
                .disabled(!status.daemonUp || status.note.active)
                .keyboardShortcutHint(.toggleSystemNote)
        }

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
