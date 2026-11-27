//! Prepare and launch workspace addons through Docker, without host IP routing.
#[cfg(test)]
mod tests;
use super::{catalog, packages};
use crate::dev_container::Runtime;
use focus_addon_sdk::{Context, Manifest, Package};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

pub(super) const TARGETS: [&str; 2] = ["x86_64-unknown-linux-musl", "aarch64-unknown-linux-musl"];

fn target(os: &str, arch: &str) -> Result<&'static str, String> {
    match (os, arch) {
        ("Linux", "x86_64" | "amd64") => Ok(TARGETS[0]),
        ("Linux", "aarch64" | "arm64") => Ok(TARGETS[1]),
        _ => Err(format!(
            "Unsupported container platform {os}/{arch}; assistants support Linux x86-64 and ARM64"
        )),
    }
}

fn checked(
    command: &mut Command,
    input: Option<File>,
    cancel: &AtomicBool,
) -> Result<String, String> {
    if cancel.load(Ordering::Relaxed) {
        return Err("Addon launch cancelled".into());
    }
    command
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .map_err(|e| format!("Cannot prepare container addon: {e}"))?;
    let input_writer = if let Some(mut file) = input {
        let mut stdin = child.stdin.take().ok_or("Container input unavailable")?;
        Some(std::thread::spawn(move || {
            std::io::copy(&mut file, &mut stdin)
        }))
    } else {
        None
    };
    let stdout = child.stdout.take().ok_or("Container output unavailable")?;
    let stderr = child
        .stderr
        .take()
        .ok_or("Container diagnostics unavailable")?;
    let read = |stream: Box<dyn Read + Send>| {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let _ = stream.take(65536).read_to_end(&mut bytes);
            bytes
        })
    };
    let out = read(Box::new(stdout));
    let err = read(Box::new(stderr));
    let deadline = Instant::now() + Duration::from_secs(30);
    let status = loop {
        if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
            break status;
        }
        if cancel.load(Ordering::Relaxed) || Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err("Container addon preparation timed out or was cancelled".into());
        }
        std::thread::sleep(Duration::from_millis(25));
    };
    let output = out.join().unwrap_or_default();
    let errors = err.join().unwrap_or_default();
    if !status.success() {
        return Err(format!(
            "Container addon command failed ({status}): {}",
            crate::logging::redaction::sanitize(&String::from_utf8_lossy(&errors)).join("; ")
        ));
    }
    if let Some(writer) = input_writer {
        writer
            .join()
            .map_err(|_| "Container copy worker stopped")?
            .map_err(|e| format!("Container executable copy failed: {e}"))?;
    }
    String::from_utf8(output).map_err(|_| "Container returned non-UTF-8 paths".into())
}

fn digest(path: &Path) -> Result<String, String> {
    Ok(Sha256::digest(fs::read(path).map_err(|e| e.to_string())?)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}
fn cache_root() -> Result<PathBuf, String> {
    Ok(crate::app_paths::data_root()
        .map_err(|e| e.to_string())?
        .join("container-addons"))
}
fn package_directory(root: &Path, manifest: &Manifest, target: &str) -> Result<PathBuf, String> {
    semver::Version::parse(&manifest.version).map_err(|e| e.to_string())?;
    if !focus_addon_sdk::valid_id(&manifest.id) || !TARGETS.contains(&target) {
        return Err("Invalid container package identity".into());
    }
    Ok(root.join(&manifest.id).join(&manifest.version).join(target))
}

/// The normal local-addon picker also accepts matching portable runtime archives.
pub(super) fn import(source: &Path) -> Result<Option<String>, String> {
    let root = cache_root()?;
    let prepared = packages::prepare_archive_for_target(&root, source, None, None)?;
    let manifest = &prepared.manifest;
    let Some(target) = manifest.target.as_deref().filter(|t| TARGETS.contains(t)) else {
        return Ok(None);
    };
    if manifest.execution != focus_addon_sdk::Execution::Workspace {
        return Err("Container package must declare workspace execution".into());
    }
    let directory = package_directory(&root, manifest, target)?;
    fs::create_dir_all(&directory).map_err(|e| e.to_string())?;
    let _lease = packages::Lease::acquire(&directory, "package", false)?;
    let package = Package {
        id: manifest.id.clone(),
        name: manifest.name.clone(),
        description: manifest.description.clone(),
        publisher: manifest.publisher.clone(),
        version: manifest.version.clone(),
        protocol: manifest.protocol,
        editor_version_req: manifest.editor_version_req.clone(),
        target: target.into(),
        archive: "package.tar.gz".into(),
        source_release: None,
        size: fs::metadata(source).map_err(|e| e.to_string())?.len(),
        sha256: digest(source)?,
    };
    store(&directory, source, &package)?;
    Ok(Some(format!(
        "Imported {} {} container package for {target}. Restart the addon to use it.",
        manifest.name, manifest.version
    )))
}

fn store(directory: &Path, source: &Path, package: &Package) -> Result<(), String> {
    let mut archive = tempfile::NamedTempFile::new_in(directory).map_err(|e| e.to_string())?;
    std::io::copy(
        &mut File::open(source).map_err(|e| e.to_string())?,
        &mut archive,
    )
    .map_err(|e| e.to_string())?;
    archive
        .persist(directory.join(format!("{}.tar.gz", package.sha256)))
        .map_err(|e| e.to_string())?;
    let mut metadata = tempfile::NamedTempFile::new_in(directory).map_err(|e| e.to_string())?;
    serde_json::to_writer(&mut metadata, package).map_err(|e| e.to_string())?;
    metadata.flush().map_err(|e| e.to_string())?;
    metadata
        .persist(directory.join("package.json"))
        .map_err(|e| e.to_string())?;
    Ok(())
}

pub(super) struct Handle {
    runtime: Runtime,
    executable: String,
    launch: Option<String>,
}
impl Handle {
    pub(super) fn stop(mut self) {
        if let Some(launch) = self.launch.take() {
            cleanup(&self.runtime, &self.executable, &launch);
        }
    }
}
impl Drop for Handle {
    fn drop(&mut self) {
        if let Some(launch) = self.launch.take() {
            cleanup(&self.runtime, &self.executable, &launch);
        }
    }
}
fn cleanup(runtime: &Runtime, executable: &str, launch: &str) {
    let cancel = AtomicBool::new(false);
    let _ = checked(
        runtime
            .command(executable)
            .args(["--container-stop", launch]),
        None,
        &cancel,
    );
    // Keep the identity record until the supervisor confirms descendant cleanup.
    let _ = checked(
            runtime.command("sh").args([
                "-c",
                "i=0; while [ -e \"$1/supervisor\" ] && [ \"$i\" -lt 40 ]; do sleep 0.1; i=$((i+1)); done; [ ! -e \"$1/supervisor\" ] && rm -rf -- \"$1\"",
                "addon-cleanup",
                launch,
            ]),
            None,
            &cancel,
        );
}

struct Stage {
    runtime: Runtime,
    path: Option<String>,
}
impl Drop for Stage {
    fn drop(&mut self) {
        if let Some(path) = self.path.take() {
            let _ = checked(
                self.runtime
                    .command("sh")
                    .args(["-c", "rm -rf -- \"$1\"", "addon-cleanup", &path]),
                None,
                &AtomicBool::new(false),
            );
        }
    }
}

pub(super) fn prepare(
    manifest: &Manifest,
    context: &Context,
    cancel: &Arc<AtomicBool>,
    progress: impl Fn(String),
) -> Result<(Command, Context, Handle), String> {
    let runtime: Runtime = context
        .runtime
        .clone()
        .ok_or("Missing container runtime")?
        .into();
    progress("Detecting container platform…".into());
    let info = checked(
        runtime.command("sh").args([
            "-c",
            r#"set -eu
uname -s
uname -m
home=$(getent passwd "$(id -u)" | cut -d: -f6)
[ -n "$home" ] || home=$HOME
printf '%s\n' "${XDG_CACHE_HOME:-$home/.cache}"
"#,
        ]),
        None,
        cancel,
    )?;
    let mut lines = info.lines();
    let target = target(
        lines.next().unwrap_or_default(),
        lines.next().unwrap_or_default(),
    )?;
    let cache = lines
        .next()
        .filter(|s| Path::new(s).is_absolute())
        .ok_or("Container user has no absolute cache directory")?;
    let root = cache_root()?;
    let directory = package_directory(&root, manifest, target)?;
    fs::create_dir_all(&directory).map_err(|e| e.to_string())?;
    let lease_deadline = Instant::now() + Duration::from_secs(125);
    let _lease = loop {
        if let Ok(lease) = packages::Lease::acquire(&directory, "package", false) {
            break lease;
        }
        if cancel.load(Ordering::Relaxed) || Instant::now() >= lease_deadline {
            return Err("Container package is busy; retry Restart".into());
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    let cached: Option<Package> = fs::read(directory.join("package.json"))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok());
    let package = if let Some(package) = cached.filter(|p| {
        p.id == manifest.id
            && p.version == manifest.version
            && p.target == target
            && p.sha256.len() == 64
            && p.sha256.bytes().all(|b| b.is_ascii_hexdigit())
            && digest(&directory.join(format!("{}.tar.gz", p.sha256)))
                .ok()
                .as_deref()
                == Some(&p.sha256)
    }) {
        package
    } else {
        progress(format!(
            "Downloading {} {} for {target}…",
            manifest.name, manifest.version
        ));
        let package = catalog::runtime_package(&manifest.id, &manifest.version, target)?;
        let download = tempfile::NamedTempFile::new_in(&directory).map_err(|e| e.to_string())?;
        catalog::download(
            package
                .source_release
                .as_deref()
                .ok_or("Missing container release")?,
            &package,
            download.path(),
            &progress,
        )?;
        if !digest(download.path())?.eq_ignore_ascii_case(&package.sha256) {
            return Err("Container package checksum mismatch".into());
        }
        store(&directory, download.path(), &package)?;
        package
    };
    let archive = directory.join(format!("{}.tar.gz", package.sha256));
    let expected = package
        .source_release
        .as_deref()
        .map(|release| (&package, release));
    let prepared = packages::prepare_archive_for_target(&root, &archive, expected, Some(target))?;
    if prepared.manifest.id != manifest.id
        || prepared.manifest.version != manifest.version
        || prepared.manifest.protocol != manifest.protocol
        || prepared.manifest.execution != focus_addon_sdk::Execution::Workspace
    {
        return Err("Container package does not match installed addon".into());
    }
    progress(format!("Preparing {} inside container…", manifest.name));
    let remote = format!(
        "{cache}/focus-editor/addons/{}/{}/{}/{}",
        manifest.id, manifest.version, target, package.sha256
    );
    let executable = format!("{remote}/{}", prepared.manifest.executable.display());
    let local_executable = prepared.payload().join(&prepared.manifest.executable);
    let expected = digest(&local_executable)?;
    let existing = checked(
        runtime.command("sh").args([
            "-c",
            "sha256sum \"$1\" 2>/dev/null || true",
            "addon-check",
            &executable,
        ]),
        None,
        cancel,
    )?;
    if existing.split_whitespace().next() != Some(&expected) {
        let stage = checked(
            runtime.command("sh").args([
                "-c",
                "set -eu; umask 077; mkdir -p -- \"$1\"; chmod 700 \"$1\"; mktemp -d \"$1/stage.XXXXXXXX\"",
                "addon-stage",
                &format!("{cache}/focus-editor/addons"),
            ]),
            None,
            cancel,
        )?
        .trim_end()
        .to_owned();
        let mut guard = Stage {
            runtime: runtime.clone(),
            path: Some(stage.clone()),
        };
        let copied = checked(runtime.command("sh").args(["-c", "set -eu; umask 077; mkdir -p -- \"$(dirname \"$1\")\"; cat > \"$1\"; chmod 700 \"$1\"", "addon-copy", &format!("{stage}/{}", prepared.manifest.executable.display())]), Some(File::open(local_executable).map_err(|e| e.to_string())?), cancel);
        if let Err(error) = copied {
            return Err(error);
        }
        let copied_hash = checked(
            runtime.command("sha256sum").arg(format!(
                "{stage}/{}",
                prepared.manifest.executable.display()
            )),
            None,
            cancel,
        )?;
        if copied_hash.split_whitespace().next() != Some(&expected) {
            return Err("Copied container executable checksum mismatch".into());
        }
        checked(
            runtime.command("sh").args([
                "-c",
                "set -eu; mkdir -p -- \"$(dirname \"$2\")\"; rm -rf -- \"$2\"; mv -- \"$1\" \"$2\"",
                "addon-commit",
                &stage,
                &remote,
            ]),
            None,
            cancel,
        )?;
        guard.path = None;
    }
    let launch = checked(
        runtime.command("sh").args([
            "-c",
            "umask 077; mktemp -d \"$1/launch.XXXXXXXX\"",
            "addon-launch",
            &format!("{cache}/focus-editor/addons"),
        ]),
        None,
        cancel,
    )?
    .trim_end()
    .to_owned();
    // Only diagnostics are forwarded; provider credentials and XDG/CODEX_HOME stay container-local.
    let handle = Handle {
        runtime: runtime.clone(),
        executable: executable.clone(),
        launch: Some(launch.clone()),
    };
    let mut command = runtime.command_with_env(
        "sh",
        &[(
            "FOCUS_ADDON_LOG_LEVEL".into(),
            log::max_level().as_str().into(),
        )],
    );
    command.args(["-c", "home=$(getent passwd \"$(id -u)\" | cut -d: -f6); [ -n \"$home\" ] || home=$HOME; HOME=$home; export HOME; exec \"$@\"", "addon-runner", &executable, "--container-supervisor", &launch]);
    command.current_dir(&directory);
    let mut translated = context.clone();
    translated.workspace = Some(runtime.workspace_folder.clone());
    translated.storage = PathBuf::from(&launch).join("data");
    translated.runtime = None;
    Ok((command, translated, handle))
}
