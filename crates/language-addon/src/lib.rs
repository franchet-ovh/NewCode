//! Shared executable runner. Language policy lives here, never in the editor.
use focus_addon_sdk::{
    Context, Message, View, Widget,
    language::{self, Packet, Start},
};
use serde_json::{Value, json};
use std::{
    io::{self, BufReader},
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

type Output = Arc<Mutex<io::Stdout>>;
fn send(out: &Output, message: Message) {
    let _ = focus_addon_sdk::write_message(&mut *out.lock().unwrap(), &message);
}
fn status(out: &Output, start: &Start, text: &str, failed: bool) {
    send(
        out,
        Message::notification(
            "lsp.status",
            json!({"generation":start.generation,"session":start.session,"status":text,"failed":failed}),
        ),
    );
    send(
        out,
        Message::notification(
            "view.update",
            View {
                generation: start.generation,
                title: format!("{} language support", start.language),
                sidebar: vec![
                    Widget::text(text),
                    Widget::Button {
                        style: Default::default(),
                        id: "retry".into(),
                        label: "Retry".into(),
                        enabled: true,
                    },
                ],
                ..Default::default()
            },
        ),
    );
}

struct Session {
    stop: Arc<AtomicBool>,
    sender: mpsc::SyncSender<Value>,
    worker: Option<thread::JoinHandle<()>>,
}
impl Drop for Session {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

pub fn run(language: &'static str, version: &str) {
    if std::env::args().any(|a| a == "--version") {
        println!("focus-addon-lsp-{language} {version}");
        return;
    }
    let out = Arc::new(Mutex::new(io::stdout()));
    let mut input = BufReader::new(io::stdin());
    let mut session: Option<Session> = None;
    let mut last: Option<Start> = None;
    let mut generation = 0;
    while let Ok(Some(message)) = focus_addon_sdk::read_message(&mut input) {
        match message.method.as_deref() {
            Some("initialize") => {
                let params = message.params.unwrap_or_default();
                generation = params["context"]["generation"].as_u64().unwrap_or(0);
                send(
                    &out,
                    Message::response(
                        message.id.unwrap_or(0),
                        json!({"protocol":1,"language":language::VERSION}),
                    ),
                );
                send(
                    &out,
                    Message::notification(
                        "view.update",
                        View {
                            generation,
                            title: format!("{language} language support"),
                            sidebar: vec![Widget::muted(
                                "Open a matching file to start language support. Missing server dependencies are installed automatically.",
                            )],
                            ..Default::default()
                        },
                    ),
                );
            }
            Some("lsp.start") => {
                if let Ok(start) =
                    serde_json::from_value::<Start>(message.params.unwrap_or_default())
                {
                    if start.generation != generation || start.language != language {
                        continue;
                    }
                    session.take();
                    session = Some(start_session(start.clone(), out.clone()));
                    last = Some(start);
                }
            }
            Some("lsp.message") => {
                if let Ok(packet) =
                    serde_json::from_value::<Packet>(message.params.unwrap_or_default())
                {
                    if last.as_ref().is_some_and(|s| {
                        s.generation == packet.generation && s.session == packet.session
                    }) {
                        if let Some(session) = &session {
                            if session.sender.try_send(packet.message).is_err() {
                                status(
                                    &out,
                                    last.as_ref().unwrap(),
                                    "Language server queue closed or full; Retry",
                                    true,
                                );
                            }
                        }
                    }
                }
            }
            Some("lsp.stop") => {
                let p = message.params.unwrap_or_default();
                if last
                    .as_ref()
                    .is_some_and(|s| p["generation"] == s.generation && p["session"] == s.session)
                {
                    session.take();
                    last = None;
                    send(&out, Message::notification("lsp.stopped", p));
                }
            }
            Some("context.changed") => {
                session.take();
                last = None;
                generation = message.params.unwrap_or_default()["generation"]
                    .as_u64()
                    .unwrap_or(0);
            }
            Some("action") => {
                let p = message.params.unwrap_or_default();
                if p["generation"] == generation && p["id"] == "retry" {
                    // The host allocates a new incarnation-safe session and resends snapshots.
                    send(
                        &out,
                        Message::notification("lsp.retry", json!({"generation":generation})),
                    );
                }
            }
            Some("shutdown") => break,
            _ => {}
        }
    }
    drop(session);
}

fn start_session(start: Start, out: Output) -> Session {
    let stop = Arc::new(AtomicBool::new(false));
    let cancelled = stop.clone();
    let (sender, receiver) = mpsc::sync_channel(64);
    let shutdown = sender.clone();
    let worker = thread::spawn(move || {
        let result = serve(&start, &out, &cancelled, receiver, shutdown);
        if let Err(error) = result {
            if !cancelled.load(Ordering::Acquire) {
                status(&out, &start, &error, true);
            }
        }
    });
    Session {
        stop,
        sender,
        worker: Some(worker),
    }
}

fn command(context: &Context, program: &str) -> Command {
    if let Some(runtime) = &context.runtime {
        let mut command = Command::new("docker");
        command
            .args(["exec", "-i", "-w"])
            .arg(&runtime.workspace_folder);
        if let Some(user) = &runtime.remote_user {
            command.args(["-u", user]);
        }
        command.arg(&runtime.container_id).arg(program);
        command
    } else {
        let mut command = Command::new(program);
        if let Some(root) = &context.workspace {
            command.current_dir(root);
        }
        command
    }
}

fn grouped(command: &mut Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
}
fn kill(child: &mut Child) {
    #[cfg(unix)]
    unsafe {
        libc::kill(-(child.id() as i32), libc::SIGKILL);
    }
    let _ = child.kill();
    let _ = child.wait();
}

// All remote tool processes have a remote process group too: killing docker exec
// alone would leave pip/rustup (or their children) running in the container.
const REMOTE_START: &str = "marker=$1; shift; exec 3<&0; setsid \"$@\" <&3 3<&- & pid=$!; exec 3<&-; echo \"$pid\" > \"$marker\"; wait \"$pid\"; result=$?; rm -f \"$marker\"; exit \"$result\"";
const REMOTE_KILL: &str = "if test -f \"$1\"; then read -r pid < \"$1\"; kill -KILL \"-$pid\" 2>/dev/null; rm -f \"$1\"; fi";
static TOOL_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn remote_cleanup(context: &Context, marker: &str) {
    let mut cmd = command(context, "sh");
    cmd.args(["-c", REMOTE_KILL, "focus-cleanup", marker])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    grouped(&mut cmd);
    if let Ok(mut child) = cmd.spawn() {
        let until = Instant::now() + Duration::from_millis(700);
        while Instant::now() < until {
            if child.try_wait().ok().flatten().is_some() {
                return;
            }
            thread::sleep(Duration::from_millis(10));
        }
        kill(&mut child);
    }
}

fn checked(mut cmd: Command, args: &[&str], stop: &AtomicBool) -> Result<(), String> {
    let mut remote = None;
    if cmd.get_program() == "docker" {
        let mut prefix: Vec<_> = cmd.get_args().map(|a| a.to_os_string()).collect();
        let program = prefix.pop().ok_or("Missing remote program")?;
        let marker = format!(
            "/tmp/focus-language-tool-{}-{}.pid",
            std::process::id(),
            TOOL_ID.fetch_add(1, Ordering::Relaxed)
        );
        let mut wrapped = Command::new("docker");
        wrapped
            .args(&prefix)
            .args(["sh", "-c", REMOTE_START, "focus-tool"])
            .arg(&marker)
            .arg(program);
        remote = Some((prefix, marker));
        cmd = wrapped;
    }
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit());
    grouped(&mut cmd);
    if stop.load(Ordering::Acquire) {
        return Err("Cancelled".into());
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("Cannot run required tool: {e}"))?;
    let started = Instant::now();
    let timeout = if args.contains(&"--version") || args.first() == Some(&"-c") {
        15
    } else {
        300
    };
    loop {
        if stop.load(Ordering::Acquire) || started.elapsed() > Duration::from_secs(timeout) {
            if let Some((prefix, marker)) = &remote {
                let mut cleanup = Command::new("docker");
                cleanup
                    .args(prefix)
                    .args(["sh", "-c", REMOTE_KILL, "focus-cleanup", marker])
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null());
                grouped(&mut cleanup);
                if let Ok(mut cleanup) = cleanup.spawn() {
                    let until = Instant::now() + Duration::from_millis(700);
                    while Instant::now() < until && cleanup.try_wait().ok().flatten().is_none() {
                        thread::sleep(Duration::from_millis(10));
                    }
                    kill(&mut cleanup);
                }
            }
            kill(&mut child);
            return Err("Installation cancelled or timed out".into());
        }
        if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
            return if status.success() {
                Ok(())
            } else {
                Err("Tool failed; see addon logs and Retry".into())
            };
        }
        thread::sleep(Duration::from_millis(25));
    }
}

fn capture(
    context: &Context,
    program: &str,
    args: &[&str],
    stop: &AtomicBool,
) -> Result<String, String> {
    use std::io::Read;
    let mut cmd = command(context, program);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    grouped(&mut cmd);
    let mut child = cmd.spawn().map_err(|e| e.to_string())?;
    let started = Instant::now();
    loop {
        if stop.load(Ordering::Acquire) || started.elapsed() > Duration::from_secs(5) {
            kill(&mut child);
            return Err("Runtime query timed out or was cancelled".into());
        }
        if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
            if !status.success() {
                return Err("Runtime query failed; see addon logs".into());
            }
            let mut text = String::new();
            child
                .stdout
                .take()
                .unwrap()
                .take(8193)
                .read_to_string(&mut text)
                .map_err(|e| e.to_string())?;
            if text.len() > 8192 {
                return Err("Runtime query too large".into());
            }
            return Ok(text);
        }
        thread::sleep(Duration::from_millis(25));
    }
}
fn remote_home(context: &Context, stop: &AtomicBool) -> Result<PathBuf, String> {
    let home = PathBuf::from(capture(
        context,
        "python3",
        &[
            "-c",
            "import os,pwd; print(pwd.getpwuid(os.getuid()).pw_dir,end='')",
        ],
        stop,
    )?);
    if !home.is_absolute() {
        return Err("Container home must be absolute".into());
    }
    Ok(home)
}

struct InstallLock(std::fs::File);
impl InstallLock {
    fn acquire(path: &std::path::Path, stop: &AtomicBool) -> Result<Self, String> {
        std::fs::create_dir_all(path.parent().ok_or("Missing cache directory")?)
            .map_err(|e| e.to_string())?;
        let file = std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(path)
            .map_err(|e| e.to_string())?;
        use std::os::fd::AsRawFd;
        let started = Instant::now();
        loop {
            if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
                return Ok(Self(file));
            }
            if stop.load(Ordering::Acquire) || started.elapsed() > Duration::from_secs(300) {
                return Err(
                    "Waiting for another server installation timed out or was cancelled".into(),
                );
            }
            thread::sleep(Duration::from_millis(25));
        }
    }
}
impl Drop for InstallLock {
    fn drop(&mut self) {
        use std::os::fd::AsRawFd;
        unsafe {
            libc::flock(self.0.as_raw_fd(), libc::LOCK_UN);
        }
    }
}
const PYTHON_IMPORTS: &str = "import pylsp, pylsp_mypy, autopep8, pyflakes, pycodestyle";
const PYTHON_PACKAGES: &[&str] = &[
    "python-lsp-server==1.15.0",
    "pylsp-mypy==0.7.0",
    "autopep8==2.3.2",
    "pyflakes==3.4.0",
    "pycodestyle==2.14.0",
];
fn interpreter(start: &Start, stop: &AtomicBool) -> Result<String, String> {
    let configured = start.context.state["interpreter"]
        .as_str()
        .filter(|s| !s.is_empty());
    let root = start
        .context
        .runtime
        .as_ref()
        .map(|r| &r.workspace_folder)
        .or(start.context.workspace.as_ref());
    let mut program = configured.unwrap_or("python3").to_string();
    if configured.is_none() {
        if let Some(root) = root {
            let python = root.join(".venv/bin/python").to_string_lossy().into_owned();
            if checked(command(&start.context, &python), &["--version"], stop).is_ok() {
                program = python;
            }
        }
    }
    let path = capture(
        &start.context,
        &program,
        &["-c", "import sys; print(sys.executable,end='')"],
        stop,
    )?;
    if !std::path::Path::new(&path).is_absolute() {
        return Err("Python interpreter must resolve to an absolute path".into());
    }
    Ok(path)
}

fn settings(language: &str) -> Value {
    if language == "rust" {
        json!({"checkOnSave":true,"check":{"command":"clippy","allTargets":false,"extraArgs":["-j","2"]},"numThreads":2,"cachePriming":{"enable":false},"cargo":{"buildScripts":{"enable":true}},"procMacro":{"enable":true},"lru":{"capacity":32}})
    } else {
        json!({"pylsp":{"plugins":{"jedi_completion":{"enabled":true},"jedi_symbols":{"enabled":true,"all_scopes":true},"pyflakes":{"enabled":true},"pycodestyle":{"enabled":true},"autopep8":{"enabled":true},"yapf":{"enabled":false},"mccabe":{"enabled":false},"pylsp_mypy":{"enabled":true,"live_mode":false,"dmypy":false}}}})
    }
}

fn provision(
    start: &Start,
    out: &Output,
    stop: &AtomicBool,
) -> Result<(String, Vec<String>), String> {
    if let Some(program) = start.command.as_ref().filter(|c| !c.is_empty()) {
        return Ok((program.clone(), start.args.clone()));
    }
    status(out, start, "Checking language server dependencies…", false);
    if start.language == "rust" {
        if checked(
            command(&start.context, "rust-analyzer"),
            &["--version"],
            stop,
        )
        .is_err()
            || checked(command(&start.context, "rustfmt"), &["--version"], stop).is_err()
            || checked(
                command(&start.context, "cargo"),
                &["clippy", "--version"],
                stop,
            )
            .is_err()
        {
            status(
                out,
                start,
                "Installing rust-analyzer, rustfmt, and Clippy using rustup…",
                false,
            );
            checked(
                command(&start.context, "rustup"),
                &[
                    "component",
                    "add",
                    "rust-analyzer",
                    "rustfmt",
                    "clippy",
                    "rust-src",
                ],
                stop,
            )
            .map_err(|e| format!("Rust requires an installed rustup toolchain. {e}"))?;
        }
        Ok(("rust-analyzer".into(), vec![]))
    } else {
        let interpreter = interpreter(start, stop)?;
        if checked(
            command(&start.context, &interpreter),
            &["-c", PYTHON_IMPORTS],
            stop,
        )
        .is_ok()
        {
            return Ok((interpreter, vec!["-m".into(), "pylsp".into()]));
        }
        let storage = if start.context.runtime.is_some() {
            remote_home(&start.context, stop)?.join(".cache/focus-editor/lsp-python-v1")
        } else {
            start.context.storage.join("python-v1")
        };
        let python = storage.join("bin/python").to_string_lossy().into_owned();
        let lock = storage.with_extension("lock");
        let _local_lock = if start.context.runtime.is_none() {
            Some(InstallLock::acquire(&lock, stop)?)
        } else {
            None
        };
        if checked(
            command(&start.context, &python),
            &["-c", PYTHON_IMPORTS],
            stop,
        )
        .is_err()
        {
            status(
                out,
                start,
                "Installing Python language server and checking/formatting plugins…",
                false,
            );
            if start.context.runtime.is_some() {
                checked(
                    command(&start.context, "mkdir"),
                    &["-p", &storage.parent().unwrap().to_string_lossy()],
                    stop,
                )?;
                let mut args = vec![
                    "-w",
                    "300",
                    lock.to_str().ok_or("Invalid lock path")?,
                    "sh",
                    "-c",
                    "if \"$3\" -c 'import pylsp, pylsp_mypy, autopep8, pyflakes, pycodestyle' 2>/dev/null; then exit 0; fi; \"$1\" -m venv \"$2\" || \"$1\" -m venv --without-pip \"$2\" || exit; if \"$3\" -m pip --version >/dev/null 2>&1; then shift 2; exec \"$@\"; else bootstrap=$1; target=$3; shift 5; exec \"$bootstrap\" -m pip --python \"$target\" \"$@\"; fi",
                    "focus-python-install",
                    &interpreter,
                    storage.to_str().ok_or("Invalid cache path")?,
                    &python,
                    "-m",
                    "pip",
                    "install",
                ];
                args.extend_from_slice(PYTHON_PACKAGES);
                checked(command(&start.context, "flock"), &args, stop)?;
            } else {
                let venv = storage.to_str().ok_or("Invalid cache path")?;
                let seeded = checked(
                    command(&start.context, &interpreter),
                    &["-m", "venv", venv],
                    stop,
                )
                .is_ok();
                if !seeded {
                    checked(
                        command(&start.context, &interpreter),
                        &["-m", "venv", "--without-pip", venv],
                        stop,
                    )
                    .map_err(|e| format!("Python requires venv support. {e}"))?;
                }
                let mut args = if seeded {
                    vec!["-m", "pip", "install"]
                } else {
                    vec!["-m", "pip", "--python", &python, "install"]
                };
                args.extend_from_slice(PYTHON_PACKAGES);
                checked(
                    command(&start.context, if seeded { &python } else { &interpreter }),
                    &args,
                    stop,
                )
                .map_err(|e| {
                    format!("Python needs ensurepip or an existing pip supporting --python. {e}")
                })?;
            }
        }
        Ok((python, vec!["-m".into(), "pylsp".into()]))
    }
}

struct OwnedChild {
    child: Child,
    remote: Option<(Context, String)>,
}
impl Drop for OwnedChild {
    fn drop(&mut self) {
        if let Some((context, marker)) = &self.remote {
            remote_cleanup(context, marker);
        }
        kill(&mut self.child);
    }
}

fn serve(
    start: &Start,
    out: &Output,
    stop: &AtomicBool,
    receiver: mpsc::Receiver<Value>,
    shutdown: mpsc::SyncSender<Value>,
) -> Result<(), String> {
    let (program, args) = provision(start, out, stop)?;
    if stop.load(Ordering::Acquire) {
        return Ok(());
    }
    let mut config = settings(&start.language);
    if start.language == "python" {
        let python = interpreter(start, stop)?;
        config["pylsp"]["plugins"]["jedi"]["environment"] = json!(python);
        config["pylsp"]["plugins"]["pylsp_mypy"]["overrides"] =
            json!(["--python-executable", python, true]);
    }
    let mut remote = None;
    let mut cmd = if start.context.runtime.is_some() {
        let marker = format!(
            "/tmp/focus-language-{}-{}.pid",
            std::process::id(),
            start.session
        );
        let mut cmd = command(&start.context, "sh");
        cmd.args(["-c", REMOTE_START, "focus-language"])
            .arg(&marker)
            .arg(&program);
        remote = Some((start.context.clone(), marker));
        cmd
    } else {
        command(&start.context, &program)
    };
    cmd.args(&args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    grouped(&mut cmd);
    let mut owned = OwnedChild {
        child: cmd
            .spawn()
            .map_err(|e| format!("Cannot start server: {e}"))?,
        remote,
    };
    let mut stdin = owned.child.stdin.take().unwrap();
    let stdout = owned.child.stdout.take().unwrap();
    let out_reader = out.clone();
    let start_reader = start.clone();
    let finished = Arc::new(AtomicBool::new(false));
    let end = finished.clone();
    let reader = thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        loop {
            match language::read(&mut reader) {
                Ok(Some(message)) => {
                    let initialized = message["result"]["capabilities"].is_object();
                    send(
                        &out_reader,
                        Message::notification(
                            "lsp.message",
                            Packet {
                                generation: start_reader.generation,
                                session: start_reader.session,
                                message,
                            },
                        ),
                    );
                    if initialized {
                        status(&out_reader, &start_reader, "Language server ready", false);
                    }
                }
                result => {
                    if let Err(error) = result {
                        status(
                            &out_reader,
                            &start_reader,
                            &format!("Invalid language server message: {error}"),
                            true,
                        );
                    }
                    break;
                }
            }
        }
        end.store(true, Ordering::Release);
    });
    let writer_done = Arc::new(AtomicBool::new(false));
    let writer_finished = writer_done.clone();
    let writer_stop = Arc::new(AtomicBool::new(false));
    let writer_cancel = writer_stop.clone();
    let writer = thread::spawn(move || {
        while !writer_cancel.load(Ordering::Acquire) {
            match receiver.recv_timeout(Duration::from_millis(25)) {
                Ok(message) => {
                    if language::write(&mut stdin, &message).is_err() {
                        break;
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
                _ => {}
            }
        }
        writer_finished.store(true, Ordering::Release);
    });
    let initialization_options = config.clone();
    let config = if start.language == "rust" {
        json!({"rust-analyzer":config})
    } else {
        config
    };
    send(
        out,
        Message::notification(
            "lsp.started",
            json!({"generation":start.generation,"session":start.session,"settings":config,"initialization_options":initialization_options}),
        ),
    );
    status(out, start, "Starting language server…", false);
    while !stop.load(Ordering::Acquire)
        && !finished.load(Ordering::Acquire)
        && !writer_done.load(Ordering::Acquire)
    {
        if owned.child.try_wait().map_err(|e| e.to_string())?.is_some() {
            break;
        }
        thread::sleep(Duration::from_millis(25));
    }
    let _ =
        shutdown.try_send(json!({"jsonrpc":"2.0","id":u64::MAX,"method":"shutdown","params":null}));
    thread::sleep(Duration::from_millis(100));
    let _ = shutdown.try_send(json!({"jsonrpc":"2.0","method":"exit"}));
    thread::sleep(Duration::from_millis(50));
    drop(owned);
    writer_stop.store(true, Ordering::Release);
    drop(shutdown);
    let _ = reader.join();
    let _ = writer.join();
    if !stop.load(Ordering::Acquire) {
        return Err("Language server exited; Retry to reconnect".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn language_policy_enables_features_without_live_type_checks() {
        assert_eq!(settings("rust")["check"]["command"], "clippy");
        assert_eq!(settings("rust")["cachePriming"]["enable"], false);
        assert_eq!(
            settings("python")["pylsp"]["plugins"]["pylsp_mypy"]["live_mode"],
            false
        );
    }
    #[test]
    fn command_override_does_not_install_dependencies() {
        let start = Start {
            generation: 0,
            session: 1,
            language: "rust".into(),
            context: Context::default(),
            command: Some("/custom/server".into()),
            args: vec![],
        };
        assert_eq!(
            provision(
                &start,
                &Arc::new(Mutex::new(io::stdout())),
                &AtomicBool::new(false)
            )
            .unwrap(),
            ("/custom/server".into(), vec![])
        );
    }
    #[test]
    fn cancellation_reaps_a_tool_process_group() {
        let stop = Arc::new(AtomicBool::new(false));
        let cancel = stop.clone();
        let started = Instant::now();
        let worker =
            thread::spawn(move || checked(Command::new("sh"), &["-c", "sleep 60"], &cancel));
        thread::sleep(Duration::from_millis(60));
        stop.store(true, Ordering::Release);
        assert!(worker.join().unwrap().is_err());
        assert!(started.elapsed() < Duration::from_secs(2));
    }
    #[test]
    fn provisioning_locks_are_exclusive_and_release_after_failure() {
        let path = std::env::temp_dir().join(format!(
            "focus-language-lock-{}-{}",
            std::process::id(),
            TOOL_ID.fetch_add(1, Ordering::Relaxed)
        ));
        let first = InstallLock::acquire(&path, &AtomicBool::new(false)).unwrap();
        assert!(InstallLock::acquire(&path, &AtomicBool::new(true)).is_err());
        drop(first);
        assert!(InstallLock::acquire(&path, &AtomicBool::new(false)).is_ok());
        let _ = std::fs::remove_file(path);
    }
    #[test]
    fn python_analysis_uses_an_absolute_runtime_interpreter() {
        let start = Start {
            generation: 0,
            session: 1,
            language: "python".into(),
            context: Context::default(),
            command: None,
            args: vec![],
        };
        let path = interpreter(&start, &AtomicBool::new(false)).unwrap();
        assert!(std::path::Path::new(&path).is_absolute());
    }
}
