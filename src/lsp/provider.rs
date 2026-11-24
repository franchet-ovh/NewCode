use super::{Output, protocol};
use focus_addon_sdk::{
    Context, Message,
    language::{Packet, Start},
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, VecDeque},
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

pub(crate) const MAX_FILE: usize = 1024 * 1024;
const MAX_RESULTS: usize = 20_000;

#[derive(Clone)]
pub(crate) struct Diagnostic {
    pub path: PathBuf,
    pub range: protocol::Range,
    pub severity: u64,
    pub source: String,
    pub message: String,
    pub offsets: Option<(usize, usize)>,
}
#[derive(Clone)]
pub(crate) struct Completion {
    pub label: String,
    pub edits: Value,
    pub cursor: usize,
}
pub(crate) struct Edit {
    pub path: PathBuf,
    pub before: Arc<str>,
    pub after: String,
    pub cursor: usize,
}
struct Document {
    path: PathBuf,
    text: Arc<str>,
    version: u64,
    sent: u64,
    sent_text: Arc<str>,
    symbols: u64,
    dirty: bool,
    save: bool,
    changed: Instant,
}
enum Kind {
    Initialize,
    Symbols,
    Completion(usize),
    Format,
}
struct Pending {
    uri: String,
    version: u64,
    kind: Kind,
    started: Instant,
}
pub(crate) struct Provider {
    pub addon: String,
    pub incarnation: u64,
    pub stopped: bool,
    pub generation: u64,
    pub session: u64,
    pub language: String,
    pub context: Context,
    pub output: Output,
    pub diagnostics: BTreeMap<PathBuf, Vec<Diagnostic>>,
    pub completions: Vec<Completion>,
    pub selected: usize,
    pub reveal_completion: bool,
    pub changes: VecDeque<Edit>,
    pub outgoing: Vec<Message>,
    documents: BTreeMap<String, Document>,
    active: String,
    cursor: usize,
    next_id: u64,
    pending: BTreeMap<u64, Pending>,
    settings: Value,
    capabilities: Value,
    ready: bool,
    failed: bool,
    started: Instant,
    activity: Instant,
    completion_due: Option<Instant>,
}
impl Provider {
    pub fn new(addon: String, incarnation: u64, start: Start) -> Self {
        Self {
            addon,
            incarnation,
            stopped: false,
            generation: start.generation,
            session: start.session,
            language: start.language.clone(),
            context: start.context.clone(),
            output: Output {
                status: "Preparing language server…".into(),
                ..Default::default()
            },
            diagnostics: BTreeMap::new(),
            completions: vec![],
            selected: 0,
            reveal_completion: true,
            changes: VecDeque::new(),
            outgoing: vec![Message::notification("lsp.start", start)],
            documents: BTreeMap::new(),
            active: String::new(),
            cursor: 0,
            next_id: 1,
            pending: BTreeMap::new(),
            settings: Value::Null,
            capabilities: Value::Null,
            ready: false,
            failed: false,
            started: Instant::now(),
            activity: Instant::now(),
            completion_due: None,
        }
    }
    pub fn idle(&self) -> bool {
        self.activity.elapsed() >= Duration::from_secs(120)
    }
    pub fn next_wake(&self) -> Duration {
        let mut delay = Duration::from_secs(120).saturating_sub(self.activity.elapsed());
        if self.failed {
            return delay;
        }
        if !self.ready {
            delay = delay.min(Duration::from_secs(330).saturating_sub(self.started.elapsed()));
        }
        for pending in self.pending.values() {
            delay = delay.min(Duration::from_secs(15).saturating_sub(pending.started.elapsed()));
        }
        if self.ready {
            for doc in self
                .documents
                .values()
                .filter(|d| d.sent != d.version || d.save)
            {
                delay = delay.min(if doc.save || doc.sent == 0 {
                    Duration::ZERO
                } else {
                    Duration::from_millis(500).saturating_sub(doc.changed.elapsed())
                });
            }
            if let Some(due) = self.completion_due {
                delay = delay.min(due.saturating_duration_since(Instant::now()));
            }
        }
        delay.max(Duration::from_millis(1))
    }
    pub fn activate(&mut self) {
        self.activity = Instant::now();
    }
    pub fn saved(&mut self, path: &Path) {
        self.activate();
        let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        if let Some(uri) = self.uri(&path) {
            if let Some(doc) = self.documents.get_mut(&uri) {
                doc.save = true;
            }
        }
    }
    pub fn active_diagnostics(&self) -> &[Diagnostic] {
        self.documents
            .get(&self.active)
            .and_then(|doc| self.diagnostics.get(&doc.path))
            .map_or(&[], Vec::as_slice)
    }
    pub fn stop_message(&self) -> Message {
        Message::notification(
            "lsp.stop",
            json!({"generation":self.generation,"session":self.session}),
        )
    }
    fn wire(&mut self, message: Value) {
        self.outgoing.push(Message::notification(
            "lsp.message",
            Packet {
                generation: self.generation,
                session: self.session,
                message,
            },
        ));
    }
    fn notify(&mut self, method: &str, params: Value) {
        self.wire(json!({"jsonrpc":"2.0","method":method,"params":params}));
    }
    fn request(&mut self, method: &str, params: Value, kind: Kind, uri: String) {
        if self.pending.len() >= 64 {
            self.output.status = "Too many pending language requests; wait or Retry".into();
            return;
        }
        let id = self.next_id;
        self.next_id += 1;
        let version = self.documents.get(&uri).map_or(0, |d| d.version);
        self.pending.insert(
            id,
            Pending {
                uri,
                version,
                kind,
                started: Instant::now(),
            },
        );
        self.wire(json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}));
    }
    fn uri(&self, path: &Path) -> Option<String> {
        let path = if let Some(runtime) = &self.context.runtime {
            runtime
                .workspace_folder
                .join(path.strip_prefix(&runtime.host_workspace).ok()?)
        } else {
            path.to_path_buf()
        };
        reqwest::Url::from_file_path(path).ok().map(String::from)
    }
    fn path(&self, uri: &str) -> Option<PathBuf> {
        let path = reqwest::Url::parse(uri).ok()?.to_file_path().ok()?;
        if let Some(runtime) = &self.context.runtime {
            let relative = path.strip_prefix(&runtime.workspace_folder).ok()?;
            if relative
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir))
            {
                return None;
            }
            Some(runtime.host_workspace.join(relative))
        } else {
            Some(path)
        }
    }
    pub fn update(&mut self, active: &Path, cursor: usize, documents: Vec<(PathBuf, &str, bool)>) {
        let Some(active) = self.uri(active) else {
            return;
        };
        let selection_changed = self.active != active || self.cursor != cursor;
        if selection_changed {
            self.cancel_completion();
            self.completions.clear();
            self.completion_due = None;
            self.activity = Instant::now();
        }
        if self.active != active {
            self.output.symbols = Default::default();
            self.output.ready = false;
            if let Some(doc) = self.documents.get_mut(&active) {
                doc.symbols = 0;
            }
        }
        self.active = active;
        self.cursor = cursor;
        let mut active_edited = false;
        let mut wanted = Vec::new();
        for (path, text, dirty) in documents {
            if text.len() > MAX_FILE {
                continue;
            }
            let Some(uri) = self.uri(&path) else {
                continue;
            };
            wanted.push(uri.clone());
            if let Some(doc) = self.documents.get_mut(&uri) {
                if &*doc.text != text {
                    doc.text = Arc::from(text);
                    doc.version += 1;
                    doc.changed = Instant::now();
                    self.diagnostics.remove(&path);
                    self.activity = Instant::now();
                    if uri == self.active {
                        active_edited = true;
                        self.completion_due = doc
                            .text
                            .chars()
                            .nth(cursor.saturating_sub(1))
                            .filter(|c| c.is_alphanumeric() || matches!(c, '_' | '.' | ':'))
                            .map(|_| Instant::now() + Duration::from_millis(300));
                        self.output.symbols = Default::default();
                        self.output.ready = false;
                    }
                }
                if doc.dirty && !dirty {
                    doc.save = true;
                    self.activity = Instant::now();
                }
                doc.dirty = dirty;
            } else {
                if let Some(diagnostics) = self.diagnostics.get_mut(&path) {
                    let positions = protocol::Positions::new(text);
                    for diagnostic in diagnostics {
                        diagnostic.offsets = positions
                            .offset(diagnostic.range.start)
                            .zip(positions.offset(diagnostic.range.end));
                    }
                }
                self.documents.insert(
                    uri,
                    Document {
                        path,
                        text: Arc::from(text),
                        sent_text: Arc::from(""),
                        version: 1,
                        sent: 0,
                        symbols: 0,
                        dirty,
                        save: false,
                        changed: Instant::now(),
                    },
                );
                self.activity = Instant::now();
            }
        }
        if active_edited {
            self.cancel_completion();
            self.completions.clear();
        }
        let closed: Vec<_> = self
            .documents
            .keys()
            .filter(|u| !wanted.contains(u))
            .cloned()
            .collect();
        for uri in closed {
            let ids: Vec<_> = self
                .pending
                .iter()
                .filter(|(_, p)| p.uri == uri)
                .map(|(id, _)| *id)
                .collect();
            for id in ids {
                self.pending.remove(&id);
                self.notify("$/cancelRequest", json!({"id":id}));
            }
            if let Some(doc) = self.documents.remove(&uri) {
                self.diagnostics.remove(&doc.path);
                if self.ready && doc.sent > 0 {
                    self.notify("textDocument/didClose", json!({"textDocument":{"uri":uri}}));
                }
            }
        }
        self.output.version = self.documents.get(&self.active).map_or(0, |d| d.version);
    }
    fn synchronize(&mut self, uri: &str) {
        let Some(doc) = self.documents.get_mut(uri) else {
            return;
        };
        if !self.ready {
            return;
        }
        let previous = doc.sent;
        let version = doc.version;
        let text = doc.text.clone();
        let save = doc.save;
        let old_text = doc.sent_text.clone();
        doc.sent_text = text.clone();
        doc.sent = version;
        doc.save = false;
        if previous == 0 {
            self.notify("textDocument/didOpen", json!({"textDocument":{"uri":uri,"languageId":self.language,"version":version,"text":&*text}}));
        } else if previous != version {
            let sync = &self.capabilities["textDocumentSync"];
            let incremental = sync.as_u64().or_else(|| sync["change"].as_u64()) == Some(2);
            let change = if incremental {
                protocol::change(&old_text, &text)
            } else {
                json!({"text":&*text})
            };
            self.notify(
                "textDocument/didChange",
                json!({"textDocument":{"uri":uri,"version":version},"contentChanges":[change]}),
            );
        }
        if save {
            self.notify(
                "textDocument/didSave",
                json!({"textDocument":{"uri":uri},"text":&*text}),
            );
        }
    }
    pub fn tick(&mut self) {
        if self.failed {
            return;
        }
        if !self.ready && self.started.elapsed() > Duration::from_secs(330) {
            self.fail("Language server startup timed out; Retry");
            return;
        }
        let expired: Vec<_> = self
            .pending
            .iter()
            .filter(|(_, p)| p.started.elapsed() > Duration::from_secs(15))
            .map(|(id, _)| *id)
            .collect();
        for id in expired {
            if self
                .pending
                .remove(&id)
                .is_some_and(|p| matches!(p.kind, Kind::Initialize))
            {
                self.fail("Language server initialization timed out; Retry");
                return;
            }
            self.output.status = "Language request timed out; Retry".into();
            self.notify("$/cancelRequest", json!({"id":id}));
        }
        if !self.ready {
            return;
        }
        let due: Vec<_> = self
            .documents
            .iter()
            .filter(|(_, d)| {
                d.sent == 0
                    || d.save
                    || d.sent != d.version && d.changed.elapsed() >= Duration::from_millis(500)
            })
            .map(|(u, _)| u.clone())
            .collect();
        for uri in due {
            self.synchronize(&uri);
        }
        if let Some(doc) = self.documents.get_mut(&self.active) {
            if doc.sent == doc.version
                && doc.symbols != doc.version
                && enabled(&self.capabilities["documentSymbolProvider"])
            {
                doc.symbols = doc.version;
                let uri = self.active.clone();
                self.request(
                    "textDocument/documentSymbol",
                    json!({"textDocument":{"uri":uri}}),
                    Kind::Symbols,
                    uri,
                );
            }
        }
        if self.completion_due.is_some_and(|due| Instant::now() >= due) {
            self.completion_due = None;
            self.complete();
        }
    }
    fn cancel_completion(&mut self) {
        let ids: Vec<_> = self
            .pending
            .iter()
            .filter(|(_, p)| matches!(p.kind, Kind::Completion(_)))
            .map(|(id, _)| *id)
            .collect();
        for id in ids {
            self.pending.remove(&id);
            self.notify("$/cancelRequest", json!({"id":id}));
        }
    }
    pub fn dismiss_completion(&mut self) {
        self.cancel_completion();
        self.completions.clear();
        self.completion_due = None;
    }
    pub fn complete(&mut self) {
        self.completion_due = None;
        if !self.ready || !enabled(&self.capabilities["completionProvider"]) {
            return;
        }
        self.activate();
        self.cancel_completion();
        let uri = self.active.clone();
        self.synchronize(&uri);
        if let Some(doc) = self.documents.get(&uri) {
            let position = protocol::position(&doc.text, self.cursor);
            self.request(
                "textDocument/completion",
                json!({"textDocument":{"uri":uri},"position":position,"context":{"triggerKind":1}}),
                Kind::Completion(self.cursor),
                uri,
            );
        }
    }
    pub fn format(&mut self, tab_size: usize, insert_spaces: bool) {
        if !self.ready || !enabled(&self.capabilities["documentFormattingProvider"]) {
            self.output.status = "This language server does not provide formatting".into();
            return;
        }
        self.activate();
        let uri = self.active.clone();
        self.synchronize(&uri);
        self.request(
            "textDocument/formatting",
            json!({"textDocument":{"uri":uri},"options":{"tabSize":tab_size,"insertSpaces":insert_spaces}}),
            Kind::Format,
            uri,
        );
    }
    pub fn accept_completion(&mut self, index: usize) {
        let Some(completion) = self.completions.get(index) else {
            return;
        };
        let Some(doc) = self.documents.get(&self.active) else {
            return;
        };
        if let Ok(after) = apply_edits(&doc.text, &completion.edits) {
            self.changes.push_back(Edit {
                path: doc.path.clone(),
                before: doc.text.clone(),
                after,
                cursor: completion.cursor,
            });
        }
        self.dismiss_completion();
    }
    pub fn fail(&mut self, error: &str) {
        if !self.failed {
            self.outgoing.push(self.stop_message());
        }
        self.failed = true;
        self.ready = false;
        self.output = Output {
            status: error.into(),
            ..Default::default()
        };
        self.diagnostics.clear();
        self.dismiss_completion();
        self.pending.clear();
    }
    pub fn event(&mut self, message: Message) {
        let p = message.params.unwrap_or_default();
        if p["generation"] != self.generation || p["session"] != self.session {
            return;
        }
        if self.failed {
            return;
        }
        match message.method.as_deref() {
            Some("lsp.status") => {
                self.started = Instant::now();
                let status = p["status"].as_str().unwrap_or_default();
                if p["failed"] == true {
                    self.fail(status);
                } else if !self.ready {
                    self.output.status = status.into();
                }
            }
            Some("lsp.started") => {
                self.settings = p["settings"].clone();
                let root = self
                    .context
                    .runtime
                    .as_ref()
                    .map(|r| r.workspace_folder.clone())
                    .or(self.context.workspace.clone());
                let root_uri = root
                    .and_then(|p| reqwest::Url::from_directory_path(p).ok())
                    .map(String::from);
                let params = json!({"processId":null,"rootUri":root_uri,"workspaceFolders":root_uri.as_ref().map(|uri|vec![json!({"uri":uri,"name":"workspace"})]),"capabilities":{"general":{"positionEncodings":["utf-16"]},"workspace":{"configuration":true},"textDocument":{"synchronization":{"didSave":true},"documentSymbol":{"hierarchicalDocumentSymbolSupport":true},"publishDiagnostics":{"versionSupport":true},"completion":{"completionItem":{"snippetSupport":false,"insertReplaceSupport":true}}}},"initializationOptions":p.get("initialization_options").unwrap_or(&self.settings)});
                self.request("initialize", params, Kind::Initialize, String::new());
            }
            Some("lsp.message") => self.receive(p["message"].clone()),
            _ => {}
        }
    }
    fn receive(&mut self, message: Value) {
        if let Some(method) = message["method"].as_str() {
            if method == "textDocument/publishDiagnostics" {
                self.diagnose(&message["params"]);
                return;
            }
            if message.get("id").is_none() {
                return;
            }
            let result = match method {
                "workspace/configuration" => Value::Array(
                    message["params"]["items"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .map(|item| {
                            let section = item["section"].as_str().unwrap_or("");
                            if section.is_empty() {
                                return self.settings.clone();
                            }
                            section
                                .split('.')
                                .fold(&self.settings, |v, k| &v[k])
                                .clone()
                        })
                        .collect(),
                ),
                "window/workDoneProgress/create" => Value::Null,
                _ => {
                    self.wire(json!({"jsonrpc":"2.0","id":message["id"],"error":{"code":-32601,"message":"Unsupported client method"}}));
                    return;
                }
            };
            self.wire(json!({"jsonrpc":"2.0","id":message["id"],"result":result}));
            return;
        }
        let Some(id) = message["id"].as_u64() else {
            return;
        };
        let Some(pending) = self.pending.remove(&id) else {
            return;
        };
        if let Some(error) = message.get("error") {
            self.output.status = error["message"]
                .as_str()
                .unwrap_or("Language request failed")
                .into();
            if matches!(pending.kind, Kind::Initialize) {
                self.fail("Language server initialization failed; Retry");
            }
            return;
        }
        let result = &message["result"];
        if matches!(pending.kind, Kind::Initialize) {
            self.capabilities = result["capabilities"].clone();
            if self.capabilities["positionEncoding"]
                .as_str()
                .is_some_and(|s| s != "utf-16")
            {
                self.fail("Unsupported server position encoding");
                return;
            }
            let sync = &self.capabilities["textDocumentSync"];
            if !matches!(
                sync.as_u64().or_else(|| sync["change"].as_u64()),
                Some(1 | 2)
            ) {
                self.fail("Language server does not support document synchronization");
                return;
            }
            self.ready = true;
            self.output.status.clear();
            self.notify("initialized", json!({}));
            let settings = self.settings.clone();
            self.notify(
                "workspace/didChangeConfiguration",
                json!({"settings":settings}),
            );
            return;
        }
        let Some(doc) = self.documents.get(&pending.uri) else {
            return;
        };
        if doc.version != pending.version {
            return;
        }
        match pending.kind {
            Kind::Symbols if pending.uri == self.active => {
                match protocol::symbols(result, &doc.text, &pending.uri) {
                    Ok(symbols) => {
                        self.output.symbols = Arc::new(symbols);
                        self.output.ready = true;
                    }
                    Err(error) => self.output.status = error,
                }
            }
            Kind::Completion(cursor) if pending.uri == self.active && cursor == self.cursor => {
                self.completions = completion_items(result, &doc.text, cursor);
                self.selected = 0;
                self.reveal_completion = true;
            }
            Kind::Format => match apply_edits(&doc.text, result) {
                Ok(after) => self.changes.push_back(Edit {
                    path: doc.path.clone(),
                    before: doc.text.clone(),
                    after,
                    cursor: self.cursor,
                }),
                Err(error) => self.output.status = error,
            },
            _ => {}
        }
    }
    fn diagnose(&mut self, p: &Value) {
        let Some(uri) = p["uri"].as_str() else {
            return;
        };
        let Some(path) = self.path(uri) else {
            return;
        };
        let doc = self.documents.get(uri);
        if let Some(doc) = doc {
            if doc.sent != doc.version
                || p["version"].as_u64().is_some_and(|v| v != doc.version)
                || doc.dirty && p["version"].is_null()
            {
                return;
            }
        }
        let other_count: usize = self
            .diagnostics
            .iter()
            .filter(|(p, _)| **p != path)
            .map(|(_, d)| d.len())
            .sum();
        let positions = doc.map(|d| protocol::Positions::new(&d.text));
        let diagnostics: Vec<Diagnostic> = p["diagnostics"]
            .as_array()
            .into_iter()
            .flatten()
            .take(MAX_RESULTS.saturating_sub(other_count))
            .filter_map(|v| {
                let range: protocol::Range = serde_json::from_value(v["range"].clone()).ok()?;
                let offsets = positions
                    .as_ref()
                    .and_then(|p| Some((p.offset(range.start)?, p.offset(range.end)?)));
                Some(Diagnostic {
                    path: path.clone(),
                    range,
                    severity: v["severity"].as_u64().unwrap_or(1),
                    source: v["source"].as_str().unwrap_or("").into(),
                    message: v["message"].as_str()?.chars().take(8192).collect(),
                    offsets,
                })
            })
            .collect();
        if diagnostics.is_empty() {
            self.diagnostics.remove(&path);
        } else {
            self.diagnostics.insert(path, diagnostics);
        }
    }
}
fn enabled(value: &Value) -> bool {
    value == true || value.is_object()
}

pub(crate) fn apply_edits(text: &str, edits: &Value) -> Result<String, String> {
    if edits.is_null() {
        return Ok(text.into());
    }
    let edits = edits.as_array().ok_or("Invalid formatting edits")?;
    if edits.len() > MAX_RESULTS {
        return Err("Too many text edits".into());
    }
    let positions = protocol::Positions::new(text);
    let mut bytes: Vec<_> = text.char_indices().map(|(i, _)| i).collect();
    bytes.push(text.len());
    let mut ranges = Vec::new();
    for edit in edits {
        let range: protocol::Range =
            serde_json::from_value(edit["range"].clone()).map_err(|_| "Invalid text edit range")?;
        let start = positions
            .offset(range.start)
            .ok_or("Invalid text edit position")?;
        let end = positions
            .offset(range.end)
            .ok_or("Invalid text edit position")?;
        if start > end {
            return Err("Reversed text edit range".into());
        }
        ranges.push((
            bytes[start],
            bytes[end],
            edit["newText"].as_str().ok_or("Missing edit text")?,
        ));
    }
    ranges.sort_by_key(|(s, e, _)| (*s, *e));
    if ranges
        .windows(2)
        .any(|w| w[0].1 > w[1].0 || w[0].0 == w[1].0)
    {
        return Err("Overlapping text edits".into());
    }
    let size = text.len() as isize
        + ranges
            .iter()
            .map(|(s, e, r)| r.len() as isize - (*e - *s) as isize)
            .sum::<isize>();
    if size < 0 || size as usize > MAX_FILE {
        return Err("Edited document exceeds 1 MiB".into());
    }
    let mut result = String::with_capacity(size as usize);
    let mut offset = 0;
    for (start, end, replacement) in ranges {
        result.push_str(&text[offset..start]);
        result.push_str(replacement);
        offset = end;
    }
    result.push_str(&text[offset..]);
    Ok(result)
}

fn completion_items(value: &Value, text: &str, cursor: usize) -> Vec<Completion> {
    let items = value.as_array().or_else(|| value["items"].as_array());
    let positions = protocol::Positions::new(text);
    let start = cursor.saturating_sub(
        text.chars()
            .take(cursor)
            .collect::<Vec<_>>()
            .iter()
            .rev()
            .take_while(|c| c.is_alphanumeric() || **c == '_')
            .count(),
    );
    let prefix = text
        .chars()
        .skip(start)
        .take(cursor - start)
        .collect::<String>()
        .to_lowercase();
    items.into_iter().flatten().filter(|item|item["filterText"].as_str().or_else(||item["label"].as_str()).is_some_and(|label|label.to_lowercase().starts_with(&prefix))).take(200).filter_map(|item| {
        if item["insertTextFormat"] == 2 { return None; }
        let label = item["label"].as_str()?.to_string();
        let mut edit = item["textEdit"].clone();
        if edit.is_null() { edit = json!({"range":{"start":protocol::position(text,start),"end":protocol::position(text,cursor)},"newText":item["insertText"].as_str().unwrap_or(&label)}); }
        if edit["range"].is_null() { edit["range"] = edit["replace"].clone(); }
        let range:protocol::Range = serde_json::from_value(edit["range"].clone()).ok()?;
        let target = positions.offset(range.start)? + edit["newText"].as_str()?.chars().count();
        let mut edits = vec![edit];
        if let Some(extra) = item["additionalTextEdits"].as_array() { edits.extend(extra.iter().cloned()); }
        // Additional imports before the primary insertion shift the final caret.
        let mut delta = 0isize;
        for extra in edits.iter().skip(1) {
            let r:protocol::Range = serde_json::from_value(extra["range"].clone()).ok()?;
            let a=positions.offset(r.start)?; let b=positions.offset(r.end)?;
            if a < positions.offset(range.start)? { delta += extra["newText"].as_str()?.chars().count() as isize - (b-a) as isize; }
        }
        let edits = Value::Array(edits);
        Some(Completion { label, edits, cursor:target.saturating_add_signed(delta) })
    }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unicode_edits_are_atomic_and_reject_overlap() {
        let edits = json!([{"range":{"start":{"line":0,"character":2},"end":{"line":0,"character":3}},"newText":"ok"}]);
        assert_eq!(apply_edits("🦀x", &edits).unwrap(), "🦀ok");
        assert!(apply_edits("🦀x", &json!([edits[0], edits[0]])).is_err());
        assert!(apply_edits("🦀x",&json!([{"range":{"start":{"line":0,"character":1},"end":{"line":0,"character":2}},"newText":"bad"}])).is_err());
    }
    #[test]
    fn completion_applies_additional_imports_and_positions_caret() {
        let items = json!([{"label":"print","textEdit":{"range":{"start":{"line":1,"character":0},"end":{"line":1,"character":2}},"newText":"print"},"additionalTextEdits":[{"range":{"start":{"line":0,"character":0},"end":{"line":0,"character":0}},"newText":"import os\n"}]}]);
        let result = completion_items(&items, "# hi\npr", 7);
        assert_eq!(result.len(), 1);
        assert_eq!(
            apply_edits("# hi\npr", &result[0].edits).unwrap(),
            "import os\n# hi\nprint"
        );
        assert_eq!(result[0].cursor, 20);
    }
    fn provider() -> Provider {
        let mut p = Provider::new(
            "fixture".into(),
            4,
            Start {
                generation: 7,
                session: 9,
                language: "python".into(),
                context: Context {
                    generation: 7,
                    workspace: Some("/workspace".into()),
                    ..Default::default()
                },
                command: None,
                args: vec![],
            },
        );
        p.update(
            Path::new("/workspace/a.py"),
            2,
            vec![
                ("/workspace/a.py".into(), "pr".into(), false),
                ("/workspace/b.py".into(), "unsaved = 1".into(), true),
            ],
        );
        p.event(Message::notification(
            "lsp.started",
            json!({"generation":7,"session":9,"settings":{}}),
        ));
        p.receive(json!({"jsonrpc":"2.0","id":1,"result":{"capabilities":{"positionEncoding":"utf-16","textDocumentSync":{"change":2,"save":true},"completionProvider":{},"documentFormattingProvider":true,"documentSymbolProvider":true}}}));
        p.tick();
        p
    }
    fn wire(p: &mut Provider) -> Vec<Value> {
        std::mem::take(&mut p.outgoing)
            .into_iter()
            .filter(|m| m.method.as_deref() == Some("lsp.message"))
            .map(|m| m.params.unwrap()["message"].clone())
            .collect()
    }
    fn response(p: &mut Provider, method: &str, result: Value) {
        let messages = wire(p);
        let id = messages
            .iter()
            .rev()
            .find(|m| m["method"] == method)
            .unwrap()["id"]
            .clone();
        p.receive(json!({"jsonrpc":"2.0","id":id,"result":result}));
    }
    #[test]
    fn open_buffers_sync_incrementally_and_saves_do_not_poll() {
        let mut p = provider();
        let initial = wire(&mut p);
        assert_eq!(
            initial
                .iter()
                .filter(|m| m["method"] == "textDocument/didOpen")
                .count(),
            2
        );
        assert!(
            initial
                .iter()
                .any(|m| m["params"]["textDocument"]["text"] == "unsaved = 1")
        );
        p.update(
            Path::new("/workspace/a.py"),
            3,
            vec![
                ("/workspace/a.py".into(), "pri".into(), true),
                ("/workspace/b.py".into(), "unsaved = 1".into(), true),
            ],
        );
        p.complete();
        let changed = wire(&mut p);
        let change = changed
            .iter()
            .find(|m| m["method"] == "textDocument/didChange")
            .unwrap();
        assert_eq!(change["params"]["contentChanges"][0]["text"], "i");
        assert!(change["params"]["contentChanges"][0]["range"].is_object());
        p.saved(Path::new("/workspace/a.py"));
        p.tick();
        assert!(
            wire(&mut p)
                .iter()
                .any(|m| m["method"] == "textDocument/didSave")
        );
        p.tick();
        assert!(wire(&mut p).is_empty());
    }
    #[test]
    fn late_completion_cannot_edit_a_new_snapshot_or_closed_document() {
        let mut p = provider();
        wire(&mut p);
        p.complete();
        let id = wire(&mut p)
            .iter()
            .find(|m| m["method"] == "textDocument/completion")
            .unwrap()["id"]
            .clone();
        p.update(
            Path::new("/workspace/a.py"),
            2,
            vec![("/workspace/a.py".into(), "xx".into(), true)],
        );
        p.receive(json!({"jsonrpc":"2.0","id":id,"result":[{"label":"print"}]}));
        assert!(p.completions.is_empty());
        p.complete();
        p.update(
            Path::new("/workspace/b.py"),
            0,
            vec![("/workspace/b.py".into(), "".into(), false)],
        );
        assert!(
            p.pending
                .values()
                .all(|q| q.uri != "file:///workspace/a.py")
        );
    }
    #[test]
    fn diagnostics_replace_clear_and_reject_obsolete_generations() {
        let mut p = provider();
        let diagnostic = json!({"uri":"file:///workspace/a.py","version":1,"diagnostics":[{"range":{"start":{"line":0,"character":0},"end":{"line":0,"character":2}},"severity":1,"source":"mypy","message":"wrong type"}]});
        p.diagnose(&diagnostic);
        assert_eq!(
            p.diagnostics[Path::new("/workspace/a.py")][0].offsets,
            Some((0, 2))
        );
        p.event(Message::notification("lsp.message",json!({"generation":6,"session":9,"message":{"jsonrpc":"2.0","method":"textDocument/publishDiagnostics","params":{"uri":"file:///workspace/a.py","diagnostics":[]}}})));
        assert_eq!(p.diagnostics[Path::new("/workspace/a.py")].len(), 1);
        p.update(
            Path::new("/workspace/a.py"),
            3,
            vec![("/workspace/a.py".into(), "pri".into(), true)],
        );
        assert!(!p.diagnostics.contains_key(Path::new("/workspace/a.py")));
        p.diagnose(&diagnostic);
        assert!(!p.diagnostics.contains_key(Path::new("/workspace/a.py")));
        p.complete();
        let mut current = diagnostic.clone();
        current["version"] = json!(2);
        p.diagnose(&current);
        assert_eq!(p.diagnostics[Path::new("/workspace/a.py")].len(), 1);
        p.diagnose(&json!({"uri":"file:///workspace/a.py","version":2,"diagnostics":[]}));
        assert!(!p.diagnostics.contains_key(Path::new("/workspace/a.py")));
    }
    #[test]
    fn formatting_is_versioned_and_never_writes_to_disk() {
        let mut p = provider();
        wire(&mut p);
        p.format(4, true);
        response(
            &mut p,
            "textDocument/formatting",
            json!([{"range":{"start":{"line":0,"character":0},"end":{"line":0,"character":2}},"newText":"print()"}]),
        );
        let edit = p.changes.pop_front().unwrap();
        assert_eq!(&*edit.before, "pr");
        assert_eq!(edit.after, "print()");
        p.format(4, true);
        let id = wire(&mut p)
            .iter()
            .find(|m| m["method"] == "textDocument/formatting")
            .unwrap()["id"]
            .clone();
        p.update(
            Path::new("/workspace/a.py"),
            3,
            vec![("/workspace/a.py".into(), "new".into(), true)],
        );
        p.receive(json!({"jsonrpc":"2.0","id":id,"result":[]}));
        assert!(p.changes.is_empty());
    }
    #[test]
    fn formatting_uses_effective_indentation_options() {
        let mut p = provider();
        wire(&mut p);
        p.format(6, false);
        let messages = wire(&mut p);
        let request = messages
            .iter()
            .find(|m| m["method"] == "textDocument/formatting")
            .unwrap();
        assert_eq!(
            request["params"]["options"],
            json!({"tabSize":6,"insertSpaces":false})
        );
    }
    #[test]
    fn idle_activity_and_symbol_switching_follow_user_events() {
        let mut p = provider();
        p.activity = Instant::now() - Duration::from_secs(121);
        p.update(
            Path::new("/workspace/a.py"),
            2,
            vec![
                ("/workspace/a.py".into(), "pr".into(), false),
                ("/workspace/b.py".into(), "unsaved = 1".into(), true),
            ],
        );
        assert!(p.idle());
        p.activate();
        assert!(!p.idle());
        p.update(
            Path::new("/workspace/b.py"),
            0,
            vec![
                ("/workspace/a.py".into(), "pr".into(), false),
                ("/workspace/b.py".into(), "unsaved = 1".into(), true),
            ],
        );
        p.tick();
        wire(&mut p);
        p.update(
            Path::new("/workspace/a.py"),
            2,
            vec![
                ("/workspace/a.py".into(), "pr".into(), false),
                ("/workspace/b.py".into(), "unsaved = 1".into(), true),
            ],
        );
        p.tick();
        assert!(
            wire(&mut p)
                .iter()
                .any(|m| m["method"] == "textDocument/documentSymbol"
                    && m["params"]["textDocument"]["uri"] == "file:///workspace/a.py")
        );
    }
    #[test]
    fn container_uris_never_leak_host_paths_or_escape_the_mount() {
        let mut p = provider();
        p.context.runtime = Some(focus_addon_sdk::Runtime {
            host_workspace: "/workspace".into(),
            container_id: "test".into(),
            workspace_folder: "/code".into(),
            remote_user: None,
        });
        assert_eq!(
            p.uri(Path::new("/workspace/a.py")).unwrap(),
            "file:///code/a.py"
        );
        assert_eq!(
            p.path("file:///code/a.py").unwrap(),
            Path::new("/workspace/a.py")
        );
        assert!(p.path("file:///code/../etc/passwd").is_none());
        assert!(p.uri(Path::new("/outside/a.py")).is_none());
    }
    #[test]
    fn idle_deadlines_do_not_request_continuous_repaints_and_failures_stop_once() {
        let mut p = provider();
        response(&mut p, "textDocument/documentSymbol", json!([]));
        assert!(p.next_wake() > Duration::from_secs(10));
        p.update(
            Path::new("/workspace/a.py"),
            3,
            vec![("/workspace/a.py".into(), "pri".into(), true)],
        );
        assert!(p.next_wake() <= Duration::from_millis(500));
        p.fail("server failed");
        p.fail("server still failed");
        assert_eq!(
            p.outgoing
                .iter()
                .filter(|m| m.method.as_deref() == Some("lsp.stop"))
                .count(),
            1
        );
        assert!(p.diagnostics.is_empty());
    }
    #[test]
    fn provider_owns_namespaces_and_initialization_options_for_any_language() {
        let mut p = provider();
        wire(&mut p);
        p.language = "another-language".into();
        p.event(Message::notification("lsp.started",json!({"generation":7,"session":9,"settings":{"another":{"answer":42}},"initialization_options":{"startup":"custom"}})));
        let messages = wire(&mut p);
        let initialize = messages
            .iter()
            .find(|m| m["method"] == "initialize")
            .unwrap();
        assert_eq!(
            initialize["params"]["initializationOptions"],
            json!({"startup":"custom"})
        );
        p.receive(json!({"jsonrpc":"2.0","id":77,"method":"workspace/configuration","params":{"items":[{"section":"another.answer"},{"section":"another"},{"section":"missing"}]}}));
        let messages = wire(&mut p);
        assert_eq!(messages[0]["result"], json!([42,{"answer":42},null]));
    }
}
