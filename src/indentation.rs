//! Indentation policy and text transformations. All offsets are character offsets.
use eframe::egui::text::{CCursor, CCursorRange};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::Path};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Config {
    pub tab_size: usize,
    pub indent_size: usize,
    pub insert_spaces: bool,
    pub auto_indent: bool,
    pub smart_backspace: bool,
    pub smart_tabs: bool,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            tab_size: 4,
            indent_size: 4,
            insert_spaces: true,
            auto_indent: true,
            smart_backspace: true,
            smart_tabs: false,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct Overrides {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tab_size: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub indent_size: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub insert_spaces: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_indent: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub smart_backspace: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub smart_tabs: Option<bool>,
}
impl Overrides {
    pub fn record_sources<'a>(
        &self,
        source: &'a str,
        sources: &mut BTreeMap<&'static str, &'a str>,
    ) {
        for (name, present) in [
            ("Tab width", self.tab_size.is_some()),
            ("Indent width", self.indent_size.is_some()),
            ("Insert spaces", self.insert_spaces.is_some()),
            ("Auto indent", self.auto_indent.is_some()),
            ("Smart Backspace", self.smart_backspace.is_some()),
            ("Smart tabs", self.smart_tabs.is_some()),
        ] {
            if present {
                sources.insert(name, source);
            } else {
                sources.entry(name).or_insert("Built-in default");
            }
        }
    }
    pub fn sanitize(&mut self) {
        for value in [&mut self.tab_size, &mut self.indent_size] {
            if value.is_some_and(|v| !(1..=256).contains(&v)) {
                log::warn!("Ignoring indentation width outside 1..=256");
                *value = None;
            }
        }
    }
    pub fn apply(&self, config: &mut Config) {
        for (value, target) in [
            (self.tab_size, &mut config.tab_size),
            (self.indent_size, &mut config.indent_size),
        ] {
            if let Some(value) = value {
                if (1..=256).contains(&value) {
                    *target = value;
                }
            }
        }
        if let Some(v) = self.insert_spaces {
            config.insert_spaces = v;
        }
        if let Some(v) = self.auto_indent {
            config.auto_indent = v;
        }
        if let Some(v) = self.smart_backspace {
            config.smart_backspace = v;
        }
        if let Some(v) = self.smart_tabs {
            config.smart_tabs = v;
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct Preferences {
    pub defaults: Overrides,
    pub file_types: BTreeMap<String, Overrides>,
}
impl Preferences {
    pub fn sanitize(&mut self) {
        self.defaults.sanitize();
        for values in self.file_types.values_mut() {
            values.sanitize();
        }
    }
    pub fn apply(&self, kind: &str, config: &mut Config) {
        self.defaults.apply(config);
        if let Some(values) = self.file_types.get(kind) {
            values.apply(config);
        }
    }
}

pub(crate) fn file_type(path: &Path) -> &'static str {
    if matches!(
        path.file_name().and_then(|s| s.to_str()),
        Some("Makefile" | "makefile" | "GNUmakefile")
    ) {
        return "makefile";
    }
    let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("");
    match ext {
        "mk" => "makefile",
        "ts" | "tsx" => "typescript",
        "js" | "jsx" => "javascript",
        "yaml" | "yml" => "yaml",
        _ => crate::syntax::language(ext).1,
    }
}

pub(crate) fn resolve(
    kind: &str,
    global: &Preferences,
    project: &Overrides,
    workspace: &Preferences,
) -> Config {
    let mut config = Config::default();
    if kind == "makefile" {
        config.insert_spaces = false;
    }
    global.apply(kind, &mut config);
    project.apply(&mut config);
    workspace.apply(kind, &mut config);
    config
}

pub(crate) fn byte(text: &str, index: usize) -> usize {
    text.char_indices()
        .nth(index)
        .map_or(text.len(), |(i, _)| i)
}
pub(crate) fn columns(text: &str, tab: usize) -> usize {
    text.chars().fold(0, |col, ch| {
        if ch == '\t' {
            col + tab - col % tab
        } else {
            col + 1
        }
    })
}
fn whitespace(width: usize, config: Config) -> String {
    if config.insert_spaces {
        " ".repeat(width)
    } else {
        format!(
            "{}{}",
            "\t".repeat(width / config.tab_size),
            " ".repeat(width % config.tab_size)
        )
    }
}
fn leading(text: &str) -> &str {
    &text[..text.len() - text.trim_start_matches([' ', '\t']).len()]
}

#[derive(Clone, Copy)]
pub(crate) enum Action {
    Tab { backwards: bool },
    Enter,
    Backspace,
    Convert { spaces: bool },
}

struct Replacement {
    start: usize,
    end: usize,
    text: String,
}
fn replace(
    text: &mut String,
    selection: &mut CCursorRange,
    edits: Vec<Replacement>,
    conversion_tab: Option<usize>,
) {
    if edits.is_empty() {
        return;
    }
    let byte_indices: Vec<usize> = text
        .char_indices()
        .map(|(i, _)| i)
        .chain(std::iter::once(text.len()))
        .collect();
    for edit in edits.iter().rev() {
        let added = edit.text.chars().count();
        for cursor in [&mut selection.primary, &mut selection.secondary] {
            let i = cursor.index.0;
            cursor.index.0 = if i < edit.start {
                i
            } else if i > edit.end {
                i - (edit.end - edit.start) + added
            } else if let Some(tab) = conversion_tab {
                let original = &text[byte_indices[edit.start]..byte_indices[i]];
                let desired = columns(original, tab);
                let mut width = 0;
                let mut count = 0;
                for ch in edit.text.chars() {
                    let next = if ch == '\t' {
                        width + tab - width % tab
                    } else {
                        width + 1
                    };
                    if next > desired {
                        break;
                    }
                    width = next;
                    count += 1;
                }
                edit.start + count
            } else {
                edit.start + added.min(i - edit.start)
            };
        }
    }
    let mut output = String::with_capacity(text.len());
    let mut offset = 0;
    for edit in edits {
        output.push_str(&text[offset..byte_indices[edit.start]]);
        output.push_str(&edit.text);
        offset = byte_indices[edit.end];
    }
    output.push_str(&text[offset..]);
    *text = output;
}

pub(crate) fn edit(
    text: &mut String,
    selection: &mut CCursorRange,
    config: Config,
    kind: &str,
    action: Action,
    visual_column: Option<usize>,
) {
    selection.h_pos = None;
    let length = text.chars().count();
    selection.primary.index.0 = selection.primary.index.0.min(length);
    selection.secondary.index.0 = selection.secondary.index.0.min(length);
    let [min, max] = selection.sorted_cursors();
    let (start, end) = (min.index.0, max.index.0);
    let before = &text[..byte(text, start)];
    let line_start = before.rfind('\n').map_or(0, |i| i + 1);
    let current_line = &before[line_start..];
    let leading_only = current_line.chars().all(|c| c == ' ' || c == '\t');
    let mut edits = Vec::new();
    match action {
        Action::Convert { spaces } => {
            let mut offset = 0;
            for line in text.split_inclusive('\n') {
                let indent = leading(line);
                let width = columns(indent, config.tab_size);
                let converted = whitespace(
                    width,
                    Config {
                        insert_spaces: spaces,
                        ..config
                    },
                );
                if converted != indent {
                    edits.push(Replacement {
                        start: offset,
                        end: offset + indent.chars().count(),
                        text: converted,
                    });
                }
                offset += line.chars().count();
            }
        }
        Action::Tab { backwards } if backwards || start != end => {
            let mut offset = 0;
            for line in text.split_inclusive('\n') {
                let length = line.chars().count();
                if offset <= end && offset + length > start && (offset < end || start == end) {
                    let indent = leading(line);
                    let width = columns(indent, config.tab_size);
                    let target = if backwards {
                        width.saturating_sub(config.indent_size)
                    } else {
                        width + config.indent_size
                    };
                    let converted = whitespace(target, config);
                    if converted != indent {
                        edits.push(Replacement {
                            start: offset,
                            end: offset + indent.chars().count(),
                            text: converted,
                        });
                    }
                }
                offset += length;
            }
            // split_inclusive omits the final empty line.
            if start == end && before.ends_with('\n') && !backwards {
                edits.push(Replacement {
                    start,
                    end,
                    text: whitespace(config.indent_size, config),
                });
            }
        }
        Action::Tab { .. } => {
            let col = visual_column.unwrap_or_else(|| columns(current_line, config.tab_size));
            if !config.insert_spaces && config.smart_tabs && leading_only {
                let inserted = whitespace(col + config.tab_size - col % config.tab_size, config);
                let cursor = before[..line_start].chars().count() + inserted.chars().count();
                text.replace_range(line_start..byte(text, end), &inserted);
                *selection = CCursorRange::one(CCursor::new(cursor));
                return;
            }
            let inserted = if config.insert_spaces {
                " ".repeat(config.tab_size - col % config.tab_size)
            } else if config.smart_tabs && !leading_only {
                " ".repeat(config.tab_size - col % config.tab_size)
            } else {
                "\t".into()
            };
            let cursor = start + inserted.chars().count();
            text.replace_range(byte(text, start)..byte(text, end), &inserted);
            *selection = CCursorRange::one(CCursor::new(cursor));
            return;
        }
        Action::Enter => {
            let line_end = text[line_start..]
                .find('\n')
                .map_or(text.len(), |i| line_start + i);
            let indent = leading(&text[line_start..line_end]);
            let base = if config.auto_indent {
                columns(
                    &indent[..indent.len().min(current_line.len())],
                    config.tab_size,
                )
            } else {
                0
            };
            let code = lexical_code(before, kind);
            let last = code
                .rsplit('\n')
                .next()
                .unwrap_or("")
                .trim_end()
                .chars()
                .last();
            let opens = config.auto_indent
                && match kind {
                    "python" => last == Some(':'),
                    "rust" | "javascript" | "typescript" | "json" | "css" => {
                        matches!(last, Some('{' | '[' | '('))
                    }
                    _ => false,
                };
            let nesting = whitespace(base + if opens { config.indent_size } else { 0 }, config);
            let newline = if text.contains("\r\n") { "\r\n" } else { "\n" };
            let mut inserted = format!("{newline}{nesting}");
            let cursor = start + inserted.chars().count();
            let next = text[byte(text, end)..].chars().next();
            if opens
                && matches!(
                    (last, next),
                    (Some('{'), Some('}')) | (Some('['), Some(']')) | (Some('('), Some(')'))
                )
            {
                inserted.push_str(&format!("{newline}{}", whitespace(base, config)));
            }
            text.replace_range(byte(text, start)..byte(text, end), &inserted);
            *selection = CCursorRange::one(CCursor::new(cursor));
            return;
        }
        Action::Backspace => {
            if start != end {
                text.replace_range(byte(text, start)..byte(text, end), "");
                *selection = CCursorRange::one(CCursor::new(start));
            } else if start > 0 {
                if config.smart_backspace && leading_only {
                    let width = columns(current_line, config.tab_size);
                    let target = width.saturating_sub(1) / config.indent_size * config.indent_size;
                    let replacement = whitespace(target, config);
                    let first = before[..line_start].chars().count();
                    text.replace_range(line_start..byte(text, start), &replacement);
                    *selection =
                        CCursorRange::one(CCursor::new(first + replacement.chars().count()));
                } else {
                    text.replace_range(byte(text, start - 1)..byte(text, start), "");
                    *selection = CCursorRange::one(CCursor::new(start - 1));
                }
            }
            return;
        }
    }
    replace(
        text,
        selection,
        edits,
        matches!(action, Action::Convert { .. }).then_some(config.tab_size),
    );
}

/// Mask strings/comments before inspecting Enter triggers, including multiline context.
fn lexical_code(text: &str, kind: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::new();
    let (mut quote, mut triple, mut escaped, mut block, mut comment) =
        (None, false, false, 0usize, false);
    let mut raw_hashes = None;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        if comment {
            if c == '\n' {
                comment = false;
                out.push('\n');
            } else {
                out.push(' ');
            }
        } else if block > 0 {
            if c == '/' && next == Some('*') {
                block += 1;
                i += 1;
            } else if c == '*' && next == Some('/') {
                block -= 1;
                i += 1;
            }
            out.push(if c == '\n' { '\n' } else { ' ' });
        } else if let Some(q) = quote {
            if let Some(hashes) = raw_hashes {
                if c == '"' && (1..=hashes).all(|n| chars.get(i + n) == Some(&'#')) {
                    quote = None;
                    raw_hashes = None;
                    i += hashes;
                }
            } else if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == q
                && (!triple || chars.get(i + 1) == Some(&q) && chars.get(i + 2) == Some(&q))
            {
                quote = None;
                if triple {
                    i += 2;
                }
            }
            out.push(if c == '\n' { '\n' } else { ' ' });
        } else if kind == "python" && c == '#' || kind != "python" && c == '/' && next == Some('/')
        {
            comment = true;
            out.push(' ');
        } else if kind != "python" && c == '/' && next == Some('*') {
            block = 1;
            i += 1;
            out.push(' ');
        } else if kind == "rust" && c == 'r' && matches!(next, Some('#' | '"')) {
            let hashes = chars
                .iter()
                .skip(i + 1)
                .take_while(|&&ch| ch == '#')
                .count();
            if chars.get(i + 1 + hashes) == Some(&'"') {
                raw_hashes = Some(hashes);
                quote = Some('"');
                i += hashes + 1;
                out.push(' ');
            } else {
                out.push(c);
            }
        } else if c == '"' || c == '\'' || matches!(kind, "javascript" | "typescript") && c == '`' {
            // Rust lifetimes are not quoted strings.
            if kind == "rust"
                && c == '\''
                && next.is_some_and(|ch| ch.is_alphabetic())
                && chars.get(i + 2) != Some(&'\'')
            {
                out.push(c);
            } else {
                quote = Some(c);
                triple = kind == "python" && next == Some(c) && chars.get(i + 2) == Some(&c);
                if triple {
                    i += 2;
                }
                out.push(' ');
            }
        } else {
            out.push(c);
        }
        i += 1;
    }
    // An unterminated string/comment must never expose an earlier trigger.
    if quote.is_some() || block > 0 {
        out.push('x');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    fn run(
        input: &str,
        start: usize,
        end: usize,
        config: Config,
        kind: &str,
        action: Action,
    ) -> (String, CCursorRange) {
        let mut text = input.to_owned();
        let mut cursor = CCursorRange::two(CCursor::new(start), CCursor::new(end));
        edit(&mut text, &mut cursor, config, kind, action, None);
        (text, cursor)
    }
    #[test]
    fn workspace_fields_override_project_and_global_without_losing_false() {
        let global: Preferences = toml::from_str(
            "[defaults]\ntab_size=8\n[file_types.rust]\nindent_size=6\ninsert_spaces=true",
        )
        .unwrap();
        let workspace: Preferences =
            toml::from_str("[defaults]\ntab_size=6\n[file_types.rust]\ninsert_spaces=false")
                .unwrap();
        let project = Overrides {
            tab_size: Some(2),
            indent_size: Some(2),
            insert_spaces: Some(true),
            ..Default::default()
        };
        let config = resolve("rust", &global, &project, &workspace);
        assert_eq!(
            (config.tab_size, config.indent_size, config.insert_spaces),
            (6, 2, false)
        );
        let other = resolve("rust", &global, &project, &Preferences::default());
        assert_eq!(
            (other.tab_size, other.indent_size, other.insert_spaces),
            (2, 2, true)
        );
        let no_project = resolve(
            "rust",
            &global,
            &Overrides::default(),
            &Preferences::default(),
        );
        assert_eq!((no_project.tab_size, no_project.indent_size), (8, 6));
        assert!(
            !resolve(
                "makefile",
                &Preferences::default(),
                &Overrides::default(),
                &Preferences::default()
            )
            .insert_spaces
        );
        let invalid = Overrides {
            tab_size: Some(0),
            indent_size: Some(usize::MAX),
            ..Default::default()
        };
        assert_eq!(
            resolve(
                "plain",
                &Preferences::default(),
                &invalid,
                &Preferences::default()
            ),
            Config::default()
        );
    }
    #[test]
    fn tab_stops_and_literal_tabs_preserve_inline_content() {
        for width in [4, 6] {
            let config = Config {
                tab_size: width,
                ..Default::default()
            };
            for input in ["", "abc", "é\t文", "     "] {
                let n = input.chars().count();
                let (text, cursor) = run(
                    input,
                    n,
                    n,
                    config,
                    "plain",
                    Action::Tab { backwards: false },
                );
                let count = width - columns(input, width) % width;
                assert_eq!(text, format!("{input}{}", " ".repeat(count)));
                assert_eq!(cursor.primary.index.0, n + count);
            }
        }
        assert_eq!(
            run(
                "a",
                1,
                1,
                Config {
                    insert_spaces: false,
                    ..Default::default()
                },
                "plain",
                Action::Tab { backwards: false }
            )
            .0,
            "a\t"
        );
    }
    #[test]
    fn block_indent_keeps_selection_direction_and_excludes_last_line() {
        let input = "a\n  b\nc";
        let (text, cursor) = run(
            input,
            6,
            0,
            Config::default(),
            "plain",
            Action::Tab { backwards: false },
        );
        assert_eq!(text, "    a\n      b\nc");
        assert!(cursor.primary.index < cursor.secondary.index);
        assert_eq!(cursor.secondary.index.0, 14);
        let (out, _) = run(
            &text,
            0,
            14,
            Config::default(),
            "plain",
            Action::Tab { backwards: true },
        );
        assert_eq!(out, input);
        assert_eq!(
            run(
                "  a",
                2,
                2,
                Config::default(),
                "plain",
                Action::Tab { backwards: true }
            )
            .0,
            "a"
        );
    }
    #[test]
    fn enter_and_backspace_use_language_and_config() {
        assert_eq!(
            run(
                "    if x:",
                9,
                9,
                Config::default(),
                "python",
                Action::Enter
            )
            .0,
            "    if x:\n        "
        );
        let (text, cursor) = run("{}", 1, 1, Config::default(), "rust", Action::Enter);
        assert_eq!(text, "{\n    \n}");
        assert_eq!(cursor.primary.index.0, 6);
        for input in ["    // {", "    \"{\"", "    /* {", "    r#\"{\"#"] {
            let n = input.chars().count();
            assert_eq!(
                run(input, n, n, Config::default(), "rust", Action::Enter).0,
                format!("{input}\n    ")
            );
        }
        assert_eq!(
            run(
                "    a",
                5,
                5,
                Config {
                    auto_indent: false,
                    ..Default::default()
                },
                "plain",
                Action::Enter
            )
            .0,
            "    a\n"
        );
        assert_eq!(
            run(
                "      x",
                6,
                6,
                Config::default(),
                "plain",
                Action::Backspace
            )
            .0,
            "    x"
        );
        assert_eq!(
            run("aé", 2, 2, Config::default(), "plain", Action::Backspace).0,
            "a"
        );
    }
    #[test]
    fn smart_tabs_and_multiline_strings_keep_alignment_and_language_context() {
        let config = Config {
            insert_spaces: false,
            smart_tabs: true,
            ..Default::default()
        };
        assert_eq!(
            run(
                "   ",
                3,
                3,
                config,
                "plain",
                Action::Tab { backwards: false }
            )
            .0,
            "\t"
        );
        assert_eq!(
            run("x", 1, 1, config, "plain", Action::Tab { backwards: false }).0,
            "x   "
        );
        let rust = "let s = r##\"a \" {\"##;\nfn f() {";
        let n = rust.chars().count();
        assert_eq!(
            run(rust, n, n, Config::default(), "rust", Action::Enter).0,
            format!("{rust}\n    ")
        );
        let python = "if x: # comment";
        let n = python.chars().count();
        assert_eq!(
            run(python, n, n, Config::default(), "python", Action::Enter).0,
            format!("{python}\n    ")
        );
        let crlf = "a\r\n    b";
        let n = crlf.chars().count();
        assert_eq!(
            run(crlf, n, n, Config::default(), "plain", Action::Enter).0,
            format!("{crlf}\r\n    ")
        );
    }
    #[test]
    fn conversions_only_change_leading_whitespace_and_keep_line_endings() {
        let config = Config {
            tab_size: 6,
            ..Default::default()
        };
        let input = "\t a\tb\r\n   \tc\n";
        let (spaces, _) = run(
            input,
            0,
            0,
            config,
            "plain",
            Action::Convert { spaces: true },
        );
        assert_eq!(spaces, "       a\tb\r\n      c\n");
        let (tabs, _) = run(
            &spaces,
            0,
            0,
            config,
            "plain",
            Action::Convert { spaces: false },
        );
        assert_eq!(tabs, "\t a\tb\r\n\tc\n");
    }
}
