//! Workspace-owned assistant data, mounted independently of container lifetime.
use super::{Configuration, Runtime, compose};
use std::{fs, path::Path, process::Command};

const IGNORE: &str = "/.devcontainer/mounted_files/";

fn ignore(workspace: &Path) -> Result<(), String> {
    let path = workspace.join(".gitignore");
    let mut contents = match fs::read_to_string(&path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(format!("Cannot read .gitignore: {error}")),
    };
    if !contents.lines().any(|line| line.trim() == IGNORE) {
        if !contents.is_empty() && !contents.ends_with('\n') {
            contents.push('\n');
        }
        contents.push_str(IGNORE);
        contents.push('\n');
        fs::write(path, contents)
            .map_err(|error| format!("Cannot ignore assistant data: {error}"))?;
    }
    Ok(())
}

pub(super) fn prepare(configuration: &Configuration) -> Result<(), String> {
    ignore(&configuration.workspace)?;
    let root = configuration.workspace.join(".devcontainer/mounted_files");
    fs::create_dir_all(&root)
        .map_err(|error| format!("Cannot create assistant storage: {error}"))?;
    let ids = compose::compose(configuration, ["ps", "--all", "-q", &configuration.service])?;
    let id = String::from_utf8_lossy(&ids).trim().to_owned();
    if id.is_empty() {
        return Ok(());
    }
    if root.join("codex").exists() && root.join("share/opencode").exists() {
        return Ok(());
    }
    migrate(configuration, &root, &id, checked)
}

fn migrate(
    configuration: &Configuration,
    root: &Path,
    id: &str,
    mut run: impl FnMut(&mut Command, &str) -> Result<Vec<u8>, String>,
) -> Result<(), String> {
    // New containers already store their data on the host, even if startup failed
    // before initialization created the assistant directories. Do not start them
    // just to migrate data that is already persisted.
    let mounts = run(
        Command::new("docker").args(["inspect", "--format", "{{json .Mounts}}", id]),
        "inspect previous container storage",
    )?;
    let mounts: Vec<serde_json::Value> = serde_json::from_slice(&mounts)
        .map_err(|_| "Cannot parse previous container storage mounts")?;
    let canonical_root = root
        .canonicalize()
        .map_err(|error| format!("Cannot resolve assistant storage: {error}"))?;
    if mounts.iter().any(|mount| {
        mount["Type"] == "bind"
            && mount["Destination"] == compose::persistence_target()
            && mount["Source"].as_str().is_some_and(|source| {
                Path::new(source)
                    .canonicalize()
                    .is_ok_and(|source| source == canonical_root)
            })
    }) {
        return Ok(());
    }
    // Starting an old stopped container lets us resolve its actual user's home and
    // environment before Compose replaces it. No shell guesses about usernames.
    run(
        Command::new("docker").args(["start", id]),
        "start previous container for migration",
    )?;
    let runtime = Runtime {
        host_workspace: configuration.workspace.clone(),
        container_id: id.to_owned(),
        workspace_folder: configuration.workspace_folder.clone(),
        remote_user: configuration.remote_user.clone(),
    };
    for (relative, expression) in [
        ("codex", "${CODEX_HOME:-$home/.codex}"),
        (
            "share/opencode",
            "${XDG_DATA_HOME:-$home/.local/share}/opencode",
        ),
    ] {
        let destination = root.join(relative);
        if destination.exists() {
            continue;
        }
        let script = format!(
            r#"set -eu
home=$(getent passwd "$(id -u)" | cut -d: -f6)
[ -n "$home" ]
directory={expression}
if [ -d "$directory" ]; then printf '%s' "$directory"; fi"#
        );
        let output = run(
            runtime.command("sh").args(["-c", &script]),
            "resolve previous assistant data",
        )?;
        let source = String::from_utf8(output).map_err(|_| "Invalid assistant data path")?;
        if source.is_empty() {
            continue;
        }
        let parent = destination
            .parent()
            .ok_or("Invalid persistence destination")?;
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        let staging = tempfile::tempdir_in(parent).map_err(|error| error.to_string())?;
        run(
            Command::new("docker")
                .arg("cp")
                .arg(format!("{}:{source}/.", runtime.container_id))
                .arg(staging.path()),
            "migrate assistant data",
        )?;
        fs::rename(staging.path(), &destination)
            .map_err(|error| format!("Cannot save migrated assistant data: {error}"))?;
    }
    Ok(())
}

fn checked(command: &mut Command, operation: &str) -> Result<Vec<u8>, String> {
    let output = command
        .output()
        .map_err(|error| format!("Cannot {operation}: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "Cannot {operation}; see Docker configuration and storage permissions"
        ));
    }
    Ok(output.stdout)
}

pub(super) fn initialize(runtime: &Runtime) -> Result<(), String> {
    let identity = checked(
        runtime
            .command("sh")
            .args(["-c", "printf '%s:%s' \"$(id -u)\" \"$(id -g)\""]),
        "resolve assistant storage owner",
    )?;
    let identity = String::from_utf8(identity).map_err(|_| "Invalid container identity")?;
    if !identity
        .split(':')
        .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
        || identity.matches(':').count() != 1
    {
        return Err("Invalid container identity".into());
    }
    checked(Command::new("docker").args(["exec", "-u", "0", &runtime.container_id, "sh", "-c",
        "set -eu; mkdir -p /focus-editor-assistant-data/codex /focus-editor-assistant-data/share/opencode; chown -R \"$1\" /focus-editor-assistant-data; chmod 700 /focus-editor-assistant-data /focus-editor-assistant-data/codex /focus-editor-assistant-data/share /focus-editor-assistant-data/share/opencode", "assistant-storage", &identity]), "initialize assistant storage permissions")?;
    checked(runtime.command("sh").args(["-c", "test -w /focus-editor-assistant-data/codex && test -w /focus-editor-assistant-data/share/opencode"]), "verify writable assistant storage")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ignore_is_idempotent_and_preserves_existing_contents() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join(".gitignore"), "existing").unwrap();
        ignore(root.path()).unwrap();
        ignore(root.path()).unwrap();
        assert_eq!(
            fs::read_to_string(root.path().join(".gitignore")).unwrap(),
            format!("existing\n{IGNORE}\n")
        );
    }

    #[test]
    fn failed_container_with_persisted_mount_needs_no_start_or_migration() {
        let workspace = tempfile::tempdir().unwrap();
        let root = workspace.path().join(".devcontainer/mounted_files");
        fs::create_dir_all(&root).unwrap();
        let configuration = super::super::fallback_configuration(&Some(workspace.path().into()));
        for directories in [false, true] {
            if directories {
                fs::create_dir_all(root.join("codex")).unwrap();
            }
            let mut operations = Vec::new();
            migrate(&configuration, &root, "failed", |command, operation| {
                operations.push(operation.to_owned());
                assert_eq!(command.get_program(), "docker");
                assert_eq!(command.get_args().next().unwrap(), "inspect");
                Ok(serde_json::to_vec(&serde_json::json!([{
                    "Type":"bind", "Source":root, "Destination":compose::persistence_target()
                }]))
                .unwrap())
            })
            .unwrap();
            assert_eq!(operations, ["inspect previous container storage"]);
            assert!(!root.join("share/opencode").exists());
        }
    }

    #[test]
    fn legacy_or_other_workspace_storage_still_requires_successful_migration() {
        let workspace = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let configuration = super::super::fallback_configuration(&Some(workspace.path().into()));
        for mounts in [
            serde_json::json!([]),
            serde_json::json!([{"Type":"bind", "Source":other.path(),
                "Destination":compose::persistence_target()}]),
            serde_json::json!([{"Type":"volume", "Source":workspace.path(),
                "Destination":compose::persistence_target()}]),
        ] {
            let mut operations = Vec::new();
            let error = migrate(
                &configuration,
                workspace.path(),
                "legacy",
                |_, operation| {
                    operations.push(operation.to_owned());
                    if operation == "inspect previous container storage" {
                        Ok(serde_json::to_vec(&mounts).unwrap())
                    } else {
                        Err("previous container cannot start".into())
                    }
                },
            )
            .unwrap_err();
            assert_eq!(error, "previous container cannot start");
            assert_eq!(
                operations,
                [
                    "inspect previous container storage",
                    "start previous container for migration"
                ]
            );
        }
    }

    #[test]
    fn legacy_data_is_copied_without_overwriting_existing_data() {
        let workspace = tempfile::tempdir().unwrap();
        let root = workspace.path();
        fs::create_dir(root.join("codex")).unwrap();
        fs::write(root.join("codex/history"), "preserved").unwrap();
        let configuration = super::super::fallback_configuration(&Some(root.into()));
        let mut operations = Vec::new();
        migrate(&configuration, root, "legacy", |command, operation| {
            operations.push(operation.to_owned());
            match operation {
                "inspect previous container storage" => Ok(b"[]".to_vec()),
                "resolve previous assistant data" => {
                    Ok(b"/home/user/.local/share/opencode".to_vec())
                }
                "migrate assistant data" => {
                    let destination = Path::new(command.get_args().last().unwrap());
                    fs::write(destination.join("history"), "migrated").unwrap();
                    Ok(Vec::new())
                }
                "start previous container for migration" => Ok(Vec::new()),
                _ => panic!("unexpected operation: {operation}"),
            }
        })
        .unwrap();
        assert_eq!(
            fs::read_to_string(root.join("codex/history")).unwrap(),
            "preserved"
        );
        assert_eq!(
            fs::read_to_string(root.join("share/opencode/history")).unwrap(),
            "migrated"
        );
        assert_eq!(
            operations
                .iter()
                .filter(|op| *op == "migrate assistant data")
                .count(),
            1
        );
    }

    #[test]
    fn failed_inspection_does_not_start_the_previous_container() {
        let workspace = tempfile::tempdir().unwrap();
        let configuration = super::super::fallback_configuration(&Some(workspace.path().into()));
        for response in [
            Err("inspection failed".into()),
            Ok(b"invalid JSON".to_vec()),
        ] {
            let mut response = Some(response);
            let mut calls = 0;
            assert!(
                migrate(&configuration, workspace.path(), "failed", |_, _| {
                    calls += 1;
                    response.take().unwrap()
                })
                .is_err()
            );
            assert_eq!(calls, 1);
        }
    }
}
