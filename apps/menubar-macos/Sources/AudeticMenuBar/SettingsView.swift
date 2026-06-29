import SwiftUI
import KeyboardShortcuts

/// Lets the user record the two global shortcuts. No defaults are shipped, so
/// these start empty until the user assigns them.
struct SettingsView: View {
    var body: some View {
        Form {
            Section {
                KeyboardShortcuts.Recorder("Toggle Dictation:", name: .toggleDictation)
                KeyboardShortcuts.Recorder("Toggle Meeting:", name: .toggleMeeting)
            } header: {
                Text("Global Keyboard Shortcuts")
            } footer: {
                Text("These work from any app. Leave blank to disable a shortcut.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
        }
        .formStyle(.grouped)
        .frame(width: 380)
        .padding()
    }
}
