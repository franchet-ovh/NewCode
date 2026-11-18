//! Bounded process-local records and a fresh, single-session diagnostic file.
use std::{
    collections::VecDeque,
    fs::{File, OpenOptions},
    io,
    path::{Path, PathBuf},
    sync::{LazyLock, Mutex},
};
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Source {
    Editor,
    Git,
    DevContainer,
    DockerCompose,
    Terminal,
    Addons,
}
impl Source {
    pub(crate) const ALL: [Self; 6] = [
        Self::Editor,
        Self::Git,
        Self::DevContainer,
        Self::DockerCompose,
        Self::Terminal,
        Self::Addons,
    ];
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Editor => "Editor",
            Self::Git => "Git",
            Self::DevContainer => "Dev Container",
            Self::DockerCompose => "Docker Compose",
            Self::Terminal => "Terminal",
            Self::Addons => "Addons",
        }
    }
    pub(crate) fn from_target(target: &str) -> Self {
        if target.contains("addons") {
            Self::Addons
        } else if target.contains("dev_container::compose") {
            Self::DockerCompose
        } else if target.contains("dev_container") {
            Self::DevContainer
        } else if target.contains("git_ui") {
            Self::Git
        } else if target.contains("terminal") || target.ends_with("::shell") {
            Self::Terminal
        } else {
            Self::Editor
        }
    }
}
#[derive(Clone)]
pub(crate) struct Entry {
    pub timestamp: String,
    pub level: log::Level,
    pub source: Source,
    pub text: String,
}
#[derive(Default)]
struct Store {
    entries: VecDeque<std::sync::Arc<Entry>>,
    bytes: usize,
    errors: usize,
    warnings: usize,
    context: Option<eframe::egui::Context>,
}
impl Store {
    fn push(&mut self, entry: Entry) {
        self.bytes += entry.text.len();
        self.errors += usize::from(entry.level == log::Level::Error);
        self.warnings += usize::from(entry.level == log::Level::Warn);
        self.entries.push_back(std::sync::Arc::new(entry));
        while self.entries.len() > 10_000 || self.bytes > 8 * 1024 * 1024 {
            if let Some(entry) = self.entries.pop_front() {
                self.bytes -= entry.text.len();
                self.errors -= usize::from(entry.level == log::Level::Error);
                self.warnings -= usize::from(entry.level == log::Level::Warn);
            }
        }
    }
    fn clear(&mut self, source: Option<Source>) {
        self.entries
            .retain(|entry| source.is_some_and(|s| entry.source != s));
        self.bytes = self.entries.iter().map(|entry| entry.text.len()).sum();
        self.errors = self
            .entries
            .iter()
            .filter(|entry| entry.level == log::Level::Error)
            .count();
        self.warnings = self
            .entries
            .iter()
            .filter(|entry| entry.level == log::Level::Warn)
            .count();
    }
}
static STORE: LazyLock<Mutex<Store>> = LazyLock::new(|| Mutex::new(Store::default()));
pub(crate) fn attach(context: &eframe::egui::Context) {
    STORE.lock().unwrap_or_else(|e| e.into_inner()).context = Some(context.clone());
}
pub(crate) fn push(entry: Entry, repaint: bool) {
    let context = {
        let mut store = STORE.lock().unwrap_or_else(|e| e.into_inner());
        store.push(entry);
        repaint.then(|| store.context.clone()).flatten()
    };
    if let Some(context) = context {
        context.request_repaint();
    }
}
/// Counts of retained application records, independent of viewer filters.
pub(crate) fn diagnostic_counts() -> (usize, usize) {
    let store = STORE.lock().unwrap_or_else(|e| e.into_inner());
    (store.errors, store.warnings)
}
pub(crate) fn snapshot() -> Vec<std::sync::Arc<Entry>> {
    STORE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .entries
        .iter()
        .cloned()
        .collect()
}
pub(crate) fn clear(source: Option<Source>) {
    STORE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clear(source);
}
pub(crate) fn log_path() -> io::Result<PathBuf> {
    Ok(crate::app_paths::data_root()?.join("logs/launch.log"))
}
pub(crate) fn fresh_file(path: &Path) -> io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("Invalid log path"))?;
    std::fs::create_dir_all(parent)?;
    let temporary = parent.join(format!(
        ".launch-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    let file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(&temporary)?;
    if let Err(error) = std::fs::rename(&temporary, path) {
        let _ = std::fs::remove_file(temporary);
        return Err(error);
    }
    Ok(file)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sources_clear_and_retention_are_independent() {
        let mut store = Store::default();
        for i in 0..10_005 {
            store.push(Entry {
                timestamp: String::new(),
                level: log::Level::Info,
                source: if i % 2 == 0 {
                    Source::Git
                } else {
                    Source::Addons
                },
                text: i.to_string(),
            });
        }
        assert_eq!(store.entries.len(), 10_000);
        assert_eq!(store.entries[0].text, "5");
        store.clear(Some(Source::Git));
        assert!(store.entries.iter().all(|e| e.source == Source::Addons));
        store.clear(None);
        assert_eq!(store.bytes, 0);
        assert_eq!(
            Source::from_target("focus_editor::dev_container::compose"),
            Source::DockerCompose
        );
        assert_eq!(
            Source::from_target("focus_editor::addons::example"),
            Source::Addons
        );
        assert_eq!(
            Source::from_target("focus_editor::addons::another"),
            Source::Addons
        );
        store.push(Entry {
            timestamp: String::new(),
            level: log::Level::Info,
            source: Source::Editor,
            text: "x".repeat(8 * 1024 * 1024 + 1),
        });
        assert!(store.entries.is_empty());
    }
    #[test]
    fn diagnostic_counts_follow_retention_and_source_clear() {
        let mut store = Store::default();
        for (level, source) in [
            (log::Level::Error, Source::Git),
            (log::Level::Warn, Source::Editor),
            (log::Level::Info, Source::Editor),
        ] {
            store.push(Entry {
                timestamp: String::new(),
                level,
                source,
                text: "record".into(),
            });
        }
        assert_eq!((store.errors, store.warnings), (1, 1));
        for _ in 0..9_998 {
            store.push(Entry {
                timestamp: String::new(),
                level: log::Level::Info,
                source: Source::Editor,
                text: String::new(),
            });
        }
        assert_eq!((store.errors, store.warnings), (0, 1));
        store.clear(Some(Source::Git));
        assert_eq!((store.errors, store.warnings), (0, 1));
        store.clear(Some(Source::Editor));
        assert_eq!((store.errors, store.warnings), (0, 0));
        store.push(Entry {
            timestamp: String::new(),
            level: log::Level::Error,
            source: Source::Git,
            text: "x".repeat(8 * 1024 * 1024 + 1),
        });
        assert_eq!((store.errors, store.warnings), (0, 0));
    }

    #[test]
    fn new_launch_replaces_file_without_interleaving_older_instance() {
        use std::io::Write;
        let root = std::env::temp_dir().join(format!("focus-log-file-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("launch.log");
        std::fs::write(&path, "old history").unwrap();
        let mut first = fresh_file(&path).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"");
        first.write_all(b"first").unwrap();
        let mut second = fresh_file(&path).unwrap();
        first.write_all(b"late old output").unwrap();
        second.write_all(b"second").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"second");
        std::fs::remove_dir_all(root).unwrap();
    }
}
