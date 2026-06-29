import SwiftUI

@main
struct AudeticMenuBarApp: App {
    @State private var state = AppState()

    init() {
        // Kick off status polling as soon as the app launches so the icon is
        // accurate before the menu is ever opened.
        _state.wrappedValue.startPolling()
    }

    var body: some Scene {
        MenuBarExtra {
            MenuContent()
                .environment(state)
        } label: {
            Image(systemName: state.status.iconName)
        }
        .menuBarExtraStyle(.menu)

        Settings {
            SettingsView()
        }
    }
}
