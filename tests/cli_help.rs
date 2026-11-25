use std::process::Command;

#[test]
fn help_exits_without_runtime_dependencies_or_state_files() {
    let data = tempfile::tempdir().unwrap();
    for (binary, name) in [
        (env!("CARGO_BIN_EXE_focus-editor"), "focus-editor"),
        (
            env!("CARGO_BIN_EXE_focus-editor-server"),
            "focus-editor-server",
        ),
    ] {
        for flag in ["-h", "--help"] {
            let output = Command::new(binary)
                .arg(flag)
                .env_remove("DISPLAY")
                .env_remove("WAYLAND_DISPLAY")
                .env_remove("FOCUS_EDITOR_SERVER_USERNAME")
                .env_remove("FOCUS_EDITOR_SERVER_PASSWORD")
                .env("PATH", "")
                .env("XDG_DATA_HOME", data.path())
                .env("XDG_CONFIG_HOME", data.path())
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            let help = String::from_utf8(output.stdout).unwrap();
            assert!(help.contains(&format!("Usage: {name}")));
            assert!(help.contains("--log-level") && help.contains("--help"));
        }
    }
    assert_eq!(std::fs::read_dir(data.path()).unwrap().count(), 0);
}
