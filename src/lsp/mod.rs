//! Resource-conscious document-symbol client. UI state never owns a live child process.
mod protocol;
#[cfg(test)]
mod tests;
mod transport;

use eframe::egui;
pub(crate) use protocol::Symbol;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, SyncSender},
    },
    time::{Duration, Instant},
};
use transport::Transport;

const MAX_FILE: usize = 1024 * 1024;
#[derive(Clone, Copy)]
struct Timing {
    startup: Duration,
    edits: Duration,
    idle: Duration,
    request: Duration,
    initialize: Duration,
}
impl Default for Timing {
    fn default() -> Self {
        Self {
            startup: Duration::from_millis(300),
            edits: Duration::from_millis(500),
            idle: Duration::from_secs(120),
            request: Duration::from_secs(5),
            initialize: Duration::from_secs(15),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct ServerConfig {
    pub command: String,
    pub args: Vec<String>,
}
impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            command: "rust-analyzer".into(),
            args: vec![],
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct Config {
    pub enabled: bool,
    pub rust: ServerConfig,
    pub python: ServerConfig,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            enabled: true,
            rust: ServerConfig::default(),
            python: ServerConfig {
                command: "pylsp".into(),
                args: vec![],
            },
        }
    }
}
impl Config {
    fn server(&self, language: Language) -> &ServerConfig {
        match language {
            Language::Rust => &self.rust,
            Language::Python => &self.python,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Language {
    Rust,
    Python,
}
impl Language {
    fn of(path: &Path) -> Option<Self> {
        match path.extension()?.to_str()? {
            "rs" => Some(Self::Rust),
            "py" | "pyi" => Some(Self::Python),
            _ => None,
        }
    }
    fn id(self) -> &'static str {
        match self {
            Self::Rust => "rust",
            Self::Python => "python",
        }
    }
}
#[derive(Clone)]
struct Input {
    path: PathBuf,
    root: Option<PathBuf>,
    runtime: Option<crate::dev_container::Runtime>,
    config: Config,
    language: Language,
    text: Arc<str>,
    version: u64,
    activity: u64,
    retry: u64,
}
impl Input {
    fn same_server(&self, other: &Self) -> bool {
        self.language == other.language
            && self.root == other.root
            && self.runtime == other.runtime
            && self.config == other.config
    }
    fn server_path(&self) -> Result<PathBuf, String> {
        if let Some(runtime) = &self.runtime {
            runtime.container_path(&self.path)
        } else if self.path.is_absolute() {
            Ok(self.path.clone())
        } else {
            std::env::current_dir()
                .map(|root| root.join(&self.path))
                .map_err(|e| e.to_string())
        }
    }
    fn uri(&self) -> Result<String, String> {
        reqwest::Url::from_file_path(self.server_path()?)
            .map(String::from)
            .map_err(|_| "Cannot create document URI".into())
    }
}
#[derive(Clone, Default)]
pub(crate) struct Output {
    pub symbols: Arc<Vec<Symbol>>,
    pub status: String,
    pub version: u64,
    pub ready: bool,
}
enum Event {
    Wake,
    Message(u64, Value),
    Failure(u64, String),
}
struct Worker {
    sender: SyncSender<Event>,
    latest: Arc<Mutex<Option<Input>>>,
    output: Arc<Mutex<Output>>,
    stop: Arc<AtomicBool>,
    done: Arc<AtomicBool>,
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let _ = self.sender.try_send(Event::Wake);
    }
}
#[derive(Default)]
pub(crate) struct Client {
    worker: Option<Worker>,
    retiring: Option<Arc<AtomicBool>>,
    input: Option<Input>,
    version: u64,
    activity: u64,
    retry: u64,
    cursor: usize,
    pub output: Output,
}
impl Client {
    /// Only used while the application is being destroyed; rendering never waits.
    pub fn shutdown(&mut self) {
        let done = self
            .worker
            .as_ref()
            .map(|w| w.done.clone())
            .or_else(|| self.retiring.clone());
        self.worker.take();
        if let Some(done) = done {
            let until = Instant::now() + Duration::from_secs(6);
            while !done.load(Ordering::Acquire) && Instant::now() < until {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }
    pub fn activate(&mut self) {
        self.activity += 1;
    }
    pub fn can_retry(&self) -> bool {
        self.input.is_some()
    }
    pub fn retry(&mut self) {
        self.retry += 1;
        self.activity += 1;
    }
    pub fn tick(
        &mut self,
        path: Option<&Path>,
        text: &str,
        root: Option<&Path>,
        runtime: Option<&crate::dev_container::Runtime>,
        config: &Config,
        cursor: usize,
        ctx: &egui::Context,
    ) {
        let Some((path, language)) = path
            .and_then(|p| Language::of(p).map(|l| (p, l)))
            .filter(|_| config.enabled && text.len() <= MAX_FILE)
        else {
            if let Some(worker) = self.worker.take() {
                self.retiring = Some(worker.done.clone());
                drop(worker);
            }
            self.input = None;
            self.output = Output {
                status: if !config.enabled {
                    "LSP disabled"
                } else if text.len() > MAX_FILE {
                    "Symbol navigation limited to files up to 1 MiB"
                } else {
                    "Symbol navigation supports Rust and Python"
                }
                .into(),
                ..Default::default()
            };
            return;
        };
        if self
            .retiring
            .as_ref()
            .is_some_and(|done| !done.load(Ordering::Acquire))
        {
            self.output = Output {
                status: "Stopping previous language server…".into(),
                ..Default::default()
            };
            return;
        }
        self.retiring = None;
        // Canonicalize container paths only on context changes, never each frame.
        let runtime = runtime.filter(|r| {
            if let Some(old) = self
                .input
                .as_ref()
                .filter(|old| old.path == path && old.root.as_deref() == root)
            {
                if old.runtime.as_ref() == Some(*r) {
                    return true;
                }
                if old.runtime.is_none() && !path.starts_with(&r.host_workspace) {
                    return false;
                }
            }
            r.container_path(path).is_ok()
        });
        let changed = self.input.as_ref().is_none_or(|i| {
            i.path != path
                || &*i.text != text
                || i.root.as_deref() != root
                || i.runtime.as_ref() != runtime
                || &i.config != config
        });
        if changed {
            self.version += 1;
            self.activity += 1;
        }
        if cursor != self.cursor {
            self.activity += 1;
            self.cursor = cursor;
        }
        if self.worker.is_none() {
            self.worker = Some(start_worker(ctx.clone()));
        }
        let previous = self.input.as_ref();
        let input = Input {
            path: path.into(),
            language,
            root: root.map(Path::to_path_buf),
            runtime: runtime.cloned(),
            config: config.clone(),
            text: if changed {
                Arc::from(text)
            } else {
                previous.unwrap().text.clone()
            },
            version: self.version,
            activity: self.activity,
            retry: self.retry,
        };
        let worker = self.worker.as_ref().unwrap();
        if changed
            || previous.is_none_or(|p| p.activity != input.activity || p.retry != input.retry)
        {
            *worker.latest.lock().unwrap() = Some(input.clone());
            let _ = worker.sender.try_send(Event::Wake);
        }
        self.input = Some(input);
        self.output = worker.output.lock().unwrap().clone();
        if self.output.version != self.version {
            self.output.symbols = Default::default();
            self.output.ready = false;
            self.output.status = "Updating symbols…".into();
        }
    }
}
fn publish(output: &Mutex<Output>, ctx: &egui::Context, value: Output) {
    *output.lock().unwrap() = value;
    ctx.request_repaint();
}
fn request(id: u64, method: &str, params: Value) -> Value {
    json!({"jsonrpc":"2.0", "id": id, "method": method, "params":params})
}
fn notify(method: &str, params: Value) -> Value {
    json!({"jsonrpc":"2.0", "method": method, "params": params})
}

fn settings(language: Language) -> Value {
    match language {
        Language::Rust => {
            json!({"cargo":{"buildScripts":{"enable":false}, "autoreload":false, "sysroot":null}, "checkOnSave":false, "check":{"enable":false}, "procMacro":{"enable":false}, "cachePriming":{"enable":false}, "numThreads":2, "lru":{"capacity":8}, "diagnostics":{"enable":false}, "notifications":{"cargoTomlNotFound":false}})
        }
        Language::Python => {
            let mut plugins = serde_json::Map::new();
            for name in [
                "black",
                "jedi_highlight",
                "jedi_rename",
                "folding",
                "autopep8",
                "flake8",
                "jedi_completion",
                "jedi_definition",
                "jedi_hover",
                "jedi_references",
                "jedi_signature_help",
                "jedi_type_definition",
                "mccabe",
                "preload",
                "pycodestyle",
                "pydocstyle",
                "pyflakes",
                "pylint",
                "rope_autoimport",
                "rope_completion",
                "yapf",
            ] {
                plugins.insert(name.into(), json!({"enabled":false}));
            }
            plugins.insert("jedi".into(), json!({"auto_import_modules":[]}));
            plugins.insert(
                "jedi_symbols".into(),
                json!({"enabled":true,"all_scopes":true,"include_import_symbols":false}),
            );
            json!({"pylsp":{"configurationSources":[],"plugins":plugins}})
        }
    }
}
struct Session {
    transport: Transport,
    input: Input,
    ready: bool,
    incremental: bool,
    opened: Option<(String, Arc<str>, u64)>,
    pending: Option<(u64, u64, Instant, bool)>,
    started: Instant,
}
fn start_worker(ctx: egui::Context) -> Worker {
    start_worker_with_timing(ctx, Timing::default())
}
fn start_worker_with_timing(ctx: egui::Context, timing: Timing) -> Worker {
    let (sender, receiver) = mpsc::sync_channel(4);
    let latest = Arc::new(Mutex::new(None::<Input>));
    let output = Arc::new(Mutex::new(Output::default()));
    let stop = Arc::new(AtomicBool::new(false));
    let done = Arc::new(AtomicBool::new(false));
    let worker = Worker {
        sender: sender.clone(),
        latest: latest.clone(),
        output: output.clone(),
        stop: stop.clone(),
        done: done.clone(),
    };
    std::thread::Builder::new()
        .name("document-symbols".into())
        .spawn(move || {
            run_worker(ctx, sender, receiver, latest, output, stop, done, timing);
        })
        .expect("Cannot start LSP worker");
    worker
}

#[allow(clippy::too_many_arguments)]
fn run_worker(
    ctx: egui::Context,
    sender: SyncSender<Event>,
    receiver: mpsc::Receiver<Event>,
    latest: Arc<Mutex<Option<Input>>>,
    output: Arc<Mutex<Output>>,
    stop: Arc<AtomicBool>,
    done: Arc<AtomicBool>,
    timing: Timing,
) {
    let mut session: Option<Session> = None;
    let mut wanted: Option<Input> = None;
    let mut generation = 0;
    let mut next_id = 1u64;
    let mut due = Instant::now();
    let mut last_activity = Instant::now();
    let mut idle = false;
    let mut failed = false;
    let mut completed_version = None;
    loop {
        if stop.load(Ordering::Acquire) {
            break;
        }
        let update = latest.lock().unwrap().take();
        if let Some(input) = update {
            let new_context = wanted.as_ref().is_none_or(|old| !old.same_server(&input));
            let retry = wanted.as_ref().is_some_and(|old| old.retry != input.retry);
            let new_source = wanted
                .as_ref()
                .is_none_or(|old| old.version != input.version);
            last_activity = Instant::now();
            idle = false;
            if new_context || retry {
                if let Some(mut old) = session.take() {
                    old.transport.stop(&receiver);
                }
                failed = false;
                completed_version = None;
            }
            if new_source || new_context || retry {
                due = Instant::now()
                    + if session.is_some() {
                        timing.edits
                    } else {
                        timing.startup
                    };
                if let Some(s) = &mut session {
                    if let Some((id, _, started, cancelled)) = &mut s.pending {
                        let _ = s
                            .transport
                            .send(notify("$/cancelRequest", json!({"id":id})));
                        *cancelled = true;
                        *started = Instant::now();
                    }
                }
            }
            wanted = Some(input);
        }
        let Some(input) = wanted.as_ref() else {
            if receiver.recv().is_err() {
                break;
            }
            continue;
        };
        let mut failure = None;
        if session.is_some() && last_activity.elapsed() >= timing.idle {
            if let Some(mut old) = session.take() {
                old.transport.stop(&receiver);
            }
            idle = true;
            completed_version = None;
            publish(
                &output,
                &ctx,
                Output {
                    version: input.version,
                    status: "LSP idle — open Symbols to restart".into(),
                    ..Default::default()
                },
            );
        }
        if !failed && !idle && session.is_none() && Instant::now() >= due {
            generation += 1;
            match Transport::start(input, generation, sender.clone()) {
                Ok(transport) => {
                    let mut options = settings(input.language);
                    if input.language == Language::Rust {
                        options["linkedProjects"] = json!([input
                            .server_path()
                            .map(|p| p.to_string_lossy().into_owned())
                            .unwrap_or_default()]);
                    }
                    // No workspace root: this client only needs active-file symbols.
                    let params = json!({"processId": if input.runtime.is_some() { None } else { Some(std::process::id()) }, "rootUri":null, "workspaceFolders":null, "capabilities":{"general":{"positionEncodings":["utf-16"]},"workspace":{"configuration":true},"textDocument":{"documentSymbol":{"hierarchicalDocumentSymbolSupport":true}}}, "initializationOptions":options});
                    if let Err(e) = transport.send(request(0, "initialize", params)) {
                        failure = Some(e);
                    }
                    session = Some(Session {
                        transport,
                        input: input.clone(),
                        ready: false,
                        incremental: false,
                        opened: None,
                        pending: None,
                        started: Instant::now(),
                    });
                    publish(
                        &output,
                        &ctx,
                        Output {
                            version: input.version,
                            status: "Starting language server…".into(),
                            ..Default::default()
                        },
                    );
                }
                Err(e) => failure = Some(e),
            }
        }
        if let Some(s) = &mut session {
            if !s.ready && s.started.elapsed() >= timing.initialize {
                failure =
                    Some("Language server initialization timed out; Retry to reconnect".into());
            }
            if let Some((id, version, started, cancelled)) = &mut s.pending {
                if started.elapsed()
                    >= (if *cancelled {
                        timing.request * 2
                    } else {
                        timing.request
                    })
                {
                    if *cancelled {
                        failure =
                            Some("Language server stopped responding; Retry to reconnect".into());
                    } else {
                        let _ = s
                            .transport
                            .send(notify("$/cancelRequest", json!({"id":id})));
                        *cancelled = true;
                        completed_version = Some(*version);
                        publish(
                            &output,
                            &ctx,
                            Output {
                                version: *version,
                                status: "Symbol request timed out; Retry to refresh".into(),
                                ..Default::default()
                            },
                        );
                    }
                }
            }
            if s.ready
                && s.pending.is_none()
                && completed_version != Some(input.version)
                && Instant::now() >= due
            {
                match synchronize(s, input, next_id) {
                    Ok(()) => {
                        s.pending = Some((next_id, input.version, Instant::now(), false));
                        next_id += 1;
                    }
                    Err(e) => failure = Some(e),
                }
            }
        }
        if let Some(error) = failure {
            if let Some(mut old) = session.take() {
                old.transport.stop(&receiver);
            }
            failed = true;
            publish(
                &output,
                &ctx,
                Output {
                    version: input.version,
                    status: error,
                    ..Default::default()
                },
            );
        }
        let now = Instant::now();
        let mut wait = if session.is_some() {
            timing.idle.saturating_sub(last_activity.elapsed())
        } else {
            Duration::from_secs(3600)
        };
        if !failed
            && !idle
            && (session.is_none()
                || session.as_ref().is_some_and(|s| {
                    s.ready && s.pending.is_none() && completed_version != Some(input.version)
                }))
        {
            wait = wait.min(due.saturating_duration_since(now));
        }
        if let Some(s) = &session {
            if !s.ready {
                wait = wait.min(timing.initialize.saturating_sub(s.started.elapsed()));
            }
            if let Some((_, _, started, cancelled)) = s.pending {
                wait = wait.min(
                    (if cancelled {
                        timing.request * 2
                    } else {
                        timing.request
                    })
                    .saturating_sub(started.elapsed()),
                );
            }
        }
        match receiver.recv_timeout(wait.max(Duration::from_millis(1))) {
            Ok(Event::Message(g, value)) if g == generation && session.is_some() => {
                let Some(s) = &mut session else {
                    continue;
                };
                let result = handle_message(s, input, &value);
                match result {
                    Err(e) => {
                        if let Some(mut old) = session.take() {
                            old.transport.stop(&receiver);
                        }
                        failed = true;
                        publish(
                            &output,
                            &ctx,
                            Output {
                                version: input.version,
                                status: e,
                                ..Default::default()
                            },
                        );
                    }
                    Ok(Some((version, result))) if version == input.version => {
                        completed_version = Some(version);
                        match result {
                            Ok(symbols) => publish(
                                &output,
                                &ctx,
                                Output {
                                    version,
                                    ready: true,
                                    status: if symbols.is_empty() {
                                        "No symbols found".into()
                                    } else {
                                        String::new()
                                    },
                                    symbols: Arc::new(symbols),
                                },
                            ),
                            Err(e) => publish(
                                &output,
                                &ctx,
                                Output {
                                    version,
                                    status: e,
                                    ..Default::default()
                                },
                            ),
                        }
                    }
                    _ => {}
                }
            }
            Ok(Event::Failure(g, error)) if g == generation && session.is_some() => {
                if let Some(mut old) = session.take() {
                    old.transport.stop(&receiver);
                }
                failed = true;
                publish(
                    &output,
                    &ctx,
                    Output {
                        version: input.version,
                        status: error,
                        ..Default::default()
                    },
                );
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            _ => {}
        }
    }
    if let Some(mut old) = session {
        old.transport.stop(&receiver);
    }
    done.store(true, Ordering::Release);
    ctx.request_repaint();
}

fn synchronize(s: &mut Session, input: &Input, id: u64) -> Result<(), String> {
    let uri = input.uri()?;
    if s.opened.as_ref().is_some_and(|(old, _, _)| old != &uri) {
        s.transport.send(notify(
            "textDocument/didClose",
            json!({"textDocument":{"uri":s.opened.as_ref().unwrap().0}}),
        ))?;
        s.opened = None;
    }
    if let Some((_, old, version)) = &s.opened {
        if *version != input.version {
            let change = if s.incremental {
                protocol::change(old, &input.text)
            } else {
                json!({"text":&*input.text})
            };
            s.transport.send(notify("textDocument/didChange",json!({"textDocument":{"uri":uri,"version":input.version},"contentChanges":[change]})))?;
        }
    } else {
        s.transport.send(notify("textDocument/didOpen",json!({"textDocument":{"uri":uri,"languageId":input.language.id(),"version":input.version,"text":&*input.text}})))?;
    }
    s.opened = Some((uri.clone(), input.text.clone(), input.version));
    s.transport.send(request(
        id,
        "textDocument/documentSymbol",
        json!({"textDocument":{"uri":uri}}),
    ))
}

type SymbolReply = Option<(u64, Result<Vec<Symbol>, String>)>;
fn handle_message(s: &mut Session, input: &Input, value: &Value) -> Result<SymbolReply, String> {
    if let Some(method) = value["method"].as_str() {
        let result = match method {
            "workspace/configuration" => {
                let config = settings(s.input.language);
                let items = value["params"]["items"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default();
                Value::Array(
                    items
                        .iter()
                        .map(|item| {
                            let mut found = &config;
                            if let Some(section) = item["section"].as_str() {
                                let section =
                                    section.strip_prefix("rust-analyzer.").unwrap_or(section);
                                if section == "rust-analyzer" {
                                    return config.clone();
                                }
                                for part in section.split('.') {
                                    found = &found[part];
                                }
                            }
                            found.clone()
                        })
                        .collect(),
                )
            }
            "client/registerCapability"
            | "client/unregisterCapability"
            | "window/workDoneProgress/create" => Value::Null,
            _ => {
                return s.transport.send(json!({"jsonrpc":"2.0","id":value["id"],"error":{"code":-32601,"message":"Unsupported client method"}})).map(|_| None);
            }
        };
        s.transport
            .send(json!({"jsonrpc":"2.0","id":value["id"],"result":result}))?;
        return Ok(None);
    }
    if value["id"] == 0 {
        if value.get("error").is_some() {
            return Err("Language server initialization failed".into());
        }
        let caps = &value["result"]["capabilities"];
        if caps["positionEncoding"]
            .as_str()
            .is_some_and(|e| e != "utf-16")
        {
            return Err("Language server requires unsupported position encoding".into());
        }
        let symbols = caps
            .get("documentSymbolProvider")
            .is_some_and(|v| v == true || v.is_object());
        if !symbols {
            return Err("Language server does not provide document symbols".into());
        }
        let sync = &caps["textDocumentSync"];
        let kind = sync
            .as_u64()
            .or_else(|| sync["change"].as_u64())
            .unwrap_or(0);
        let sync = kind > 0;
        s.incremental = kind == 2;
        if !sync {
            return Err("Language server does not support document synchronization".into());
        }

        s.transport.send(notify("initialized", json!({})))?;
        s.transport.send(notify("workspace/didChangeConfiguration",json!({"settings":match input.language { Language::Rust => json!({"rust-analyzer":settings(input.language)}), Language::Python => settings(input.language) }})))?;
        s.ready = true;
    } else if let Some((id, version, _, cancelled)) = s.pending {
        if value["id"].as_u64() == Some(id) {
            s.pending = None;
            if cancelled || version != input.version {
                return Ok(None);
            }
            if value.get("error").is_some() {
                return Ok(Some((
                    version,
                    Err("Language server could not load symbols; Retry to refresh".into()),
                )));
            }
            let uri = input.uri()?;
            return Ok(Some((
                version,
                protocol::symbols(&value["result"], &input.text, &uri),
            )));
        }
    }
    Ok(None)
}
