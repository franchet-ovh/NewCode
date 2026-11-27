//! Exercise argument resolution and headless startup through the shipped binary.
use serde_json::{Value, json};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

fn fixture(root: &Path) -> Value {
    fs::create_dir(root.join("bin")).unwrap();
    fs::create_dir_all(root.join("source/sub project")).unwrap();
    let docker = root.join("bin/docker");
    fs::write(&docker, include_str!("fixtures/attachment-docker.sh")).unwrap();
    fs::set_permissions(docker, fs::Permissions::from_mode(0o700)).unwrap();
    json!({"Id":"pinned-id", "State":{"Running":true,"Paused":false},
        "Mounts":[{"Type":"bind","Source":root.join("source"),"Destination":"/workspace"}]})
}

fn command(root: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_focus-editor"));
    command
        .args([
            "--foreground",
            "--headless",
            "--attach-container",
            "my-container",
            "--container-workspace",
            "/workspace/sub project",
        ])
        .env("FOCUS_ATTACH_TEST_ROOT", root)
        .env("PATH", root.join("bin"))
        .env("XDG_DATA_HOME", root.join("data"))
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY")
        .stdin(Stdio::null());
    command
}

struct Editor(Child);
impl Drop for Editor {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn headless_attachment_discovers_workspace_and_leaves_project_files_untouched() {
    let root = tempfile::tempdir().unwrap();
    let data = fixture(root.path());
    fs::write(root.path().join("inspection.json"), data.to_string()).unwrap();
    let workspace = root.path().join("source/sub project");
    fs::write(
        workspace.join(".devcontainer.json"),
        "invalid configuration",
    )
    .unwrap();
    fs::write(workspace.join(".gitignore"), "existing\n").unwrap();
    let log = root.path().join("editor.log");
    let mut editor = Editor(
        command(root.path())
            .stdout(Stdio::null())
            .stderr(fs::File::create(&log).unwrap())
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(15);
    let record: Value = loop {
        let directory = root.path().join("data/focus-editor/runtime/instances");
        let record = fs::read_dir(directory).ok().and_then(|entries| {
            entries.filter_map(Result::ok).find_map(|entry| {
                fs::read(entry.path())
                    .ok()
                    .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            })
        });
        if let Some(record) = record {
            break record;
        }
        assert!(
            editor.0.try_wait().unwrap().is_none() && Instant::now() < deadline,
            "Editor failed to initialize: {}",
            fs::read_to_string(&log).unwrap()
        );
        std::thread::sleep(Duration::from_millis(25));
    };
    assert_eq!(
        PathBuf::from(record["workspace"].as_str().unwrap()),
        workspace.canonicalize().unwrap()
    );
    assert_eq!(record["mode"], "headless");
    let origin = record["endpoint"].as_str().unwrap();
    let client = reqwest::blocking::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap();
    let info: Value = client
        .get(format!("{origin}/api/info"))
        .header("Origin", origin)
        .header("X-Focus-Token", record["token"].as_str().unwrap())
        .send()
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .unwrap();
    assert!(info["title"].as_str().unwrap().contains("Devcontainer"));
    let response = client
        .post(format!("{origin}/api/manage/stop"))
        .header("Origin", origin)
        .header("X-Focus-Token", record["token"].as_str().unwrap())
        .json(&json!({}))
        .send()
        .unwrap();
    assert!(response.status().is_success());
    while editor.0.try_wait().unwrap().is_none() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(25));
    }
    let docker_log = fs::read_to_string(root.path().join("docker.log")).unwrap();
    assert!(docker_log.contains("exec -it"));
    assert!(
        docker_log
            .lines()
            .all(|line| line.starts_with("inspect ") || line.starts_with("exec "))
    );
    assert_eq!(
        fs::read_to_string(workspace.join(".gitignore")).unwrap(),
        "existing\n"
    );
    assert_eq!(
        fs::read_to_string(workspace.join(".devcontainer.json")).unwrap(),
        "invalid configuration"
    );
    assert!(!workspace.join(".devcontainer").exists());
}

#[test]
fn attachment_failure_exits_before_starting_a_local_session() {
    let root = tempfile::tempdir().unwrap();
    let mut data = fixture(root.path());
    data["State"]["Running"] = json!(false);
    fs::write(root.path().join("inspection.json"), data.to_string()).unwrap();
    let output = command(root.path()).output().unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("must be running"));
    assert_eq!(
        fs::read_to_string(root.path().join("docker.log")).unwrap(),
        "inspect --type\n"
    );
    assert!(!root.path().join("data").exists());
}
