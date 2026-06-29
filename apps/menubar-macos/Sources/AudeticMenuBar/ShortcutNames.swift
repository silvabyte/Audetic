import KeyboardShortcuts

// Strongly-typed names for the two global shortcuts the menu app registers.
//
// Deliberately NO `initial:` default — per the KeyboardShortcuts author's
// guidance, a distributed app should not steal a user's existing shortcuts.
// The user picks both in the Settings window; until then no global hotkey is
// registered. Point-and-click toggles in the menu always work regardless.
extension KeyboardShortcuts.Name {
    static let toggleDictation = Self("toggleDictation")
    static let toggleMeeting = Self("toggleMeeting")
}
