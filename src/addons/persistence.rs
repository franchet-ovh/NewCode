//! Host-owned snapshots stay available when an attached container is replaced.
use focus_addon_sdk::{Context, Message};
use std::{
    io::Write,
    path::{Path, PathBuf},
};

pub(super) fn path(context: &Context) -> PathBuf {
    let name = match &context.runtime {
        None => "state-host.json".into(),
        Some(runtime) => {
            use sha2::{Digest, Sha256};
            let digest = Sha256::digest(runtime.workspace_folder.to_string_lossy().as_bytes());
            format!(
                "state-container-{}.json",
                digest
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect::<String>()
            )
        }
    };
    context.storage.join(name)
}

pub(super) fn load(context: &Context) -> serde_json::Value {
    if let Some(state) = std::fs::read(path(context))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
    {
        return state;
    }
    let sessions: serde_json::Value = std::fs::read(context.storage.join("sessions.json"))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_else(|| context.state["sessions"].clone());
    if let Some(session) = context
        .workspace
        .as_ref()
        .and_then(|root| sessions.get(root.to_string_lossy().as_ref()))
        .and_then(|v| v.as_str())
    {
        serde_json::json!({"session":session})
    } else {
        serde_json::Value::Null
    }
}

pub(super) fn accept(path: &Path, generation: u64, message: &Message) -> Result<(), String> {
    if message.method.as_deref() != Some("state.changed") {
        return Ok(());
    }
    let params = message.params.as_ref().ok_or("Missing state snapshot")?;
    if params["generation"].as_u64() != Some(generation) {
        return Ok(());
    }
    let state = &params["state"];
    if !state.is_object() {
        return Err("Invalid addon state snapshot".into());
    }
    let parent = path.parent().ok_or("Missing addon state directory")?;
    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let mut file = tempfile::NamedTempFile::new_in(parent).map_err(|e| e.to_string())?;
    serde_json::to_writer(&mut file, state).map_err(|e| e.to_string())?;
    file.flush().map_err(|e| e.to_string())?;
    file.persist(path).map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn snapshots_are_scoped_and_legacy_session_files_are_preserved() {
        let root = tempfile::tempdir().unwrap();
        let mut context = Context {
            generation: 7,
            workspace: Some(root.path().join("workspace")),
            storage: root.path().into(),
            ..Default::default()
        };
        let legacy =
            json!({context.workspace.as_ref().unwrap().to_string_lossy().as_ref(): "session-a"});
        std::fs::write(
            context.storage.join("sessions.json"),
            serde_json::to_vec(&legacy).unwrap(),
        )
        .unwrap();
        assert_eq!(load(&context), json!({"session":"session-a"}));
        let host_path = path(&context);
        accept(
            &host_path,
            7,
            &Message::notification(
                "state.changed",
                json!({"generation":6,"state":{"draft":"stale"}}),
            ),
        )
        .unwrap();
        assert!(!host_path.exists());
        accept(
            &host_path,
            7,
            &Message::notification(
                "state.changed",
                json!({"generation":7,"state":{"draft":"saved","session":"session-b"}}),
            ),
        )
        .unwrap();
        assert_eq!(load(&context)["draft"], "saved");
        context.runtime = Some(focus_addon_sdk::Runtime {
            host_workspace: root.path().into(),
            container_id: "first".into(),
            workspace_folder: "/workspace".into(),
            remote_user: None,
        });
        let container_path = path(&context);
        assert_ne!(host_path, container_path);
        context.runtime.as_mut().unwrap().container_id = "replacement".into();
        assert_eq!(container_path, path(&context));
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(
                &std::fs::read(context.storage.join("sessions.json")).unwrap()
            )
            .unwrap(),
            legacy
        );
    }
}
