use super::protocol::*;
use super::*;
use std::io::{BufReader, Cursor};

#[test]
fn framing_is_bounded_and_handles_consecutive_messages() {
    let value = json!({"jsonrpc":"2.0","id":3,"result":"é😀"});
    let mut bytes = Vec::new();
    write_message(&mut bytes, &value).unwrap();
    write_message(&mut bytes, &value).unwrap();
    let mut reader = BufReader::with_capacity(3, Cursor::new(bytes));
    assert_eq!(read_message(&mut reader).unwrap(), Some(value.clone()));
    assert_eq!(read_message(&mut reader).unwrap(), Some(value));
    assert_eq!(read_message(&mut reader).unwrap(), None);
    for text in [
        "Content-Length: 8388609\r\n\r\n",
        "Content-Length: 2\r\nContent-Length: 2\r\n\r\n{}",
        "Content-Length: 50\r\n\r\n{}",
        "Content-Length: 2\n\n{}",
        "\r\n",
    ] {
        assert!(read_message(&mut Cursor::new(text)).is_err(), "{text}");
    }
}
#[test]
fn utf16_offsets_handle_surrogates_crlf_and_invalid_positions() {
    let text = "é😀\r\nfn test() {}\n";
    assert_eq!(
        position(text, 2),
        Position {
            line: 0,
            character: 3
        }
    );
    assert_eq!(
        offset(
            text,
            Position {
                line: 0,
                character: 3
            }
        ),
        Some(2)
    );
    assert_eq!(
        offset(
            text,
            Position {
                line: 0,
                character: 2
            }
        ),
        None
    );
    assert_eq!(
        offset(
            text,
            Position {
                line: 1,
                character: 0
            }
        ),
        Some(4)
    );
    assert_eq!(
        offset(
            text,
            Position {
                line: 3,
                character: 0
            }
        ),
        None
    );
    assert_eq!(
        offset(
            text,
            Position {
                line: 0,
                character: 4
            }
        ),
        None
    );
    assert_eq!(
        offset(
            text,
            Position {
                line: 2,
                character: 0
            }
        ),
        Some(text.chars().count())
    );
}
#[test]
fn incremental_replacement_round_trips_edits() {
    for (old, new) in [
        ("é😀 one\r\nend", "é😀 two\r\nend"),
        ("", "😀"),
        ("hello", ""),
        ("abc", "a\nβc"),
        ("unchanged", "unchanged"),
        ("a\r\nb", "a\nb"),
        ("a\nb", "a\r\nb"),
        ("a\r\nb", "a\r\nβ"),
        ("a\r\nb", "a\rX\nb"),
    ] {
        let edit = change(old, new);
        let range: Range = serde_json::from_value(edit["range"].clone()).unwrap();
        let start = offset(old, range.start).unwrap();
        let end = offset(old, range.end).unwrap();
        let result = old.chars().take(start).collect::<String>()
            + edit["text"].as_str().unwrap()
            + &old.chars().skip(end).collect::<String>();
        assert_eq!(result, new);
    }
}
fn range(start: (u32, u32), end: (u32, u32)) -> Value {
    json!({"start":{"line":start.0,"character":start.1},"end":{"line":end.0,"character":end.1}})
}
#[test]
fn hierarchical_and_flat_symbols_have_safe_targets() {
    let text = "class C:\n    def f(self):\n        pass\n";
    let symbols_json = json!([{"name":"C","kind":5,"range":range((0,0),(2,12)),"selectionRange":range((0,6),(0,7)),"children":[{"name":"f","kind":6,"range":range((1,4),(2,12)),"selectionRange":range((1,8),(1,9))}]}]);
    let list = symbols(&symbols_json, text, "file:///a.py").unwrap();
    assert_eq!(list.len(), 2);
    assert_eq!(list[1].parent, Some(0));
    assert_eq!(list[1].target, 17);
    let flat = json!([{"name":"C","kind":5,"location":{"uri":"file:///a.py","range":range((0,0),(2,12))}},{"name":"f","kind":6,"location":{"uri":"file:///a.py","range":range((1,8),(1,9))}},{"name":"other","location":{"uri":"file:///else.py","range":range((0,0),(0,1))}}]);
    let list = symbols(&flat, text, "file:///a.py").unwrap();
    assert_eq!(list.len(), 2);
    assert_eq!(list[1].parent, Some(0));
    assert!(!list[1].enclosing);
    assert!(
        symbols(&json!(null), text, "file:///a.py")
            .unwrap()
            .is_empty()
    );
    let invalid = json!([{"name":"bad","range":range((99,0),(99,3))}]);
    assert!(symbols(&invalid, text, "file:///a.py").unwrap().is_empty());
}
#[test]
fn resource_settings_disable_expensive_services() {
    let rust = settings(Language::Rust);
    assert_eq!(rust["checkOnSave"], false);
    assert_eq!(rust["numThreads"], 2);
    assert_eq!(rust["cargo"]["buildScripts"]["enable"], false);
    assert_eq!(rust["procMacro"]["enable"], false);
    let python = settings(Language::Python);
    assert_eq!(python["pylsp"]["plugins"]["jedi_symbols"]["enabled"], true);
    assert_eq!(python["pylsp"]["plugins"]["pyflakes"]["enabled"], false);
}
#[test]
fn settings_remain_backward_compatible() {
    let config: Config = toml::from_str("").unwrap();
    assert!(config.enabled);
    assert_eq!(config.python.command, "pylsp");
    let settings: crate::settings::Settings = toml::from_str(
        "[lsp]\nenabled = false\n[lsp.python]\ncommand = '/venv/bin/pylsp'\nargs = ['--verbose']\n",
    )
    .unwrap();
    assert!(!settings.lsp.enabled);
    assert_eq!(settings.lsp.python.args, ["--verbose"]);
}

// A real child process exercising both protocol directions, unsaved synchronization,
// stale-response rejection, configuration requests and orderly shutdown.
const FAKE_SERVER: &str = r#"
import sys,json,time,os
log=sys.argv[1]
def write(v):
 b=json.dumps(dict(jsonrpc='2.0',**v)).encode();sys.stdout.buffer.write(('Content-Length: %d\r\n\r\n'%len(b)).encode()+b);sys.stdout.buffer.flush()
def read():
 n=None
 while True:
  line=sys.stdin.buffer.readline()
  if not line:return None
  if line==b'\r\n':break
  if line.lower().startswith(b'content-length:'):n=int(line.split(b':')[1])
 return json.loads(sys.stdin.buffer.read(n))
with open(log,'a') as f:f.write('START '+str(os.getpid())+'\n')
while True:
 v=read()
 if v is None:break
 method=v.get('method','')
 with open(log,'a') as f:f.write(json.dumps(v)+'\n')
 if method=='initialize':
  write(dict(id=v['id'],result=dict(capabilities=dict(textDocumentSync=dict(openClose=True,change=2),documentSymbolProvider=True))))
 elif method=='initialized':write(dict(id='config',method='workspace/configuration',params=dict(items=[dict(section='pylsp')])))
 elif method=='textDocument/documentSymbol':
  write(dict(id=v['id'],result=[dict(name='main',kind=12,range=dict(start=dict(line=0,character=0),end=dict(line=0,character=12)),selectionRange=dict(start=dict(line=0,character=3),end=dict(line=0,character=7)))]))
 elif method=='shutdown':write(dict(id=v['id'],result=None))
 elif method=='exit':break
with open(log,'a') as f:f.write('EXIT\n')
"#;
fn fake_config(log: &Path) -> Config {
    let server = ServerConfig {
        command: "python3".into(),
        args: vec![
            "-u".into(),
            "-c".into(),
            FAKE_SERVER.into(),
            log.display().to_string(),
        ],
    };
    Config {
        enabled: true,
        rust: server.clone(),
        python: server,
    }
}
fn wait_until(mut condition: impl FnMut() -> bool) {
    let until = Instant::now() + Duration::from_secs(8);
    while Instant::now() < until {
        if condition() {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("Timed out waiting for LSP worker");
}
#[test]
fn worker_coalesces_edits_caches_caret_motion_and_reaps_child() {
    let root = tempfile::tempdir().unwrap();
    let log = root.path().join("events");
    let file = root.path().join("a.py");
    let config = fake_config(&log);
    let ctx = egui::Context::default();
    let mut client = Client::default();
    let tick = |client: &mut Client, text: &str, cursor| {
        client.tick(
            Some(&file),
            text,
            Some(root.path()),
            None,
            &config,
            cursor,
            &ctx,
        )
    };
    wait_until(|| {
        tick(&mut client, "def main(): pass", 0);
        client.output.ready
    });
    assert_eq!(client.output.symbols[0].name, "main");
    for cursor in 0..10 {
        tick(&mut client, "def main(): pass", cursor);
    }
    for text in ["def main(): a", "def main(): b", "def main(): c"] {
        tick(&mut client, text, 0);
    }
    assert!(
        !client.output.ready,
        "Old symbol offsets must not be selectable"
    );
    wait_until(|| {
        tick(&mut client, "def main(): c", 0);
        client.output.ready
    });
    drop(client);
    wait_until(|| {
        std::fs::read_to_string(&log)
            .unwrap_or_default()
            .contains("EXIT")
    });
    let events = std::fs::read_to_string(log).unwrap();
    assert_eq!(
        events.matches("textDocument/documentSymbol").count(),
        2,
        "{events}"
    );
    assert_eq!(
        events.matches("textDocument/didChange").count(),
        1,
        "{events}"
    );
    assert!(events.contains("\"id\": \"config\""));
    assert!(events.contains("shutdown"));
}
#[test]
fn missing_server_does_not_restart_on_every_frame() {
    let ctx = egui::Context::default();
    let mut client = Client::default();
    let config = Config {
        rust: ServerConfig {
            command: "/missing/focus-test-server".into(),
            args: vec![],
        },
        ..Default::default()
    };
    let tick = |client: &mut Client| {
        client.tick(
            Some(Path::new("/test.rs")),
            "fn main() {}",
            None,
            None,
            &config,
            0,
            &ctx,
        )
    };
    wait_until(|| {
        tick(&mut client);
        client.output.status.contains("Cannot start")
    });
    let status = client.output.status.clone();
    for _ in 0..100 {
        tick(&mut client);
        assert_eq!(client.output.status, status);
    }
}

#[test]
#[ignore = "Set FOCUS_TEST_RUST_ANALYZER and FOCUS_TEST_PYLSP to installed server executables"]
fn real_servers_navigate_unsaved_rust_and_python() {
    for (variable, name, text, changed) in [
        (
            "FOCUS_TEST_RUST_ANALYZER",
            "main.rs",
            "struct Hello;\nimpl Hello { fn method(&self) {} }\nfn main() {}\n",
            "struct Hello;\nimpl Hello { fn updated(&self) {} }\nfn main() {}\n",
        ),
        (
            "FOCUS_TEST_PYLSP",
            "main.py",
            "class Hello:\n    def method(self):\n        pass\n",
            "class Hello:\n    def updated(self):\n        pass\n",
        ),
    ] {
        let command = std::env::var(variable).expect(variable);
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join(name);
        std::fs::write(&file, text).unwrap();
        let server = ServerConfig {
            command,
            args: vec![],
        };
        let config = Config {
            enabled: true,
            rust: server.clone(),
            python: server,
        };
        let ctx = egui::Context::default();
        let mut client = Client::default();
        let tick = |client: &mut Client, source: &str| {
            client.tick(
                Some(&file),
                source,
                Some(root.path()),
                None,
                &config,
                0,
                &ctx,
            )
        };
        wait_until(|| {
            tick(&mut client, text);
            client.output.ready
        });
        assert!(
            client.output.symbols.iter().any(|s| s.name == "method"),
            "{}: {}",
            name,
            client.output.status
        );
        wait_until(|| {
            tick(&mut client, changed);
            client.output.ready
        });
        assert!(client.output.symbols.iter().any(|s| s.name == "updated"));
        assert!(!client.output.symbols.iter().any(|s| s.name == "method"));
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            text,
            "LSP navigation must not write files"
        );
        client.shutdown();
    }
}

fn fast_client(ctx: &egui::Context) -> Client {
    Client {
        worker: Some(start_worker_with_timing(
            ctx.clone(),
            Timing {
                startup: Duration::from_millis(10),
                edits: Duration::from_millis(20),
                idle: Duration::from_millis(800),
                request: Duration::from_millis(250),
                initialize: Duration::from_secs(2),
            },
        )),
        ..Default::default()
    }
}
#[test]
fn idle_stops_server_and_activation_restarts_without_frame_driven_restarts() {
    let root = tempfile::tempdir().unwrap();
    let log = root.path().join("events");
    let file = root.path().join("a.py");
    let ctx = egui::Context::default();
    let config = fake_config(&log);
    let mut client = fast_client(&ctx);
    let tick = |c: &mut Client| {
        c.tick(
            Some(&file),
            "def main(): pass",
            Some(root.path()),
            None,
            &config,
            0,
            &ctx,
        )
    };
    wait_until(|| {
        tick(&mut client);
        client.output.ready
    });
    wait_until(|| {
        tick(&mut client);
        client.output.status.contains("LSP idle")
    });
    for _ in 0..20 {
        tick(&mut client);
    }
    assert_eq!(
        std::fs::read_to_string(&log)
            .unwrap()
            .matches("START")
            .count(),
        1
    );
    client.activate();
    wait_until(|| {
        tick(&mut client);
        client.output.ready
    });
    client.shutdown();
    let events = std::fs::read_to_string(log).unwrap();
    assert_eq!(events.matches("START").count(), 2);
    assert_eq!(events.matches("EXIT").count(), 2);
}
#[test]
fn language_changes_finish_previous_process_before_starting_next() {
    let root = tempfile::tempdir().unwrap();
    let log = root.path().join("events");
    let py = root.path().join("a.py");
    let rs = root.path().join("a.rs");
    let ctx = egui::Context::default();
    let config = fake_config(&log);
    let mut client = fast_client(&ctx);
    wait_until(|| {
        client.tick(
            Some(&py),
            "def main(): pass",
            Some(root.path()),
            None,
            &config,
            0,
            &ctx,
        );
        client.output.ready
    });
    wait_until(|| {
        client.tick(
            Some(&rs),
            "fn main() {}",
            Some(root.path()),
            None,
            &config,
            0,
            &ctx,
        );
        client.output.ready
    });
    client.shutdown();
    let events = std::fs::read_to_string(log).unwrap();
    let starts: Vec<_> = events.match_indices("START").map(|p| p.0).collect();
    let first_exit = events.find("EXIT").unwrap();
    assert_eq!(starts.len(), 2);
    assert!(first_exit < starts[1]);
}
#[test]
fn stale_symbol_response_never_becomes_selectable() {
    let root = tempfile::tempdir().unwrap();
    let log = root.path().join("events");
    let file = root.path().join("a.py");
    let ctx = egui::Context::default();
    let mut config = fake_config(&log);
    let script = FAKE_SERVER.replace(
        "elif method=='textDocument/documentSymbol':",
        "elif method=='textDocument/documentSymbol':\n  time.sleep(0.2)",
    );
    config.python.args[2] = script;
    let mut client = Client::default();
    let tick = |c: &mut Client, text: &str| {
        c.tick(Some(&file), text, Some(root.path()), None, &config, 0, &ctx)
    };
    wait_until(|| {
        tick(&mut client, "def main(): pass");
        std::fs::read_to_string(&log)
            .unwrap_or_default()
            .contains("textDocument/documentSymbol")
    });
    tick(&mut client, "def main(): other");
    std::thread::sleep(Duration::from_millis(250));
    tick(&mut client, "def main(): other");
    assert!(
        !client.output.ready,
        "The first request described an obsolete buffer"
    );
    wait_until(|| {
        tick(&mut client, "def main(): other");
        client.output.ready
    });
    client.shutdown();
}
#[test]
fn unresponsive_server_is_cancelled_and_stopped_without_retry_loop() {
    let root = tempfile::tempdir().unwrap();
    let log = root.path().join("events");
    let file = root.path().join("a.py");
    let ctx = egui::Context::default();
    let mut config = fake_config(&log);
    config.python.args[2] = FAKE_SERVER.replace(
        "elif method=='textDocument/documentSymbol':",
        "elif method=='textDocument/documentSymbol':\n  continue",
    );
    let mut client = Client {
        worker: Some(start_worker_with_timing(
            ctx.clone(),
            Timing {
                idle: Duration::from_secs(3),
                request: Duration::from_millis(150),
                ..Default::default()
            },
        )),
        ..Default::default()
    };
    let tick = |c: &mut Client| {
        c.tick(
            Some(&file),
            "def main(): pass",
            Some(root.path()),
            None,
            &config,
            0,
            &ctx,
        )
    };
    wait_until(|| {
        tick(&mut client);
        client.output.status.contains("stopped responding")
    });
    for _ in 0..20 {
        tick(&mut client);
    }
    client.shutdown();
    let events = std::fs::read_to_string(log).unwrap();
    assert!(events.contains("$/cancelRequest"));
    assert_eq!(events.matches("START").count(), 1);
}

#[cfg(unix)]
#[test]
fn remote_wrapper_preserves_arguments_stdin_and_kills_its_process_group() {
    use std::process::{Command, Stdio};
    let root = tempfile::tempdir().unwrap();
    let marker = root.path().join("process.pid");
    let copied = root.path().join("quoted name.txt");
    let mut child = Command::new("sh")
        .args(["-c", transport::REMOTE_START, "focus-lsp"])
        .arg(&marker)
        .args(["sh", "-c", "cat > \"$1\"; sleep 30", "server"])
        .arg(&copied)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    use std::io::Write;
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"document with unicode: \xc3\xa9")
        .unwrap();
    wait_until(|| marker.exists() && std::fs::read(&copied).is_ok_and(|s| !s.is_empty()));
    assert_eq!(
        std::fs::read(&copied).unwrap(),
        b"document with unicode: \xc3\xa9"
    );
    let status = Command::new("sh")
        .args(["-c", transport::REMOTE_KILL, "cleanup"])
        .arg(&marker)
        .status()
        .unwrap();
    assert!(status.success());
    wait_until(|| child.try_wait().unwrap().is_some());
    assert!(!marker.exists());
}
#[test]
fn full_sync_and_unavailable_capabilities_are_handled() {
    for full_sync in [true, false] {
        let root = tempfile::tempdir().unwrap();
        let log = root.path().join("events");
        let file = root.path().join("a.py");
        let ctx = egui::Context::default();
        let mut config = fake_config(&log);
        config.python.args[2] = if full_sync {
            FAKE_SERVER.replace("change=2", "change=1")
        } else {
            FAKE_SERVER.replace(
                "documentSymbolProvider=True",
                "documentSymbolProvider=False",
            )
        };
        let mut client = Client::default();
        let tick = |c: &mut Client, text: &str| {
            c.tick(Some(&file), text, Some(root.path()), None, &config, 0, &ctx)
        };
        if full_sync {
            wait_until(|| {
                tick(&mut client, "def main(): pass");
                client.output.ready
            });
            wait_until(|| {
                tick(&mut client, "def main(): edit");
                client.output.ready
            });
            client.shutdown();
            let events = std::fs::read_to_string(log).unwrap();
            let change: Value = events
                .lines()
                .filter_map(|line| serde_json::from_str::<Value>(line).ok())
                .find(|value| value["method"] == "textDocument/didChange")
                .unwrap();
            assert_eq!(
                change["params"]["contentChanges"],
                json!([{"text":"def main(): edit"}])
            );
        } else {
            wait_until(|| {
                tick(&mut client, "def main(): pass");
                client
                    .output
                    .status
                    .contains("does not provide document symbols")
            });
            client.shutdown();
        }
    }
}
