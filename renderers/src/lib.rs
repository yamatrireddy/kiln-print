//! Document renderers.
//!
//! | Document | Renderer            | Output                                   |
//! |----------|---------------------|------------------------------------------|
//! | RAW      | [`raw::RawRenderer`]  | the client's bytes, untouched            |
//! | TEXT     | [`text::TextRenderer`]| RAW bytes (encoded) or a native text layout |
//!
//! PDF, HTML and image renderers arrive in Phase 2 behind the same interface.

#![forbid(unsafe_code)]

use std::sync::Arc;

use kiln_core::renderer::DocumentRenderer;

pub mod encoding;
pub mod raw;
pub mod text;

pub fn builtin() -> Vec<Arc<dyn DocumentRenderer>> {
    vec![Arc::new(raw::RawRenderer), Arc::new(text::TextRenderer)]
}
