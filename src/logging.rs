//! Logging configuration and launch argument parsing; protocol content is Debug-only.
use std::{ffi::OsString, io::Write, path::PathBuf, str::FromStr};
pub(crate) mod process;
pub(crate) mod redaction;
pub(crate) mod session;

#[derive(Clone, Debug)]
pub(crate) struct Arguments {
    pub(crate) version: bool,
    pub(crate) help: bool,
    pub(crate) ui: bool,
    pub(crate) headless: bool,
    pub(crate) level: Option<log::LevelFilter>,
    pub(crate) workspace: Option<PathBuf>,
    pub(crate) disabled_addons: Vec<String>,
}
impl Default for Arguments {
    fn default() -> Self {
        Self {
            version: false,
            help: false,
            ui: true,
            headless: false,
            level: None,
            workspace: None,
            disabled_addons: Vec::new(),
        }
    }
}

pub(crate) const HELP: &str = "Focus Editor — native desktop editor

Usage: focus-editor [OPTIONS] [WORKSPACE]

Options:
  --ui                     Native desktop window (default)
  --browser                Chromium/Chrome app window (requires installed browser)
  --headless               Workspace session without a local window; requires WORKSPACE
  --foreground             Stay attached to the launching terminal
  --no-addon ID            Suppress an addon for this launch; repeatable
  --log-level LEVEL        trace, debug, info, warn, error, or off
  -V, --version            Print version
  -h, --help               Print this help
  --                       Treat following arguments as workspace paths

Window modes are mutually exclusive. Native mode requires a desktop display.
For remote browser access, run focus-editor-server.

Examples:
  focus-editor ./my-project
  focus-editor --browser ./my-project
  focus-editor --ui --foreground --log-level debug ./my-project";

impl Arguments {
    pub(crate) fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Self, String> {
        let mut result = Self::default();
        let mut args = args.into_iter();
        let mut positional = false;
        let mut mode: Option<OsString> = None;
        while let Some(arg) = args.next() {
            if !positional && (arg == "--help" || arg == "-h") {
                result.help = true;
                continue;
            }
            if !positional && (arg == "--ui" || arg == "--browser" || arg == "--headless") {
                if mode.as_ref().is_some_and(|old| old != &arg) {
                    return Err("Choose only one of --ui, --browser, or --headless".into());
                }
                result.ui = arg == "--ui";
                result.headless = arg == "--headless";
                mode = Some(arg);
                continue;
            }
            if !positional
                && matches!(
                    arg.to_str().unwrap_or("").split('=').next().unwrap_or(""),
                    "--server" | "--bind" | "--port" | "--server-config" | "--config"
                )
            {
                return Err(
                    "Server mode moved to focus-editor-server; run focus-editor-server instead"
                        .into(),
                );
            }
            if !positional && (arg == "--version" || arg == "-V") {
                result.version = true;
                continue;
            }
            if !positional && arg == "--" {
                positional = true;
                continue;
            }
            let text = arg.to_string_lossy();
            if !positional && (text == "--no-addon" || text.starts_with("--no-addon=")) {
                let id = if text == "--no-addon" {
                    args.next()
                        .ok_or("--no-addon requires an addon ID")?
                        .to_string_lossy()
                        .into_owned()
                } else {
                    text[11..].to_owned()
                };
                if id.is_empty() {
                    return Err("--no-addon requires an addon ID".into());
                }
                result.disabled_addons.push(id);
                continue;
            }
            let value = if !positional && arg == "--log-level" {
                Some(
                    args.next()
                        .ok_or("--log-level requires trace, debug, info, warn, error, or off")?,
                )
            } else if !positional && text.starts_with("--log-level=") {
                Some(OsString::from(&text[12..]))
            } else {
                None
            };
            if let Some(value) = value {
                let value = value.to_str().ok_or("Invalid --log-level value")?;
                result.level = Some(log::LevelFilter::from_str(value).map_err(
                    |_| "Invalid --log-level; use trace, debug, info, warn, error, or off",
                )?);
            } else if (positional || !text.starts_with('-')) && result.workspace.is_none() {
                result.workspace = Some(arg.into());
            }
        }
        if !result.help && !result.version && result.headless && result.workspace.is_none() {
            return Err("--headless requires a workspace".into());
        }
        Ok(result)
    }
}
fn filter(level: Option<log::LevelFilter>, environment: Option<String>) -> String {
    if let Some(level) = level {
        format!(
            "{},focus_editor={}",
            level.min(log::LevelFilter::Warn).as_str().to_lowercase(),
            level.as_str().to_lowercase()
        )
    } else {
        environment
            .filter(|v| !v.trim().is_empty())
            .unwrap_or_else(|| "warn,focus_editor=info".into())
    }
}
pub(crate) fn init(arguments: &Arguments) -> Result<(), Box<dyn std::error::Error>> {
    init_at(arguments, &session::log_path()?)
}

pub(crate) fn init_server(
    level: Option<log::LevelFilter>,
) -> Result<(), Box<dyn std::error::Error>> {
    init_at(
        &Arguments {
            level,
            ..Default::default()
        },
        &crate::app_paths::data_root()?.join("logs/server.log"),
    )
}

fn init_at(
    arguments: &Arguments,
    path: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let filter = filter(arguments.level, std::env::var("RUST_LOG").ok());
    let detached = std::env::args_os().any(|a| a == "--focus-editor-detached");
    let disk = if detached {
        None
    } else {
        Some(session::fresh_file(path)?)
    };
    let disk = std::sync::Mutex::new(disk);
    let mut logger = env_logger::Builder::new();
    logger
        .parse_filters(&filter)
        .target(env_logger::Target::Stderr)
        .format(move |buffer, record| {
            let timestamp = buffer.timestamp_millis().to_string();
            let source = session::Source::from_target(record.target());
            // Protocol records are already redacted and bounded before reaching the facade.
            let messages = redaction::sanitize(&record.args().to_string());
            for message in messages {
                let text = format!(
                    "{} {:5} pid={} [{}] {}",
                    timestamp,
                    record.level(),
                    std::process::id(),
                    record.target(),
                    message
                );
                // egui/epaint can log while holding the context lock. Waking that same
                // context here would recursively acquire it and deadlock the renderer.
                let repaint =
                    !matches!(record.target().split("::").next(), Some("egui" | "epaint"));
                session::push(
                    session::Entry {
                        timestamp: timestamp.clone(),
                        level: record.level(),
                        source,
                        text: text.clone(),
                    },
                    repaint,
                );
                if let Some(file) = disk.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
                    if let Err(error) = writeln!(file, "{text}").and_then(|_| file.flush()) {
                        eprintln!("Could not write current-session application log: {error}");
                    }
                }
                writeln!(buffer, "{text}")?;
            }
            Ok(())
        });
    logger.try_init()?;
    Ok(())
}

/// Subprocess diagnostics are never protocol payloads or terminal contents.
pub(crate) fn process_line(target: &str, line: &str) {
    if line.trim().is_empty() {
        return;
    }
    let lower = line.to_ascii_lowercase();
    let level = if lower.contains("error") || lower.contains("failed") {
        log::Level::Error
    } else if lower.contains("warn") {
        log::Level::Warn
    } else {
        log::Level::Info
    };
    // Sanitize before the facade too: alternate/test loggers must not see secrets.
    for text in redaction::sanitize(line) {
        log::log!(target: target, level, "{text}");
    }
}

#[cfg(test)]
mod tests;
