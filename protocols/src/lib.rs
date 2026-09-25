//! Built-in printer command languages.
//!
//! Each language implements [`PrinterProtocol`]: a descriptor plus cheap, read-only
//! inspection that catches the most common integration mistakes (sending ZPL to an
//! ESC/POS printer, a label without its terminator, …). Inspection never modifies or
//! rejects data by itself — see `strict_languages` in the engine configuration.
//!
//! Adding a language means adding a type that implements [`PrinterProtocol`] and
//! registering it with the engine builder; the engine itself does not change.

#![forbid(unsafe_code)]

use std::sync::Arc;

use kiln_core::protocol::PrinterProtocol;

mod bytes;
pub mod cpcl;
pub mod epl;
pub mod escp;
pub mod escpos;
pub mod raw;
pub mod tspl;
pub mod zpl;

/// Every built-in language, in the order they are listed to clients.
pub fn builtin() -> Vec<Arc<dyn PrinterProtocol>> {
    vec![
        Arc::new(raw::Raw),
        Arc::new(zpl::Zpl),
        Arc::new(epl::Epl),
        Arc::new(cpcl::Cpcl),
        Arc::new(tspl::Tspl),
        Arc::new(escpos::EscPos),
        Arc::new(escp::EscP),
    ]
}

#[cfg(test)]
mod tests {
    use kiln_core::protocol::ProtocolRegistry;

    #[test]
    fn aliases_resolve_to_canonical_ids() {
        let mut registry = ProtocolRegistry::new();
        for p in super::builtin() {
            registry.register(p);
        }
        for (alias, id) in [
            ("zpl", "ZPL"),
            ("ZPL II", "ZPL"),
            ("epl2", "EPL"),
            ("esc_pos", "ESC/POS"),
            ("escpos", "ESC/POS"),
            ("ESC/P2", "ESC/P"),
            ("tspl2", "TSPL"),
            ("binary", "RAW"),
        ] {
            let resolved = registry.resolve(alias).map(|p| p.info().id);
            assert_eq!(resolved, Some(id), "alias {alias}");
        }
        assert!(registry.resolve("pcl6").is_none());
    }
}
