use super::*;
use eframe::egui;
use focus_addon_sdk::{Execution, Message};
use std::process::Output;

#[test]
fn platforms_are_selected_from_container_os_and_architecture() {
    assert_eq!(target("Linux", "x86_64").unwrap(), TARGETS[0]);
    assert_eq!(target("Linux", "aarch64").unwrap(), TARGETS[1]);
    assert!(target("Windows", "x86_64").is_err());
    assert!(target("Linux", "armv7l").is_err());
}

fn docker(args: &[&str]) -> Output {
    let output = Command::new("docker").args(args).output().unwrap();
    assert!(
        output.status.success(),
        "docker {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}
struct Container(String);
impl Drop for Container {
    fn drop(&mut self) {
        let _ = Command::new("docker").args(["rm", "-f", &self.0]).output();
    }
}
fn in_container(container: &Container, script: &str) -> String {
    String::from_utf8(docker(&["exec", &container.0, "sh", "-c", script]).stdout).unwrap()
}
fn start(
    directory: &Path,
    manifest: &Manifest,
    context: &Context,
) -> super::super::process::Process {
    let ctx = egui::Context::default();
    let mut process =
        super::super::process::Process::start(directory, manifest, context, &ctx).unwrap();
    let deadline = Instant::now() + Duration::from_secs(120);
    let mut last_state = String::new();
    loop {
        assert!(!process.expired(), "Addon exited before becoming ready");
        if let Some(error) = process.launch_error.take() {
            panic!("{error}");
        }
        while let Ok(event) = process.receiver.try_recv() {
            let message = event.unwrap();
            if message.id == Some(0) {
                process.ready = true;
            }
            if message.method.as_deref() == Some("panel.status") {
                let state = message.params.as_ref().unwrap()["state"]
                    .as_str()
                    .unwrap()
                    .to_owned();
                if state != last_state {
                    eprintln!("{}: {state}", manifest.id);
                    last_state = state.clone();
                }
                assert!(
                    !state.starts_with("Downloading"),
                    "Offline package cache was not reused"
                );
                if state == "Ready" {
                    return process;
                }
            }
        }
        assert!(
            Instant::now() < deadline,
            "Addon did not become ready: {last_state}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}
fn archive(root: &Path, name: &str, target: &str) -> (PathBuf, Manifest) {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    let manifest_path = repository.join("addons").join(name).join("addon.toml");
    let text = fs::read_to_string(&manifest_path).unwrap();
    let manifest: Manifest = toml::from_str(&text).unwrap();
    assert_eq!(manifest.execution, Execution::Workspace);
    let path = root.join(format!("{name}.tar.gz"));
    let text = format!("target = {target:?}\n{text}");
    let binary = repository
        .join("target")
        .join(target)
        .join("debug")
        .join(&manifest.executable);
    let stripped = root.join(format!("{name}-stripped"));
    let output = Command::new("strip")
        .args(["--strip-debug", "-o"])
        .arg(&stripped)
        .arg(binary)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    write_archive(
        &path,
        &manifest,
        &text,
        &stripped,
        &manifest_path.parent().unwrap().join(&manifest.icon),
    );
    (path, manifest)
}

fn write_archive(path: &Path, manifest: &Manifest, text: &str, binary: &Path, icon: &Path) {
    let encoder =
        flate2::write::GzEncoder::new(File::create(path).unwrap(), flate2::Compression::fast());
    let mut builder = tar::Builder::new(encoder);
    // Stripped binaries can be sparse; the importer accepts regular entries only.
    builder.sparse(false);
    let mut header = tar::Header::new_gnu();
    header.set_size(text.len() as u64);
    header.set_mode(0o600);
    header.set_cksum();
    builder
        .append_data(&mut header, "addon.toml", text.as_bytes())
        .unwrap();
    builder
        .append_path_with_name(binary, &manifest.executable)
        .unwrap();
    builder.append_path_with_name(icon, &manifest.icon).unwrap();
    builder.into_inner().unwrap().finish().unwrap();
}

#[test]
fn sparse_fixture_archives_import_as_regular_files_and_preserve_contents() {
    use std::io::{Seek, SeekFrom};

    let root = tempfile::tempdir().unwrap();
    let text = format!(
        "execution='workspace'\nid='fixture'\nname='Fixture'\nversion='1.0.0'\nprotocol=1\nexecutable='program'\nicon='icon.png'\ntarget='{}'\n",
        TARGETS[0]
    );
    let manifest: Manifest = toml::from_str(&text).unwrap();
    let binary = root.path().join("program");
    let mut file = File::create(&binary).unwrap();
    file.write_all(b"header").unwrap();
    file.seek(SeekFrom::Start(4 * 1024 * 1024)).unwrap();
    file.write_all(b"tail").unwrap();
    drop(file);
    let icon = root.path().join("icon.png");
    fs::write(&icon, b"icon").unwrap();
    let path = root.path().join("fixture.tar.gz");
    write_archive(&path, &manifest, &text, &binary, &icon);

    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(File::open(&path).unwrap()));
    let mut entries = Vec::new();
    for entry in archive.entries().unwrap() {
        let entry = entry.unwrap();
        assert_eq!(entry.header().entry_type(), tar::EntryType::Regular);
        entries.push(entry.path().unwrap().into_owned());
    }
    assert_eq!(
        entries,
        ["addon.toml", "program", "icon.png"].map(PathBuf::from)
    );
    let prepared = packages::prepare_archive_for_target(
        &root.path().join("cache"),
        &path,
        None,
        Some(TARGETS[0]),
    )
    .unwrap();
    let mut expected = vec![0; 4 * 1024 * 1024 + 4];
    expected[..6].copy_from_slice(b"header");
    expected[4 * 1024 * 1024..].copy_from_slice(b"tail");
    assert_eq!(
        fs::read(prepared.payload().join("program")).unwrap(),
        expected
    );
}
fn assert_backend_gone(container: &Container, name: &str) {
    let script = format!(
        "for pid in $(sed -n 's/^child=//p' /tmp/{name}-fixture.log); do test ! -e /proc/$pid || exit 1; done"
    );
    in_container(container, &script);
    assert_eq!(
        String::from_utf8(
            docker(&["inspect", "--format", "{{.State.Running}}", &container.0]).stdout
        )
        .unwrap()
        .trim(),
        "true"
    );
}

#[test]
#[ignore = "requires Docker, prebuilt musl assistant addons and a local Debian or Alpine image; never sends model prompts"]
fn live_container_assistants_smoke() {
    const MARKER: &str = "FOCUS_CONTAINER_SMOKE_ROOT";
    let Ok(root) = std::env::var(MARKER) else {
        let root = tempfile::tempdir().unwrap();
        let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "addons::container::tests::live_container_assistants_smoke",
                "--ignored",
                "--nocapture",
            ])
            .env(MARKER, root.path())
            .env("XDG_DATA_HOME", root.path().join("host-data"))
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    };
    let root = PathBuf::from(root);
    let image =
        std::env::var("FOCUS_CONTAINER_SMOKE_IMAGE").unwrap_or_else(|_| "debian:trixie".into());
    let target = target("Linux", std::env::consts::ARCH).unwrap();
    let fixture = root.join("assistant-fixture");
    let compiler = std::env::var(format!(
        "CARGO_TARGET_{}_LINKER",
        target.to_uppercase().replace('-', "_")
    ))
    .unwrap_or_else(|_| "musl-gcc".into());
    let result = Command::new("rustc")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/container-assistant.rs"))
        .args([
            "--edition=2024",
            "--target",
            target,
            "-C",
            &format!("linker={compiler}"),
            "-o",
        ])
        .arg(&fixture)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let output = docker(&[
        "run",
        "--rm",
        "-d",
        "--network",
        "none",
        "--user",
        "12345:12345",
        "-e",
        "HOME=/tmp/focus-home",
        "-e",
        "XDG_CACHE_HOME=/tmp/focus-cache",
        "-e",
        "XDG_DATA_HOME=/tmp/focus-home/share",
        "-e",
        "CODEX_HOME=/tmp/focus-home/codex",
        "-e",
        "PATH=/tmp/focus-tools:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
        "--entrypoint",
        "sh",
        &image,
        "-c",
        "mkdir -p /tmp/project /tmp/focus-tools /tmp/focus-home/codex /tmp/focus-home/share/opencode; printf '%s' '{\"fixture\":true}' > /tmp/focus-home/codex/auth.json; printf '%s' '{\"fixture\":true}' > /tmp/focus-home/share/opencode/auth.json; exec sleep 900",
    ]);
    let container = Container(String::from_utf8(output.stdout).unwrap().trim().into());
    for _ in 0..50 {
        if Command::new("docker")
            .args(["exec", &container.0, "test", "-d", "/tmp/project"])
            .status()
            .unwrap()
            .success()
        {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    for name in ["codex", "opencode"] {
        docker(&[
            "cp",
            fixture.to_str().unwrap(),
            &format!("{}:/tmp/focus-tools/{name}", container.0),
        ]);
        let (package, manifest) = archive(&root, name, target);
        assert!(
            import(&package)
                .unwrap_or_else(|error| panic!(
                    "Cannot import {name} smoke package for {target}: {error}"
                ))
                .is_some()
        );
        let installed = root.join("installed").join(name);
        fs::create_dir_all(&installed).unwrap();
        let context = Context {
            generation: 7,
            workspace: Some(root.join("project")),
            storage: root.join("state").join(name),
            state: serde_json::json!({"autostart":true}),
            runtime: Some(focus_addon_sdk::Runtime {
                host_workspace: root.join("project"),
                container_id: container.0.clone(),
                workspace_folder: "/tmp/project".into(),
                remote_user: None,
            }),
            ..Default::default()
        };
        let snapshot = super::super::persistence::path(&context);
        super::super::persistence::accept(&snapshot, 7, &Message::notification("state.changed", serde_json::json!({"generation":7,"state":{"draft":"unsent smoke draft","session":"fixture-session"}}))).unwrap();
        let process = start(&installed, &manifest, &context);
        process.stop_now();
        assert_backend_gone(&container, name);
        assert_eq!(
            super::super::persistence::load(&context)["draft"],
            "unsent smoke draft"
        );
        // A damaged executable in the container must be repaired from the validated host cache.
        let executable = in_container(
            &container,
            &format!("find /tmp/focus-cache -type f -name focus-addon-{name}"),
        );
        let executable = executable.trim();
        docker(&[
            "exec",
            &container.0,
            "sh",
            "-c",
            "printf broken > \"$1\"",
            "damage-fixture",
            executable,
        ]);
        let process = start(&installed, &manifest, &context);
        // Abruptly stop the supervisor: it must still reap the backend's detached children.
        in_container(
            &container,
            "for record in /tmp/focus-cache/focus-editor/addons/launch.*/supervisor; do read pid start < \"$record\"; kill -TERM \"$pid\"; done",
        );
        process.stop_now();
        assert_backend_gone(&container, name);
        // Cancellation during preparation must release its lease and clean temporary launches.
        let process = super::super::process::Process::start(
            &installed,
            &manifest,
            &context,
            &egui::Context::default(),
        )
        .unwrap();
        process
            .retire()
            .recv_timeout(Duration::from_secs(10))
            .unwrap();
        super::super::packages::Lease::acquire(installed.parent().unwrap(), name, false).unwrap();
        let leftovers = in_container(&container, "find /tmp/focus-cache -type d -name 'launch.*'");
        assert!(
            leftovers.trim().is_empty(),
            "Uncollected launch directories: {leftovers}"
        );
    }
    let requests = in_container(&container, "cat /tmp/opencode-fixture.log");
    assert!(requests.contains("--hostname 127.0.0.1"));
    assert!(requests.contains("/global/health"));
    assert!(requests.contains("/event"));
    for file in [
        "/tmp/focus-home/codex/auth.json",
        "/tmp/focus-home/share/opencode/auth.json",
    ] {
        assert_eq!(
            String::from_utf8(docker(&["exec", &container.0, "cat", file]).stdout).unwrap(),
            "{\"fixture\":true}"
        );
    }
}
