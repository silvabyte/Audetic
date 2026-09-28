//! Real binary argument/error checks, requiring no daemon or external provider.
use std::process::Command;

fn cli(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_audetic"))
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn notes_exposes_complete_unified_command_surface() {
    let output = cli(&["notes", "--help"]);
    assert!(output.status.success());
    let help = String::from_utf8(output.stdout).unwrap();
    for command in [
        "start", "stop", "toggle", "confirm", "cancel", "status", "list", "show", "import",
        "delete", "retry", "process", "copy",
    ] {
        assert!(help.contains(command), "missing {command}");
    }
}

#[test]
fn obsolete_commands_are_removed() {
    for command in ["history", "meeting", "transcribe"] {
        let output = cli(&[command]);
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("unrecognized subcommand"));
    }
}

#[test]
fn missing_import_is_rejected_before_network_access() {
    let output = cli(&[
        "notes",
        "import",
        "/definitely-not-an-audetic-test-fixture/audio.wav",
    ]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("Failed to open"));
}

#[test]
fn invalid_trim_and_note_ids_are_rejected_before_network_access() {
    for args in [
        vec!["notes", "confirm", "--start", "NaN"],
        vec!["notes", "confirm", "--start", "3", "--end", "2"],
        vec!["notes", "show", "0"],
    ] {
        let output = cli(&args);
        assert!(!output.status.success());
        assert!(!String::from_utf8_lossy(&output.stderr).contains("Failed to connect"));
    }
}
