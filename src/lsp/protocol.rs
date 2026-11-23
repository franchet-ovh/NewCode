//! Small, bounded LSP wire and position helpers. Offsets in the editor are characters.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::{self, BufRead, Read, Write};

pub(super) const MAX_MESSAGE: usize = 8 * 1024 * 1024;
pub(super) const MAX_SYMBOLS: usize = 20_000;

pub(super) fn read_message(reader: &mut impl BufRead) -> io::Result<Option<Value>> {
    let mut length = None;
    let mut total = 0;
    loop {
        let mut line = Vec::new();
        let read = reader.take(8193).read_until(b'\n', &mut line)?;
        if read == 0 && total == 0 {
            return Ok(None);
        }
        total += read;
        if read == 0 || total > 8192 || !line.ends_with(b"\r\n") {
            return Err(io::Error::other("Invalid LSP header"));
        }
        if line == b"\r\n" {
            break;
        }
        let line = std::str::from_utf8(&line).map_err(io::Error::other)?;
        let (key, value) = line
            .split_once(':')
            .ok_or_else(|| io::Error::other("Invalid LSP header"))?;
        if key.eq_ignore_ascii_case("Content-Length") {
            if length.is_some() {
                return Err(io::Error::other("Duplicate LSP length"));
            }
            length = Some(value.trim().parse::<usize>().map_err(io::Error::other)?);
        }
    }
    let length = length
        .filter(|n| *n <= MAX_MESSAGE)
        .ok_or_else(|| io::Error::other("Missing or oversized LSP length"))?;
    let mut bytes = vec![0; length];
    reader.read_exact(&mut bytes)?;
    let value: Value = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
    if value.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Err(io::Error::other("Invalid JSON-RPC version"));
    }
    Ok(Some(value))
}

pub(super) fn write_message(writer: &mut impl Write, value: &Value) -> io::Result<()> {
    let bytes = serde_json::to_vec(value)?;
    if bytes.len() > MAX_MESSAGE {
        return Err(io::Error::other("LSP message too large"));
    }
    write!(writer, "Content-Length: {}\r\n\r\n", bytes.len())?;
    writer.write_all(&bytes)?;
    writer.flush()
}

#[derive(Clone, Copy, Default, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct Position {
    pub line: u32,
    pub character: u32,
}
#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
pub(super) struct Range {
    pub start: Position,
    pub end: Position,
}

pub(super) fn position(text: &str, offset: usize) -> Position {
    let mut p = Position::default();
    for ch in text.chars().take(offset) {
        if ch == '\n' {
            p.line += 1;
            p.character = 0;
        } else {
            p.character += ch.len_utf16() as u32;
        }
    }
    p
}
struct Positions {
    units: Vec<u32>,
    lines: Vec<(usize, usize)>,
}
impl Positions {
    fn new(text: &str) -> Self {
        let mut units = vec![0];
        let mut count = 0u32;
        for ch in text.chars() {
            if ch.len_utf16() == 2 {
                units.push(u32::MAX);
            }
            count += 1;
            units.push(count);
        }
        let mut lines = Vec::new();
        let mut start = 0;
        for line in text.split_inclusive('\n') {
            lines.push((
                start,
                line.trim_end_matches(['\r', '\n']).encode_utf16().count(),
            ));
            start += line.encode_utf16().count();
        }
        if text.is_empty() || text.ends_with('\n') {
            lines.push((start, 0));
        }
        Self { units, lines }
    }
    fn offset(&self, p: Position) -> Option<usize> {
        let (start, len) = *self.lines.get(p.line as usize)?;
        if p.character as usize > len {
            return None;
        }
        let offset = *self.units.get(start + p.character as usize)?;
        (offset != u32::MAX).then_some(offset as usize)
    }
}
#[cfg(test)]
pub(super) fn offset(text: &str, p: Position) -> Option<usize> {
    Positions::new(text).offset(p)
}

#[derive(Clone, Debug)]
pub(crate) struct Symbol {
    pub name: String,
    pub kind: u32,
    pub start: usize,
    pub end: usize,
    pub target: usize,
    pub line: u32,
    pub parent: Option<usize>,
    pub enclosing: bool,
}

pub(super) fn symbols(value: &Value, text: &str, uri: &str) -> Result<Vec<Symbol>, String> {
    fn walk(
        value: &Value,
        positions: &Positions,
        uri: &str,
        parent: Option<usize>,
        depth: usize,
        out: &mut Vec<Symbol>,
    ) -> Result<(), String> {
        if depth > 64 {
            return Err("Symbol nesting exceeds 64 levels".into());
        }
        let Some(items) = value.as_array() else {
            return Ok(());
        };
        for item in items {
            if out.len() >= MAX_SYMBOLS {
                return Err("Symbol limit exceeded (20,000)".into());
            }
            let flat = item.get("location").is_some();
            if flat && item["location"]["uri"].as_str() != Some(uri) {
                continue;
            }
            let range = if flat {
                &item["location"]["range"]
            } else {
                &item["range"]
            };
            let Ok(range) = serde_json::from_value::<Range>(range.clone()) else {
                continue;
            };
            let selection =
                serde_json::from_value::<Range>(item["selectionRange"].clone()).unwrap_or(range);
            let (Some(start), Some(end), Some(target)) = (
                positions.offset(range.start),
                positions.offset(range.end),
                positions.offset(selection.start),
            ) else {
                continue;
            };
            let Some(name) = item["name"].as_str().filter(|name| !name.is_empty()) else {
                continue;
            };
            if start > end || target < start || target > end {
                continue;
            }
            let index = out.len();
            out.push(Symbol {
                name: name.chars().take(512).collect(),
                kind: item["kind"].as_u64().unwrap_or(0) as u32,
                start,
                end,
                target,
                line: selection.start.line + 1,
                parent,
                enclosing: !flat || range.end.line > range.start.line,
            });
            walk(
                &item["children"],
                positions,
                uri,
                Some(index),
                depth + 1,
                out,
            )?;
        }
        Ok(())
    }
    let mut result = Vec::new();
    walk(value, &Positions::new(text), uri, None, 0, &mut result)?;
    // Normalize flat and hierarchical replies into source order and infer only
    // scopes whose locations actually cover their children.
    result.sort_by_key(|s| (s.start, std::cmp::Reverse(s.end)));
    let mut stack: Vec<usize> = Vec::new();
    for index in 0..result.len() {
        while stack.last().is_some_and(|parent| {
            result[*parent].end < result[index].end || result[*parent].end <= result[index].start
        }) {
            stack.pop();
        }
        result[index].parent = stack.last().copied();
        if result[index].enclosing && stack.len() < 64 {
            stack.push(index);
        }
    }
    Ok(result)
}

/// One replacement spanning the changed middle, computed on Unicode boundaries.
pub(super) fn change(old: &str, new: &str) -> Value {
    let mut prefix = old
        .chars()
        .zip(new.chars())
        .take_while(|(a, b)| a == b)
        .count();
    fn splits_crlf(text: &str, index: usize) -> bool {
        index > 0
            && text.chars().nth(index - 1) == Some('\r')
            && text.chars().nth(index) == Some('\n')
    }
    if splits_crlf(old, prefix) || splits_crlf(new, prefix) {
        prefix -= 1;
    }
    let old_rest: String = old.chars().skip(prefix).collect();
    let new_rest: String = new.chars().skip(prefix).collect();
    let mut suffix = old_rest
        .chars()
        .rev()
        .zip(new_rest.chars().rev())
        .take_while(|(a, b)| a == b)
        .count();
    let old_len = old.chars().count();
    let new_len = new.chars().count();
    if splits_crlf(old, old_len - suffix) || splits_crlf(new, new_len - suffix) {
        suffix = suffix.saturating_sub(1);
    }
    let end = old_len - suffix;
    let replacement: String = new_rest
        .chars()
        .take(new_rest.chars().count() - suffix)
        .collect();
    serde_json::json!({"range": {"start": position(old, prefix), "end": position(old, end)}, "text": replacement})
}
