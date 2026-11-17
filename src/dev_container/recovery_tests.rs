//! Opt-in recovery checks use isolated Compose projects, never the workspace container.
use super::*;
use std::{fs, os::unix::fs::MetadataExt};

struct Fixture {
    root: tempfile::TempDir,
    configuration: Configuration,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join(".devcontainer")).unwrap();
        fs::write(
            root.path().join(".devcontainer/devcontainer.json"),
            r#"{"dockerComposeFile":"compose.yml","service":"dev","workspaceFolder":"/workspace"}"#,
        )
        .unwrap();
        fs::write(root.path().join(".devcontainer/marker"), "recovery fixture").unwrap();
        fs::write(
            root.path().join(".devcontainer/compose.yml"),
            "services: {}\n",
        )
        .unwrap();
        let mut configuration = load_configuration(root.path()).unwrap().unwrap();
        // Match the host owner so initialization keeps temporary storage removable.
        let owner = fs::metadata(root.path()).unwrap();
        configuration.remote_user = Some(format!("{}:{}", owner.uid(), owner.gid()));
        Self {
            root,
            configuration,
        }
    }

    fn compose_file(&self, command: &str) {
        // A unique explicit project name avoids touching any existing workspace.
        let project = self
            .root
            .path()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .to_lowercase();
        fs::write(&self.configuration.compose_files[0], format!(
            "name: focus-recovery-{}-{}\nservices:\n  dev:\n    build: .\n    command: {command}\n",
            std::process::id(), project.trim_start_matches('.')
        )).unwrap();
    }

    fn dockerfile(&self, fail: bool) {
        // Use a local base image: no package installation or network access needed.
        fs::write(
            self.root.path().join(".devcontainer/Dockerfile"),
            format!(
                "FROM debian:trixie\nWORKDIR /workspace\nCOPY marker /marker\n{}",
                if fail { "RUN exit 42\n" } else { "" }
            ),
        )
        .unwrap();
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let result = compose(&self.configuration, ["down", "--rmi", "local"]);
        assert!(
            result.is_ok(),
            "Could not clean up recovery fixture: {result:?}"
        );
    }
}

#[test]
#[ignore = "requires Docker daemon and local debian:trixie image; creates and cleans isolated containers/images"]
fn rebuilds_recover_from_failed_build_and_unstartable_container() {
    for mode in [StartMode::Rebuild, StartMode::RebuildWithoutCache] {
        let fixture = Fixture::new();
        fixture.dockerfile(false);
        fixture.compose_file("[\"/nonexistent-recovery-command\"]");
        assert!(start(&fixture.configuration, StartMode::Normal).is_err());
        let previous = compose(&fixture.configuration, ["ps", "--all", "-q", "dev"]).unwrap();
        assert!(
            !previous.is_empty(),
            "failed start should leave a container to recover"
        );
        let storage = fixture.root.path().join(".devcontainer/mounted_files");
        assert!(!storage.join("codex").exists());
        fs::write(storage.join("sentinel"), "preserve me").unwrap();

        fixture.compose_file("[\"sleep\", \"infinity\"]");
        fixture.dockerfile(true);
        assert!(start(&fixture.configuration, mode).is_err());
        assert_eq!(
            compose(&fixture.configuration, ["ps", "--all", "-q", "dev"]).unwrap(),
            previous
        );

        fixture.dockerfile(false);
        let runtime = start(&fixture.configuration, mode).unwrap();
        assert_ne!(
            runtime.container_id,
            String::from_utf8_lossy(&previous).trim()
        );
        let output = runtime.command("cat").arg("/marker").output().unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, b"recovery fixture");
        assert_eq!(
            fs::read_to_string(storage.join("sentinel")).unwrap(),
            "preserve me"
        );
        assert!(storage.join("codex").is_dir());
        assert!(storage.join("share/opencode").is_dir());
    }
}
