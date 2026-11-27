//! Offline protocol stubs for container-addon-smoke; never contact an AI provider.
use std::{
    fs::OpenOptions,
    io::{BufRead, BufReader, Write},
    net::{TcpListener, TcpStream},
    process::Command,
    time::Duration,
};
fn record(tool: &str, text: &str) {
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(format!("/tmp/{tool}-fixture.log"))
        .unwrap();
    writeln!(file, "{text}").unwrap();
}
fn main() {
    let program = std::env::args().next().unwrap();
    let tool = if program.ends_with("opencode") {
        "opencode"
    } else {
        "codex"
    };
    record(
        tool,
        &std::env::args().skip(1).collect::<Vec<_>>().join(" "),
    );
    let child = Command::new("sh")
        .args(["-c", "sleep 900"])
        .spawn()
        .unwrap();
    record(tool, &format!("child={}", child.id()));
    if tool == "opencode" {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        println!(
            "opencode server listening on http://{}",
            listener.local_addr().unwrap()
        );
        for stream in listener.incoming() {
            std::thread::spawn(move || http(stream.unwrap()));
        }
    } else {
        for line in std::io::stdin().lock().lines().map_while(Result::ok) {
            if line.contains("turn/start") {
                panic!("Smoke tests must never send a model prompt");
            }
            let Some(id) = line.split("\"id\":").nth(1).and_then(|s| {
                s.chars()
                    .take_while(|c| c.is_ascii_digit())
                    .collect::<String>()
                    .parse::<u64>()
                    .ok()
            }) else {
                continue;
            };
            record(tool, &line);
            let result = if line.contains("thread/start") || line.contains("thread/resume") {
                r#"{"thread":{"id":"fixture-session","turns":[]}}"#
            } else {
                r#"{"data":[],"rateLimits":{},"userAgent":"fixture"}"#
            };
            println!("{{\"id\":{id},\"result\":{result}}}");
        }
    }
}
fn http(mut stream: TcpStream) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    record("opencode", line.trim());
    let path = line
        .split_whitespace()
        .nth(1)
        .unwrap()
        .split('?')
        .next()
        .unwrap();
    if path == "/event" {
        let _ = write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n"
        );
        while stream.write_all(b": heartbeat\n\n").is_ok() {
            std::thread::sleep(Duration::from_millis(50));
        }
        return;
    }
    let body = match path {
        "/global/health" => r#"{"version":"fixture"}"#,
        "/provider" => r#"{"all":[],"connected":[]}"#,
        "/agent" | "/permission" | "/question" => "[]",
        "/session" if line.starts_with("POST") => r#"{"id":"fixture-session"}"#,
        "/session" => "[]",
        _ if path.ends_with("/message") => "[]",
        _ => r#"{"id":"fixture-session"}"#,
    };
    let _ = write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
}
