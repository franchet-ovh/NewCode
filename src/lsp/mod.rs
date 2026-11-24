//! Language-neutral editor integration; addons own every server process.
pub(crate) mod protocol;
mod provider;
#[cfg(test)]
mod tests;
pub(crate) use protocol::Symbol;
pub(crate) use provider::*;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

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
    pub providers: std::collections::BTreeMap<String, String>,
    pub servers: std::collections::BTreeMap<String, ServerConfig>,
    pub rust: ServerConfig,
    pub python: ServerConfig,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            enabled: true,
            providers: Default::default(),
            servers: Default::default(),
            rust: ServerConfig::default(),
            python: ServerConfig {
                command: "pylsp".into(),
                args: vec![],
            },
        }
    }
}
#[derive(Clone, Default)]
pub(crate) struct Output {
    pub symbols: Arc<Vec<Symbol>>,
    pub status: String,
    pub version: u64,
    pub ready: bool,
}

/// UI state for the existing breadcrumb picker; no process ownership.
#[derive(Default)]
pub(crate) struct Client {
    pub output: Output,
    pub wake: bool,
    pub restart: bool,
}
impl Client {
    pub fn activate(&mut self) {
        self.wake = true;
    }
    pub fn can_retry(&self) -> bool {
        true
    }
    pub fn retry(&mut self) {
        self.restart = true;
    }
    pub fn shutdown(&mut self) {}
}
