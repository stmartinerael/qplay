use std::process::Command;

#[test]
fn help_lists_playback_options() {
    let output = Command::new(env!("CARGO_BIN_EXE_qplay"))
        .arg("--help")
        .output()
        .unwrap();
    assert!(output.status.success());
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(text.contains("--clip-seconds"));
    assert!(text.contains("--transition"));
}

#[test]
fn empty_directory_reports_no_videos_without_opening_a_window() {
    let dir = tempfile::tempdir().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_qplay"))
        .arg(dir.path())
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("No video files found"));
}

#[test]
fn invalid_dimensions_are_rejected_before_opening_a_window() {
    let output = Command::new(env!("CARGO_BIN_EXE_qplay"))
        .args(["example.mp4", "--width", "10"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("at least 240x160"));
}
