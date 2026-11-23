//! Owned stdio process. All blocking work, including shutdown, runs off the UI thread.
use super::{Event, Input, protocol};
use serde_json::{Value, json};
use std::{
    io::{BufReader, Read},
    process::{Child, Command, Stdio},
    sync::mpsc::{self, Receiver, SyncSender},
    time::{Duration, Instant},
};

pub(super) const REMOTE_START: &str = r#"marker=$1; shift; command -v setsid >/dev/null 2>&1 || { echo 'LSP requires setsid in the Dev Container' >&2; exit 127; }; exec 3<&0; setsid "$@" <&3 3<&- & pid=$!; exec 3<&-; echo "$pid" > "$marker"; wait "$pid"; result=$?; rm -f "$marker"; exit "$result""#;
pub(super) const REMOTE_KILL: &str =
    r#"if test -f "$1"; then read -r pid < "$1"; kill -KILL "-$pid" 2>/dev/null; rm -f "$1"; fi"#;

pub(super) struct Transport {
    child: Child,
    sender: Option<SyncSender<Value>>,
    remote: Option<(crate::dev_container::Runtime, String)>,
    generation: u64,
    stopped: bool,
}
impl Transport {
    pub fn start(
        input: &Input,
        generation: u64,
        events: SyncSender<Event>,
    ) -> Result<Self, String> {
        let config = input.config.server(input.language);
        let mut remote = None;
        let mut command = if let Some(runtime) = &input.runtime {
            let marker = format!("/tmp/focus-lsp-{}-{generation}.pid", std::process::id());
            let mut command = runtime.command("sh");
            // Keep the child's stdin attached, and retain a remote PID for forced cleanup.
            command
                .args(["-c", REMOTE_START, "focus-lsp"])
                .arg(&marker)
                .arg(&config.command);
            remote = Some((runtime.clone(), marker));
            command
        } else {
            let mut command = Command::new(&config.command);
            if let Some(root) = &input.root {
                command.current_dir(root);
            }
            command
        };
        command
            .args(&config.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        let mut child = command.spawn().map_err(|e| {
            format!(
                "Cannot start {}: {e}. Install the server or configure lsp; then Retry.",
                config.command
            )
        })?;
        let stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();
        let mut stdin = child.stdin.take().unwrap();
        let (sender, requests) = mpsc::sync_channel(4);
        let errors = events.clone();
        std::thread::spawn(move || {
            while let Ok(value) = requests.recv() {
                if protocol::write_message(&mut stdin, &value).is_err() {
                    let _ = errors.try_send(Event::Failure(
                        generation,
                        "Language server input closed".into(),
                    ));
                    break;
                }
            }
        });
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                match protocol::read_message(&mut reader) {
                    Ok(Some(value)) => {
                        // Ignore unsolicited diagnostics/progress/logs without retaining them.
                        if value.get("id").is_none() {
                            continue;
                        }
                        if events.send(Event::Message(generation, value)).is_err() {
                            break;
                        }
                    }
                    result => {
                        let detail = match result {
                            Err(e) => format!("Language server protocol: {e}"),
                            _ => "Language server exited; Retry to reconnect".into(),
                        };
                        let _ = events.send(Event::Failure(generation, detail));
                        break;
                    }
                }
            }
        });
        std::thread::spawn(move || {
            // Drain without collecting or logging document text or server diagnostics.
            let mut reader = stderr;
            let mut chunk = [0u8; 4096];
            while reader.read(&mut chunk).is_ok_and(|n| n > 0) {}
        });
        Ok(Self {
            child,
            sender: Some(sender),
            remote,
            generation,
            stopped: false,
        })
    }
    pub fn send(&self, value: Value) -> Result<(), String> {
        self.sender
            .as_ref()
            .ok_or("Language server stopped")?
            .try_send(value)
            .map_err(|_| "Language server input queue full or closed".into())
    }
    pub fn stop(&mut self, events: &Receiver<Event>) {
        let _ = self.send(json!({"jsonrpc":"2.0", "id":-1, "method":"shutdown", "params":null}));
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            match events.recv_timeout(Duration::from_millis(25)) {
                Ok(Event::Message(g, value)) if g == self.generation && value["id"] == -1 => {
                    let _ = self.send(json!({"jsonrpc":"2.0", "method":"exit", "params":null}));
                    break;
                }
                Ok(Event::Failure(g, _)) if g == self.generation => break,
                _ => {}
            }
        }
        while Instant::now() < deadline {
            if self.child.try_wait().ok().flatten().is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        self.kill();
    }
    fn kill(&mut self) {
        if self.stopped {
            return;
        }
        self.stopped = true;
        if let Some((runtime, marker)) = self.remote.take() {
            let mut command = runtime.command("sh");
            command
                .args(["-c", REMOTE_KILL, "focus-lsp-cleanup"])
                .arg(marker)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            if let Ok(mut child) = command.spawn() {
                let deadline = Instant::now() + Duration::from_secs(2);
                while Instant::now() < deadline && child.try_wait().ok().flatten().is_none() {
                    std::thread::sleep(Duration::from_millis(20));
                }
                let _ = child.kill();
                let _ = child.wait();
            }
        }
        #[cfg(unix)]
        unsafe {
            libc::kill(-(self.child.id() as i32), libc::SIGKILL);
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
        self.sender.take();
    }
}
impl Drop for Transport {
    fn drop(&mut self) {
        self.kill();
    }
}
