import SwiftUI
import KeyboardShortcuts

/// Lets the user record the two global shortcuts. No defaults are shipped, so
/// these start empty until the user assigns them. Everything else lives in the
/// web UI, linked from the top section.
struct SettingsView: View {
    @Environment(\.openURL) private var openURL

    var body: some View {
        Form {
            Section {
                Button("Open Audetic Settings…") {
                    openURL(Daemon.webSettingsURL)
                }
            } header: {
                Text("Audetic")
            } footer: {
                Text("Provider, capture, post-processing, and integrations are configured in the Audetic web app.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }

            Section {
                KeyboardShortcuts.Recorder("Microphone Note:", name: .toggleNote)
                KeyboardShortcuts.Recorder("Microphone + System Audio:", name: .toggleSystemNote)
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
