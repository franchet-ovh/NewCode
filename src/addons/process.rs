//! Host and container transports share protocol handling and retirement semantics.
use eframe::egui;
use focus_addon_sdk::{Context, Execution, Manifest, Message, read_message, write_message};
use std::{
    io::BufReader,
    path::Path,
    process::{Child, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender},
    },
    time::{Duration, Instant},
};

struct Launch {
    child: Option<Child>,
    remote: Option<super::container::Handle>,
    reader_done: Option<Receiver<()>>,
    _lease: Arc<super::packages::Lease>,
}
impl Launch {
    fn stop(mut self) {
        if let Some(mut child) = self.child.take() {
            let deadline = Instant::now() + Duration::from_secs(2);
            while Instant::now() < deadline {
                if child.try_wait().ok().flatten().is_some() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            if let Some(remote) = self.remote.take() {
                remote.stop();
            }
            if child.try_wait().ok().flatten().is_none() {
                let _ = child.kill();
            }
            let _ = child.wait();
        }
        if let Some(done) = self.reader_done.take() {
            let _ = done.recv_timeout(Duration::from_secs(1));
        }
    }
}
impl Drop for Launch {
    fn drop(&mut self) {
        if self.child.is_some() {
            let launch = Self {
                child: self.child.take(),
                remote: self.remote.take(),
                reader_done: self.reader_done.take(),
                _lease: self._lease.clone(),
            };
            std::thread::spawn(move || launch.stop());
        }
    }
}

pub(super) struct Process {
    active: Option<Launch>,
    preparing: Option<Receiver<Result<Launch, String>>>,
    cancelled: Arc<AtomicBool>,
    retiring: Arc<AtomicBool>,
    sender: SyncSender<Message>,
    pub receiver: Receiver<Result<Message, String>>,
    pub launch_error: Option<String>,
    pub started: Instant,
    handshake_started: Option<Instant>,
    pub ready: bool,
    pub last_tick: Instant,
}
impl Process {
    pub fn start(
        directory: &Path,
        manifest: &Manifest,
        context: &Context,
        ctx: &egui::Context,
    ) -> Result<Self, String> {
        let lease = Arc::new(super::packages::Lease::acquire(
            directory.parent().ok_or("Missing addon directory")?,
            &manifest.id,
            true,
        )?);
        let cancelled = Arc::new(AtomicBool::new(false));
        let retiring = Arc::new(AtomicBool::new(false));
        let (sender, requests) = mpsc::sync_channel(64);
        let (events, receiver) = mpsc::sync_channel(128);
        let directory = directory.to_path_buf();
        let manifest = manifest.clone();
        let mut context = context.clone();
        context.persistent_state = super::persistence::load(&context);
        let snapshot = super::persistence::path(&context);
        let generation = context.generation;
        let worker_cancel = cancelled.clone();
        let worker_retiring = retiring.clone();
        let worker_ctx = ctx.clone();
        let container = manifest.execution == Execution::Workspace && context.runtime.is_some();
        let launch = move || -> Result<Launch, String> {
            let (mut command, translated, remote) = if container {
                let progress = |state| {
                    let _ = events.try_send(Ok(Message::notification(
                        "panel.status",
                        serde_json::json!({"generation":generation,"state":state}),
                    )));
                    worker_ctx.request_repaint();
                };
                let (command, translated, handle) =
                    super::container::prepare(&manifest, &context, &worker_cancel, progress)?;
                (command, translated, Some(handle))
            } else {
                (
                    Command::new(directory.join(&manifest.executable)),
                    context,
                    None,
                )
            };
            command
                .current_dir(&directory)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            for (key, _) in std::env::vars_os() {
                let name = key.to_string_lossy().to_ascii_uppercase();
                if ["TOKEN", "PASSWORD", "SECRET", "API_KEY", "CREDENTIAL"]
                    .iter()
                    .any(|word| name.contains(word))
                    && !manifest
                        .environment
                        .iter()
                        .any(|allowed| std::ffi::OsStr::new(allowed) == key)
                {
                    command.env_remove(key);
                }
            }
            command.env("FOCUS_ADDON_LOG_LEVEL", log::max_level().as_str());
            if worker_cancel.load(Ordering::Relaxed) {
                return Err("Addon launch cancelled".into());
            }
            let child = command
                .spawn()
                .map_err(|e| format!("Cannot start addon: {e}"))?;
            let mut launched = Launch {
                child: Some(child),
                remote,
                reader_done: None,
                _lease: lease,
            };
            let child = launched.child.as_mut().unwrap();
            let stdout = child.stdout.take().unwrap();
            let stderr = child.stderr.take().unwrap();
            let mut stdin = child.stdin.take().unwrap();
            write_message(&mut stdin, &Message::request(0, "initialize", serde_json::json!({"protocol":focus_addon_sdk::PROTOCOL_VERSION,"language":focus_addon_sdk::language::VERSION,"context":translated}))).map_err(|e| e.to_string())?;
            let error_events = events.clone();
            std::thread::spawn(move || {
                while let Ok(message) = requests.recv() {
                    if write_message(&mut stdin, &message).is_err() {
                        let _ = error_events.try_send(Err("Addon input closed".into()));
                        break;
                    }
                    if message.method.as_deref() == Some("shutdown") {
                        break;
                    }
                }
            });
            let (done_tx, reader_done) = mpsc::channel();
            launched.reader_done = Some(reader_done);
            std::thread::spawn(move || {
                let mut reader = BufReader::new(stdout);
                loop {
                    let event = match read_message(&mut reader) {
                        Ok(Some(message)) => Ok(message),
                        Ok(None) => Err("Addon exited. Use Restart to reconnect.".into()),
                        Err(e) => Err(format!("Invalid addon protocol: {e}")),
                    };
                    let finished = event.is_err();
                    if let Ok(message) = &event {
                        if let Err(error) =
                            super::persistence::accept(&snapshot, generation, message)
                        {
                            log::warn!("Cannot save addon snapshot: {error}");
                        }
                        if message.method.as_deref() == Some("state.changed") {
                            continue;
                        }
                    }
                    // Retired processes still drain their final snapshots, even if UI output filled the queue.
                    let mut pending = event;
                    while !worker_retiring.load(Ordering::Relaxed) {
                        match events.try_send(pending) {
                            Ok(()) => break,
                            Err(mpsc::TrySendError::Full(event)) => {
                                pending = event;
                                std::thread::sleep(Duration::from_millis(10));
                            }
                            Err(mpsc::TrySendError::Disconnected(_)) => break,
                        }
                    }
                    worker_ctx.request_repaint();
                    if finished {
                        break;
                    }
                }
                let _ = done_tx.send(());
            });
            let id = manifest.id;
            std::thread::spawn(move || {
                use std::io::{BufRead, Read};
                let mut reader = BufReader::new(stderr);
                loop {
                    let mut line = Vec::new();
                    if reader
                        .by_ref()
                        .take(4096)
                        .read_until(b'\n', &mut line)
                        .unwrap_or(0)
                        == 0
                    {
                        break;
                    }
                    for line in crate::logging::redaction::sanitize(&String::from_utf8_lossy(&line))
                    {
                        let (level, text) =
                            line.split_once(' ').unwrap_or(("DEBUG", line.as_str()));
                        log::log!(
                            level.parse::<log::Level>().unwrap_or(log::Level::Debug),
                            "Addon {}: {}",
                            id,
                            text
                        );
                    }
                }
            });
            Ok(launched)
        };
        let (active, preparing, handshake_started) = if container {
            let (tx, rx) = mpsc::channel();
            let wake = ctx.clone();
            std::thread::spawn(move || {
                let _ = tx.send(launch());
                wake.request_repaint();
            });
            (None, Some(rx), None)
        } else {
            (Some(launch()?), None, Some(Instant::now()))
        };
        Ok(Self {
            active,
            preparing,
            cancelled,
            retiring,
            sender,
            receiver,
            launch_error: None,
            started: Instant::now(),
            handshake_started,
            ready: false,
            last_tick: Instant::now(),
        })
    }
    pub fn send(&self, message: Message) -> Result<(), String> {
        self.sender
            .try_send(message)
            .map_err(|_| "Addon is not accepting messages. Restart it to reconnect.".into())
    }
    pub fn expired(&mut self) -> bool {
        if let Some(launch) = &self.preparing {
            match launch.try_recv() {
                Ok(Ok(active)) => {
                    self.active = Some(active);
                    self.handshake_started = Some(Instant::now());
                    self.preparing = None;
                }
                Ok(Err(error)) => {
                    self.preparing = None;
                    self.launch_error = Some(error);
                    return false;
                }
                Err(mpsc::TryRecvError::Empty) => {
                    if self.started.elapsed() > Duration::from_secs(300) {
                        self.cancelled.store(true, Ordering::Relaxed);
                        self.launch_error =
                            Some("Addon preparation timed out. Use Restart to retry.".into());
                    }
                    return false;
                }
                Err(_) => {
                    self.preparing = None;
                    self.launch_error = Some("Addon preparation worker stopped".into());
                    return false;
                }
            }
        }
        (!self.ready
            && self
                .handshake_started
                .is_some_and(|time| time.elapsed() > Duration::from_secs(5)))
            || self
                .active
                .as_mut()
                .and_then(|launch| launch.child.as_mut())
                .is_none_or(|child| child.try_wait().ok().flatten().is_some())
    }
    pub fn is_container(&self) -> bool {
        self.preparing.is_some()
            || self
                .active
                .as_ref()
                .is_some_and(|launch| launch.remote.is_some())
    }
    fn shutdown(&mut self) -> impl FnOnce() + Send + 'static {
        self.cancelled.store(true, Ordering::Relaxed);
        self.retiring.store(true, Ordering::Relaxed);
        let _ = self.send(Message::notification("shutdown", ()));
        let active = self.active.take();
        let preparing = self.preparing.take();
        move || {
            let active =
                active.or_else(|| preparing.and_then(|rx| rx.recv().ok().and_then(Result::ok)));
            if let Some(active) = active {
                active.stop();
            }
        }
    }
    pub fn retire(mut self) -> Receiver<()> {
        let (tx, rx) = mpsc::channel();
        let stop = self.shutdown();
        std::thread::spawn(move || {
            stop();
            let _ = tx.send(());
        });
        rx
    }
    pub fn stop_now(mut self) {
        self.shutdown()();
    }
}
impl Drop for Process {
    fn drop(&mut self) {
        if self.active.is_some() || self.preparing.is_some() {
            let stop = self.shutdown();
            std::thread::spawn(stop);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    #[test]
    fn retirement_drains_full_ui_queues_and_saves_the_final_snapshot_before_unlocking() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("addons/fixture");
        std::fs::create_dir_all(&directory).unwrap();
        let program = directory.join("program");
        std::fs::write(&program, "#!/bin/sh\nIFS= read -r init\ni=0\nwhile [ $i -lt 300 ]; do printf '%s\\n' '{\"method\":\"panel.status\",\"jsonrpc\":\"2.0\",\"params\":{\"generation\":7,\"state\":\"busy\"}}'; i=$((i+1)); done\nwhile IFS= read -r line; do case \"$line\" in *shutdown*) printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"method\":\"state.changed\",\"params\":{\"generation\":7,\"state\":{\"draft\":\"final unsent draft\"}}}'; exit 0;; esac; done\n").unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        let manifest = Manifest {
            id: "fixture".into(),
            executable: "program".into(),
            ..Default::default()
        };
        let context = Context {
            generation: 7,
            storage: root.path().join("data"),
            ..Default::default()
        };
        let process =
            Process::start(&directory, &manifest, &context, &egui::Context::default()).unwrap();
        std::thread::sleep(Duration::from_millis(50));
        process
            .retire()
            .recv_timeout(Duration::from_secs(4))
            .unwrap();
        assert_eq!(
            super::super::persistence::load(&context)["draft"],
            "final unsent draft"
        );
        super::super::packages::Lease::acquire(directory.parent().unwrap(), "fixture", false)
            .unwrap();
    }
}
