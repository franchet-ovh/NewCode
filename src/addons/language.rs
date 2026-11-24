use super::*;
use crate::lsp::{self, Provider};
use focus_addon_sdk::language::Start;
use std::time::Instant;

pub(super) struct Retiring {
    addon: String,
    incarnation: u64,
    session: u64,
    generation: u64,
    started: Instant,
    forced: Option<Arc<std::sync::atomic::AtomicBool>>,
}

impl Addons {
    fn language_path(&mut self, path: &Path) -> PathBuf {
        self.language_paths
            .entry(path.to_path_buf())
            .or_insert_with(|| absolute(path))
            .clone()
    }
    pub fn stop_language(&mut self) {
        if let Some(provider) = self.language.take() {
            if provider.stopped {
                return;
            }
            if let Some(process) = self
                .entries
                .iter()
                .find(|a| a.manifest.id == provider.addon && a.incarnation == provider.incarnation)
                .and_then(|a| a.process.as_ref())
            {
                let _ = process.send(provider.stop_message());
                self.language_retiring = Some(Retiring {
                    addon: provider.addon,
                    incarnation: provider.incarnation,
                    session: provider.session,
                    generation: provider.generation,
                    started: Instant::now(),
                    forced: None,
                });
            }
        }
    }
    pub fn language_providers(&self, path: &Path) -> Vec<(String, String)> {
        let extension = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or_default();
        self.entries
            .iter()
            .filter(|a| {
                a.enabled
                    && !self.suppressed.contains(&a.manifest.id)
                    && a.manifest
                        .languages
                        .iter()
                        .any(|l| l.extensions.iter().any(|e| e == extension))
            })
            .map(|a| (a.manifest.id.clone(), a.manifest.name.clone()))
            .collect()
    }
    #[allow(clippy::too_many_arguments)]
    pub fn update_language(
        &mut self,
        active: &Path,
        text: &str,
        cursor: usize,
        mut documents: Vec<(PathBuf, &str, bool)>,
        config: &lsp::Config,
        wake: bool,
        restart: bool,
        ctx: &egui::Context,
    ) -> lsp::Output {
        let missing = |status: &str| lsp::Output {
            status: status.into(),
            ..Default::default()
        };
        if !config.enabled || text.len() > lsp::MAX_FILE {
            self.stop_language();
            self.language_idle = None;
            return missing(if !config.enabled {
                "LSP disabled"
            } else {
                "Language features limited to files up to 1 MiB"
            });
        }
        let providers = self.language_providers(active);
        let ext = active
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or_default();
        let target = config
            .providers
            .get(ext)
            .filter(|id| providers.iter().any(|(a, _)| a == *id))
            .cloned()
            .or_else(|| (providers.len() == 1).then(|| providers[0].0.clone()));
        let Some(target) = target else {
            self.stop_language();
            self.language_idle = None;
            return missing(if providers.is_empty() {
                "Install and enable a language addon in Addons"
            } else {
                "Select a language provider in the editor context menu"
            });
        };
        if self.installation_pending(&target) {
            self.stop_language();
            return missing("Language addon update pending");
        }
        if restart {
            if let Some(entry) = self.entries.iter_mut().find(|a| a.manifest.id == target) {
                entry.error = None;
            }
        }
        let entry = self
            .entries
            .iter()
            .find(|a| a.manifest.id == target)
            .unwrap();
        if let Some(error) = &entry.error {
            let error = error.clone();
            self.stop_language();
            return missing(&error);
        }
        if entry.process.is_none() {
            let active_panel = self.active.clone();
            let detail = self.detail;
            self.activate(target.clone(), ctx);
            self.active = active_panel;
            self.detail = detail;
        }
        let entry = self
            .entries
            .iter()
            .find(|a| a.manifest.id == target)
            .unwrap();
        if !entry.process.as_ref().is_some_and(|p| p.ready) {
            ctx.request_repaint_after(Duration::from_secs(1));
            return missing("Starting language addon…");
        }
        let language = entry
            .manifest
            .languages
            .iter()
            .find(|l| l.extensions.iter().any(|e| e == ext))
            .unwrap()
            .id
            .clone();
        let supported: Vec<_> = entry
            .manifest
            .languages
            .iter()
            .filter(|l| l.id == language)
            .flat_map(|l| l.extensions.iter().cloned())
            .collect();
        let incarnation = entry.incarnation;
        let storage = entry.storage.clone();
        let mut context = self.context.clone();
        context.storage = storage;
        context.state = entry.seed.clone();
        let active_path = self.language_path(active);
        // Outside-container documents deliberately use a local server.
        if context
            .runtime
            .as_ref()
            .is_some_and(|r| !active_path.starts_with(&r.host_workspace))
        {
            context.runtime = None;
            context.workspace = active_path.parent().map(Path::to_path_buf);
        }
        for (path, _, _) in &mut documents {
            *path = self.language_path(path);
        }
        documents.retain(|(p, _, _)| {
            supported
                .iter()
                .any(|e| p.extension().is_some_and(|x| x == e.as_str()))
                && context
                    .runtime
                    .as_ref()
                    .is_none_or(|r| p.starts_with(&r.host_workspace))
        });
        let changed = self.language.as_ref().is_some_and(|p| {
            p.addon != target
                || p.incarnation != incarnation
                || p.generation != self.context.generation
                || p.language != language
                || p.context.runtime != context.runtime
        });
        if changed || restart {
            self.stop_language();
            self.language_idle = None;
        }
        if self.language_retiring.is_some() {
            ctx.request_repaint_after(Duration::from_millis(50));
            return missing("Stopping previous language server…");
        }
        if let Some(provider) = &mut self.language {
            provider.update(&active_path, cursor, documents);
            if wake {
                provider.activate();
            }
            if provider.idle() {
                self.language_idle = Some((active.to_path_buf(), text.into(), cursor));
                self.stop_language();
                return missing("LSP idle — interact with the document to restart");
            }
        } else {
            if !wake
                && !restart
                && self
                    .language_idle
                    .as_ref()
                    .is_some_and(|(p, t, c)| p == active && t == text && *c == cursor)
            {
                return missing("LSP idle — interact with the document to restart");
            }
            self.language_idle = None;
            self.language_session += 1;
            let server = config.servers.get(&target);
            let start = Start {
                generation: self.context.generation,
                session: self.language_session,
                language,
                context,
                command: server.map(|s| s.command.clone()),
                args: server.map_or_else(Vec::new, |s| s.args.clone()),
            };
            let mut provider = Provider::new(target, incarnation, start);
            provider.update(&active_path, cursor, documents);
            self.language = Some(provider);
        }
        self.poll_language();
        // Schedule actual work deadlines; idle language support does not animate.
        if let Some(provider) = &self.language {
            ctx.request_repaint_after(provider.next_wake());
        }
        self.language
            .as_ref()
            .map(|p| p.output.clone())
            .unwrap_or_default()
    }
    pub(super) fn poll_language(&mut self) {
        if let Some(retiring) = &mut self.language_retiring {
            if retiring
                .forced
                .as_ref()
                .is_some_and(|done| done.load(Ordering::Acquire))
            {
                self.language_retiring = None;
            } else if retiring.forced.is_none()
                && retiring.started.elapsed() > Duration::from_secs(3)
            {
                let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
                let finished = done.clone();
                let process = self
                    .entries
                    .iter_mut()
                    .find(|a| {
                        a.manifest.id == retiring.addon && a.incarnation == retiring.incarnation
                    })
                    .and_then(|a| {
                        a.error =
                            Some("Language server cleanup timed out. Restart the addon.".into());
                        a.process.take()
                    });
                std::thread::spawn(move || {
                    if let Some(process) = process {
                        process.stop_now();
                    }
                    finished.store(true, Ordering::Release);
                });
                retiring.forced = Some(done);
            }
        }
        if let Some(provider) = &mut self.language {
            let entry = self.entries.iter().find(|a| {
                a.manifest.id == provider.addon
                    && a.incarnation == provider.incarnation
                    && a.enabled
                    && a.process.is_some()
            });
            if entry.is_none() {
                self.stop_language();
                return;
            }
            provider.tick();
        }
        self.flush_language();
    }
    pub fn flush_language(&mut self) {
        if let Some(provider) = &mut self.language {
            let process = self
                .entries
                .iter()
                .find(|a| a.manifest.id == provider.addon && a.incarnation == provider.incarnation)
                .and_then(|a| a.process.as_ref());
            if let Some(process) = process {
                for message in std::mem::take(&mut provider.outgoing) {
                    if let Err(error) = process.send(message) {
                        provider.fail(&error);
                        break;
                    }
                }
            }
        }
    }
    pub(super) fn language_event(&mut self, addon: &str, incarnation: u64, message: Message) {
        if message.method.as_deref() == Some("lsp.stopped") {
            if let Some(provider) = &mut self.language {
                if provider.addon == addon
                    && provider.incarnation == incarnation
                    && message.params.as_ref().is_some_and(|v| {
                        v["generation"] == provider.generation && v["session"] == provider.session
                    })
                {
                    provider.stopped = true;
                }
            }
            if self.language_retiring.as_ref().is_some_and(|r| {
                r.addon == addon
                    && r.incarnation == incarnation
                    && message.params.as_ref().is_some_and(|v| {
                        v["generation"] == r.generation && v["session"] == r.session
                    })
            }) {
                self.language_retiring = None;
            }
            return;
        }
        if message.method.as_deref() == Some("lsp.retry") {
            if self.language.as_ref().is_some_and(|p| {
                p.addon == addon
                    && p.incarnation == incarnation
                    && message
                        .params
                        .as_ref()
                        .is_some_and(|v| v["generation"] == p.generation)
            }) {
                self.stop_language();
                self.language_idle = None;
            }
            return;
        }
        if let Some(provider) = &mut self.language {
            if provider.addon == addon && provider.incarnation == incarnation {
                provider.event(message);
            }
        }
    }
}
fn absolute(path: &Path) -> PathBuf {
    if let Ok(path) = path.canonicalize() {
        return path;
    }
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().unwrap_or_default().join(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use focus_addon_sdk::language::Language;
    use serde_json::json;
    use std::{fs, os::unix::fs::PermissionsExt, thread};

    const ADDON: &str = r#"#!/usr/bin/env python3
import json, sys
state = {}
def emit(method, params):
 print(json.dumps(dict(jsonrpc='2.0',method=method,params=params)),flush=True)
def lsp(message):
 emit('lsp.message',dict(generation=state['generation'],session=state['session'],message=dict(jsonrpc='2.0',**message)))
for line in sys.stdin:
 v=json.loads(line); p=v.get('params',{}); m=v.get('method')
 if m=='initialize': print(json.dumps(dict(jsonrpc='2.0',id=v['id'],result=dict(protocol=1,language=1))),flush=True)
 elif m=='lsp.start':
  state=p
  emit('lsp.started',dict(generation=p['generation'],session=p['session'],settings={}))
 elif m=='lsp.message':
  q=p['message']; method=q.get('method')
  if method=='initialize': lsp(dict(id=q['id'],result=dict(capabilities=dict(textDocumentSync=2,documentSymbolProvider=True,completionProvider={},documentFormattingProvider=True))))
  elif method=='textDocument/didOpen':
   d=q['params']['textDocument']
   lsp(dict(method='textDocument/publishDiagnostics',params=dict(uri=d['uri'],version=d['version'],diagnostics=[dict(range=dict(start=dict(line=0,character=0),end=dict(line=0,character=2)),severity=1,source='fixture',message='wrong type')])))
  elif method=='textDocument/documentSymbol': lsp(dict(id=q['id'],result=[]))
  elif method=='textDocument/completion': lsp(dict(id=q['id'],result=[dict(label='print')]))
  elif method=='textDocument/formatting': lsp(dict(id=q['id'],result=[dict(range=dict(start=dict(line=0,character=0),end=dict(line=0,character=2)),newText='print()')]))
 elif m=='lsp.stop': emit('lsp.stopped',p)
 elif m=='shutdown': break
"#;
    fn fixture() -> (tempfile::TempDir, Addons, egui::Context, PathBuf) {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("addons/fixture");
        fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join("program"), ADDON).unwrap();
        fs::set_permissions(directory.join("program"), fs::Permissions::from_mode(0o755)).unwrap();
        let storage = root.path().join("storage");
        fs::create_dir_all(&storage).unwrap();
        let path = root.path().join("file.py");
        fs::write(&path, "pr").unwrap();
        let mut host = Addons::default();
        host.sync(Some(root.path()), None);
        host.entries.push(Addon {
            manifest: Manifest {
                id: "fixture".into(),
                name: "Fixture".into(),
                protocol: 1,
                executable: "program".into(),
                languages: vec![Language {
                    id: "python".into(),
                    extensions: vec!["py".into()],
                }],
                ..Default::default()
            },
            directory,
            storage,
            enabled: true,
            process: None,
            error: None,
            view: Default::default(),
            values: Default::default(),
            icon: None,
            working: false,
            incarnation: 0,
            surface: Default::default(),
            seed: Default::default(),
            status: String::new(),
        });
        (root, host, egui::Context::default(), path)
    }
    fn wait(
        host: &mut Addons,
        ctx: &egui::Context,
        path: &Path,
        predicate: impl Fn(&Addons) -> bool,
    ) {
        let until = Instant::now() + Duration::from_secs(5);
        loop {
            host.tick(ctx, false, false);
            host.update_language(
                path,
                "pr",
                2,
                vec![(path.into(), "pr".into(), false)],
                &lsp::Config::default(),
                false,
                false,
                ctx,
            );
            if predicate(host) {
                return;
            }
            assert!(
                Instant::now() < until,
                "Language fixture timed out: {:?}",
                host.entries[0].error
            );
            thread::sleep(Duration::from_millis(5));
        }
    }
    #[test]
    fn addon_bridge_routes_features_and_waits_for_cleanup_before_restart() {
        let (_root, mut host, ctx, path) = fixture();
        host.active = Some("unrelated".into());
        wait(&mut host, &ctx, &path, |h| {
            h.language
                .as_ref()
                .is_some_and(|p| p.output.ready && !p.diagnostics.is_empty())
        });
        assert_eq!(host.active.as_deref(), Some("unrelated"));
        let old_session = host.language.as_ref().unwrap().session;
        host.language.as_mut().unwrap().complete();
        host.flush_language();
        wait(&mut host, &ctx, &path, |h| {
            !h.language.as_ref().unwrap().completions.is_empty()
        });
        host.language.as_mut().unwrap().accept_completion(0);
        let edit = host.language.as_mut().unwrap().changes.pop_front().unwrap();
        assert_eq!(edit.after, "print");
        host.language.as_mut().unwrap().format();
        host.flush_language();
        wait(&mut host, &ctx, &path, |h| {
            !h.language.as_ref().unwrap().changes.is_empty()
        });
        assert_eq!(
            host.language
                .as_mut()
                .unwrap()
                .changes
                .pop_front()
                .unwrap()
                .after,
            "print()"
        );
        let output = host.update_language(
            &path,
            "pr",
            2,
            vec![(path.clone(), "pr".into(), false)],
            &lsp::Config::default(),
            false,
            true,
            &ctx,
        );
        assert!(output.status.contains("Stopping"));
        assert!(host.language.is_none());
        assert!(host.language_retiring.is_some());
        wait(&mut host, &ctx, &path, |h| {
            h.language.as_ref().is_some_and(|p| p.output.ready)
        });
        assert!(host.language.as_ref().unwrap().session > old_session);
        let incarnation = host.entries[0].incarnation;
        let p = host.language.as_ref().unwrap();
        let message = Message::notification(
            "lsp.status",
            json!({"generation":p.generation,"session":p.session,"status":"obsolete","failed":true}),
        );
        host.language_event("fixture", incarnation + 1, message);
        assert!(host.language.as_ref().unwrap().output.ready);
        host.entries[0].enabled = false;
        host.poll_language();
        assert!(host.language.is_none());
        assert_eq!(fs::read_to_string(path).unwrap(), "pr");
    }
    #[test]
    fn absent_or_disabled_addons_never_launch_a_fallback_server() {
        let (_root, mut host, ctx, path) = fixture();
        host.entries[0].enabled = false;
        let output = host.update_language(
            &path,
            "pr",
            2,
            vec![],
            &lsp::Config::default(),
            false,
            false,
            &ctx,
        );
        assert!(output.status.contains("Install"));
        assert!(host.entries[0].process.is_none());
        assert!(host.language.is_none());
        host.entries[0].enabled = true;
        let config = lsp::Config {
            enabled: false,
            ..Default::default()
        };
        let output = host.update_language(&path, "pr", 2, vec![], &config, false, false, &ctx);
        assert_eq!(output.status, "LSP disabled");
        assert!(host.entries[0].process.is_none());
    }
}
