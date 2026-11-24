use std::process::Command;

#[test]
fn compatibility_preflight_runs_without_opening_an_editor_or_modifying_packages() {
    let directory = tempfile::tempdir().unwrap();
    let manifest = directory.path().join("addon.toml");
    for (requirement, success) in [
        (">=0.4.8, <0.5.0", true),
        (">=99.0.0", false),
        ("invalid", false),
    ] {
        let text = format!(
            "id='fixture'\nname='Fixture'\nversion='1.0.0'\nprotocol=1\neditor_version_req='{requirement}'\nexecutable='program'\nicon='icon.png'\n"
        );
        std::fs::write(&manifest, &text).unwrap();
        let output = Command::new(env!("CARGO_BIN_EXE_focus-editor"))
            .arg("--check-addon-compatibility")
            .arg(&manifest)
            .output()
            .unwrap();
        assert_eq!(
            output.status.success(),
            success,
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(std::fs::read_to_string(&manifest).unwrap(), text);
    }
    assert!(
        !Command::new(env!("CARGO_BIN_EXE_focus-editor"))
            .arg("--check-addon-compatibility")
            .status()
            .unwrap()
            .success()
    );
}
