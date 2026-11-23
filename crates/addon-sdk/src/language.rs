//! Optional language-provider extension, independent of the editor UI.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::{self, BufRead, Read, Write};

pub const VERSION: u32 = 1;
// Leave space for the enclosing addon notification and JSON escaping.
pub const MAX_FRAME: usize = 3 * 1024 * 1024;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Language {
    pub id: String,
    pub extensions: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Start {
    pub generation: u64,
    pub session: u64,
    pub language: String,
    pub context: crate::Context,
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Packet {
    pub generation: u64,
    pub session: u64,
    pub message: Value,
}

/// Bounded Content-Length framing on the server side of the addon bridge.
pub fn read(reader: &mut impl BufRead) -> io::Result<Option<Value>> {
    let mut length = None;
    let mut total = 0;
    loop {
        let mut line = Vec::new();
        let count = reader.take(8193).read_until(b'\n', &mut line)?;
        if count == 0 && total == 0 {
            return Ok(None);
        }
        total += count;
        if count == 0 || total > 8192 || !line.ends_with(b"\r\n") {
            return Err(io::Error::other("Invalid LSP header"));
        }
        if line == b"\r\n" {
            break;
        }
        let text = std::str::from_utf8(&line).map_err(io::Error::other)?;
        let (key, value) = text
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
        .filter(|n| *n <= MAX_FRAME)
        .ok_or_else(|| io::Error::other("Oversized or missing LSP length"))?;
    let mut bytes = vec![0; length];
    reader.read_exact(&mut bytes)?;
    let value: Value = serde_json::from_slice(&bytes)?;
    if value["jsonrpc"] != "2.0" {
        return Err(io::Error::other("Invalid LSP version"));
    }
    Ok(Some(value))
}

pub fn write(writer: &mut impl Write, value: &Value) -> io::Result<()> {
    let bytes = serde_json::to_vec(value)?;
    if bytes.len() > MAX_FRAME {
        return Err(io::Error::other("LSP message too large"));
    }
    write!(writer, "Content-Length: {}\r\n\r\n", bytes.len())?;
    writer.write_all(&bytes)?;
    writer.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn framing_is_bounded_and_round_trips_unicode() {
        let value = serde_json::json!({"jsonrpc":"2.0", "method":"hello", "params":"é🦀"});
        let mut wire = Vec::new();
        write(&mut wire, &value).unwrap();
        assert_eq!(read(&mut io::Cursor::new(wire)).unwrap(), Some(value));
        for wire in [
            "Content-Length: 999999999\r\n\r\n",
            "Content-Length: 1\r\nContent-Length: 1\r\n\r\nx",
            "Content-Length: 4\r\n\r\n{}",
        ] {
            assert!(read(&mut io::Cursor::new(wire)).is_err());
        }
    }
}
