# Keybindings Manager Module

## Status: IMPLEMENTED

## Overview
A CLI subcommand (`audetic keybind`) that helps users configure Hyprland keybindings 
for Audetic with minimal friction, conflict detection, and safe file modifications.

## CLI Interface

```bash
audetic keybind                    # Interactive guided setup (default)
audetic keybind install            # Auto-install with defaults (SUPER+R)
audetic keybind install --key "SUPER SHIFT, R"  # Custom keybinding
audetic keybind install --dry-run  # Preview changes without applying
audetic keybind uninstall          # Remove Audetic keybindings
audetic keybind uninstall --dry-run
audetic keybind status             # Show current keybinding status
```

## Features

### Config Discovery
- Searches for Hyprland configs in order: `bindings.conf`, `keybinds.conf`, `hyprland.conf`
- Parses `source =` directives to find all sourced config files
- Prefers writing to dedicated bindings file over main config

### Binding Parser
Parses Hyprland bind syntax variants:
- `bind = MOD, KEY, exec, cmd`
- `bindd = MOD, KEY, Description, exec, cmd`
- `bindr = MOD, KEY, exec, cmd` (release trigger)
- `bindl = MOD, KEY, exec, cmd` (locked)
- `bindld = MOD, KEY, Description, exec, cmd`

### Conflict Detection
- Builds lookup of all existing keybindings from all config files
- Checks proposed binding against existing ones
- Offers alternatives or custom input on conflict

### Backup Management
- Creates timestamped backups before any modification
- Stores in `~/.local/share/audetic/keybind-backups/`
- Keeps last 3 backups, rotates older ones

### Safe Writing
- Uses section markers to identify Audetic bindings
- Updates in place if section exists, otherwise appends
- Always uses `bindd` format for description support

## File Structure

```
src/
├── cli/
│   └── keybind.rs          # CLI subcommand handler
└── keybind/
    ├── mod.rs              # Public API and types
    ├── discovery.rs        # Config file discovery
    ├── parser.rs           # Hyprland binding parser
    ├── backup.rs           # Backup management
    └── writer.rs           # Safe file modification
```

## Example Output

### Status
```
$ audetic keybind status

Audetic Keybinding Status
=========================

Status: INSTALLED

Keybinding: SUPER + R
Description: Audetic
Command: curl -X POST http://127.0.0.1:3737/toggle
Location: ~/.config/hypr/bindings.conf:60
```

### Dry Run
```
$ audetic keybind install --key "SUPER ALT, V" --dry-run

Dry run - would add to ~/.config/hypr/bindings.conf:
  # Audetic voice-to-text (managed by audetic keybind)
  bindd = SUPER ALT, V, Audetic, exec, curl -X POST http://127.0.0.1:3737/toggle
```

### Conflict Detection
```
$ audetic keybind install --dry-run

Conflict detected:
  SUPER + R is already bound in ~/.config/hypr/bindings.conf:60
Error: Keybinding SUPER + R conflicts with existing binding. Use --key to specify a different key.
```

## Future Extensibility

1. **Other Window Managers**: Abstract behind a `KeybindBackend` trait
2. **Multiple Actions**: Support more than just toggle (e.g., transcribe-clipboard)
3. **Restore Command**: Add `audetic keybind restore` to restore from backup
