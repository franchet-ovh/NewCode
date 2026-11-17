//! Builds use Compose-file paths; relative bind mounts use the host workspace.
use super::*;
use std::{
    fs,
    path::{Path, PathBuf},
};

struct Fixture {
    root: PathBuf,
    configuration: Configuration,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "focus-compose-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(root.join(".devcontainer/vscode/files")).unwrap();
        let compose = root.join(".devcontainer/compose.yml");
        fs::create_dir_all(root.join("repo")).unwrap();
        fs::write(root.join("settings.json"), "workspace settings").unwrap();
        fs::write(&compose, "services:\n  dev:\n    build:\n      context: .\n      dockerfile: ./vscode/Dockerfile\n      args:\n        PROJECT_NAME: ${COMPOSE_PROJECT_NAME}\n    volumes:\n      - .:/workspaces/test\n      - ./repo:/repo:ro\n      - ./settings.json:/etc/settings.json:ro\n      - cache:/cache\n  dependency:\n    image: scratch\n    volumes:\n      - type: bind\n        source: ./settings.json\n        target: /etc/settings.json\n        read_only: true\n        bind:\n          create_host_path: false\nvolumes:\n  cache:\n").unwrap();
        fs::write(
            root.join(".devcontainer/vscode/Dockerfile"),
            "FROM scratch\nCOPY ./vscode/files/toto.txt /tmp/toto.txt\n",
        )
        .unwrap();
        fs::write(
            root.join(".devcontainer/vscode/files/toto.txt"),
            "copy source stays in place\n",
        )
        .unwrap();
        let configuration = Configuration {
            name: "Test".into(),
            workspace: root.clone(),
            compose_files: vec![compose],
            service: "dev".into(),
            workspace_folder: "/workspaces/test".into(),
            remote_user: None,
        };
        Self {
            root,
            configuration,
        }
    }
    fn files(&self) -> Vec<PathBuf> {
        vec![
            self.root.join(".devcontainer/compose.yml"),
            self.root.join(".devcontainer/vscode/Dockerfile"),
            self.root.join(".devcontainer/vscode/files/toto.txt"),
        ]
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn overrides_only_builds_including_dependency_inline_and_remote_builds() {
    let fixture = Fixture::new();
    let build = json!({"context": fixture.root.join(".devcontainer"), "dockerfile":"vscode/Dockerfile",
        "args":{"EXAMPLE":"value"}, "additional_contexts":{"files":fixture.root.join(".devcontainer/vscode/files")}});
    let inline =
        json!({"context":fixture.root.join(".devcontainer"),"dockerfile_inline":"FROM scratch"});
    let remote = json!({"context":"https://example.invalid/repo.git","dockerfile":"Dockerfile"});
    let resolved = json!({"services":{
        "dev":{"build":build,"volumes":[{"source":"must-not-override","target":"/workspaces/test"}]},
        "dependency":{"build":inline}, "remote":{"build":remote}, "database":{"image":"example:latest"}}});
    let before = resolved.clone();
    let overlay = build_override(&fixture.configuration, &resolved)
        .unwrap()
        .unwrap();
    assert_eq!(
        overlay,
        json!({"services":{"dev":{"build":build},"dependency":{"build":inline},"remote":{"build":remote}}})
    );
    assert_eq!(resolved, before);
    let context = Path::new(
        overlay["services"]["dev"]["build"]["context"]
            .as_str()
            .unwrap(),
    );
    assert!(context.join("vscode/Dockerfile").is_file());
    assert!(context.join("vscode/files/toto.txt").is_file());
    assert!(!fixture.root.join("vscode/files/toto.txt").exists());
}

#[test]
fn image_only_services_need_no_override_and_missing_services_are_errors() {
    let fixture = Fixture::new();
    assert!(
        build_override(
            &fixture.configuration,
            &json!({"services":{"dev":{"image":"example:latest"}}})
        )
        .unwrap()
        .is_none()
    );
    assert!(
        build_override(&fixture.configuration, &json!({"services":{}}))
            .unwrap_err()
            .contains("not defined")
    );
}

#[test]
fn relative_binds_use_workspace_and_preserve_mount_options_and_other_volumes() {
    let fixture = Fixture::new();
    let absolute = fixture.root.join("absolute");
    let runtime = json!({"services": {
        "dev": {"volumes": [
            {"type":"bind", "source":"./repo", "target":"/repo", "read_only":true,
                "bind":{"propagation":"rshared", "create_host_path":false}},
            {"type":"bind", "source":absolute, "target":"/absolute"},
            {"type":"volume", "source":"cache", "target":"/cache", "volume":{"nocopy":true}},
            {"type":"volume", "target":"/anonymous"},
            {"type":"tmpfs", "target":"/tmp", "tmpfs":{"size":1024}}
        ]},
        "dependency": {"volumes": [
            {"type":"bind", "source":"./settings.json", "target":"/etc/settings.json", "read_only":true}
        ]},
        "database": {"volumes": [{"type":"volume", "source":"database", "target":"/data"}]}
    }});
    let before = runtime.clone();
    let mut overlay = json!({"services":{"dev":{"build":{"context":"preserved"}}}});
    workspace_mounts(&fixture.configuration, &runtime, &mut overlay).unwrap();
    let mut expected = runtime["services"]["dev"]["volumes"].clone();
    expected[0]["source"] = json!(fixture.root.join("repo"));
    assert_eq!(overlay["services"]["dev"]["volumes"], expected);
    assert_eq!(overlay["services"]["dev"]["build"]["context"], "preserved");
    assert_eq!(
        overlay["services"]["dependency"]["volumes"][0]["source"],
        json!(fixture.root.join("settings.json"))
    );
    assert!(overlay["services"].get("database").is_none());
    assert_eq!(runtime, before);
}

fn verify_resolved(configuration: &Configuration) -> Value {
    let original: Value =
        serde_json::from_slice(&compose(configuration, ["config", "--format", "json"]).unwrap())
            .unwrap();
    let merged: Value =
        serde_json::from_slice(&start(configuration, ["config", "--format", "json"]).unwrap())
            .unwrap();
    for (name, service) in merged["services"].as_object().unwrap() {
        let mounts: Vec<_> = service["volumes"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|mount| mount["target"] != persistence_target())
            .cloned()
            .collect();
        assert_eq!(json!(mounts), original["services"][name]["volumes"]);
    }
    assert_eq!(merged["name"], original["name"]);
    let service = &merged["services"][&configuration.service];
    let persistence = service["volumes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|mount| mount["target"] == persistence_target())
        .unwrap();
    assert_eq!(
        persistence["source"],
        json!(configuration.workspace.join(".devcontainer/mounted_files"))
    );
    if service["build"]["args"].get("PROJECT_NAME").is_some() {
        assert_eq!(service["build"]["args"]["PROJECT_NAME"], merged["name"]);
    }
    merged
}

#[test]
#[ignore = "requires Docker Compose plugin; resolves config only, never starts containers"]
fn nested_copy_context_and_workspace_mount_resolve_independently() {
    let fixture = Fixture::new();
    let before: Vec<_> = fixture
        .files()
        .iter()
        .map(|file| fs::read(file).unwrap())
        .collect();
    let merged = verify_resolved(&fixture.configuration);
    let service = &merged["services"]["dev"];
    let context = Path::new(service["build"]["context"].as_str().unwrap());
    assert_eq!(
        context.canonicalize().unwrap(),
        fixture.root.join(".devcontainer").canonicalize().unwrap()
    );
    assert!(
        context
            .join(service["build"]["dockerfile"].as_str().unwrap())
            .is_file()
    );
    assert!(context.join("vscode/files/toto.txt").is_file());
    assert_eq!(
        service["volumes"][0]["source"],
        fixture.root.to_str().unwrap()
    );
    for (target, source) in [("/repo", "repo"), ("/etc/settings.json", "settings.json")] {
        let mount = service["volumes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|mount| mount["target"] == target)
            .unwrap();
        assert_eq!(mount["source"], json!(fixture.root.join(source)));
        assert_eq!(mount["read_only"], true);
    }
    assert_eq!(
        merged["services"]["dependency"]["volumes"][0]["source"],
        json!(fixture.root.join("settings.json"))
    );
    for (file, bytes) in fixture.files().iter().zip(before) {
        assert_eq!(fs::read(file).unwrap(), bytes);
    }
}

#[test]
#[ignore = "requires Docker Compose plugin; resolves config only, never starts containers"]
fn compose_override_file_bind_sources_stay_relative_to_workspace() {
    let mut fixture = Fixture::new();
    fs::create_dir_all(fixture.root.join("other-repo")).unwrap();
    let override_file = fixture.root.join(".devcontainer/override.yml");
    fs::write(
        &override_file,
        "services:\n  dev:\n    volumes:\n      - ./other-repo:/repo:ro\n",
    )
    .unwrap();
    fixture
        .configuration
        .compose_files
        .push(override_file.clone());
    let before = fs::read(&override_file).unwrap();
    let merged = verify_resolved(&fixture.configuration);
    let mount = merged["services"]["dev"]["volumes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|mount| mount["target"] == "/repo")
        .unwrap();
    assert_eq!(mount["source"], json!(fixture.root.join("other-repo")));
    assert_eq!(mount["read_only"], true);
    assert_eq!(fs::read(override_file).unwrap(), before);
}

#[test]
#[ignore = "requires Docker Compose plugin; resolves config only, never starts containers"]
fn bundled_configuration_resolves_with_runtime_override_without_file_edits() {
    let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let configuration = crate::dev_container::load_configuration(&workspace)
        .unwrap()
        .unwrap();
    let merged = verify_resolved(&configuration);
    let build = &merged["services"][&configuration.service]["build"];
    let context = Path::new(build["context"].as_str().unwrap());
    let dockerfile = context.join(build["dockerfile"].as_str().unwrap_or("Dockerfile"));
    let files = [
        workspace.join(".devcontainer/devcontainer.json"),
        configuration.compose_files[0].clone(),
        dockerfile,
    ];
    let before: Vec<_> = files.iter().map(|file| fs::read(file).unwrap()).collect();
    verify_resolved(&configuration);
    for (file, bytes) in files.iter().zip(before) {
        assert_eq!(fs::read(file).unwrap(), bytes);
    }
}

#[test]
#[ignore = "requires Docker daemon; builds the configured image without recreating containers"]
fn bundled_image_build_succeeds_through_launcher() {
    let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let configuration = crate::dev_container::load_configuration(&workspace)
        .unwrap()
        .unwrap();
    start(&configuration, ["build", configuration.service.as_str()]).unwrap();
}
