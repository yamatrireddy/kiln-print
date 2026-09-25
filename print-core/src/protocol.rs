//! Printer command-language extension point (ZPL, EPL, ESC/POS, …).
//!
//! A [`PrinterProtocol`] only ever receives `&[u8]`: it can inspect RAW data but has no way
//! to alter it, which is how the byte-for-byte guarantee is enforced by the type system.
//! Command *builders* (Phase 3) produce new documents; they never rewrite client bytes.

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum LanguageFamily {
    Label,
    Receipt,
    DotMatrix,
    Generic,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LanguageInfo {
    /// Canonical id used on the wire, e.g. `ZPL`, `ESC/POS`.
    pub id: &'static str,
    pub name: &'static str,
    pub family: LanguageFamily,
    /// Alternative spellings accepted from clients (case-insensitive).
    pub aliases: &'static [&'static str],
}

/// Result of a read-only sanity check.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Inspection {
    pub warnings: Vec<String>,
}

impl Inspection {
    pub fn warn(&mut self, message: impl Into<String>) {
        self.warnings.push(message.into());
    }
}

pub trait PrinterProtocol: Send + Sync {
    fn info(&self) -> &LanguageInfo;

    /// Heuristic checks that catch common mistakes (wrong language, truncated payload).
    /// Must be cheap and must never reject valid data outright; the engine decides
    /// whether warnings are fatal (`strict_languages`).
    fn inspect(&self, _data: &[u8]) -> Inspection {
        Inspection::default()
    }
}

#[derive(Default, Clone)]
pub struct ProtocolRegistry {
    by_key: HashMap<String, Arc<dyn PrinterProtocol>>,
    ordered: Vec<Arc<dyn PrinterProtocol>>,
}

impl ProtocolRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, protocol: Arc<dyn PrinterProtocol>) {
        let info = protocol.info();
        for key in std::iter::once(info.id).chain(info.aliases.iter().copied()) {
            self.by_key.insert(normalise(key), protocol.clone());
        }
        self.ordered.retain(|p| p.info().id != info.id);
        self.ordered.push(protocol);
    }

    pub fn resolve(&self, name: &str) -> Option<&Arc<dyn PrinterProtocol>> {
        self.by_key.get(&normalise(name))
    }

    pub fn languages(&self) -> Vec<LanguageInfo> {
        self.ordered.iter().map(|p| p.info().clone()).collect()
    }
}

impl fmt::Debug for ProtocolRegistry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list()
            .entries(self.ordered.iter().map(|p| p.info().id))
            .finish()
    }
}

/// `esc/pos`, `ESC_POS` and `escpos` all resolve to the same language.
fn normalise(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_uppercase())
        .collect()
}
