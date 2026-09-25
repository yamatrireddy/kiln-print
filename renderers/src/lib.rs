//! Document renderers.
//!
//! | Document | Renderer                    | Output                                        |
//! |----------|-----------------------------|-----------------------------------------------|
//! | RAW      | [`raw::RawRenderer`]         | the client's bytes, untouched                 |
//! | TEXT     | [`text::TextRenderer`]       | RAW bytes (encoded) or a native text layout   |
//! | PDF      | [`pdf::PdfRenderer`]         | validated PDF; the provider rasterises/passes |
//! | IMAGE    | [`image::ImageRenderer`]     | decoded RGB raster with rotation applied      |
//! | HTML     | [`html::HtmlRenderer`]       | PDF produced by a sandboxed headless browser  |
//! | LABEL    | [`label::LabelRenderer`]     | ZPL / EPL / TSPL / CPCL for the printer's language |
//! | RECEIPT  | [`receipt::ReceiptRenderer`] | ESC/POS bytes                                 |
//! | DOT_MATRIX | [`dotmatrix::DotMatrixRenderer`] | ESC/P bytes                           |

#![forbid(unsafe_code)]

use std::sync::Arc;

use kiln_core::renderer::DocumentRenderer;

/// Strict text encoders (moved to `kiln-protocols`; re-exported for compatibility).
pub use kiln_protocols::encoding;
pub mod dotmatrix;
pub mod html;
pub mod image;
pub mod label;
pub mod pdf;
pub mod raw;
pub mod receipt;
pub mod text;

/// Renderers that need no configuration. The HTML renderer is added separately because it
/// depends on a browser installation and administrator settings.
pub fn builtin() -> Vec<Arc<dyn DocumentRenderer>> {
    vec![
        Arc::new(raw::RawRenderer),
        Arc::new(text::TextRenderer),
        Arc::new(pdf::PdfRenderer),
        Arc::new(image::ImageRenderer::default()),
        Arc::new(label::LabelRenderer::default()),
        Arc::new(receipt::ReceiptRenderer),
        Arc::new(dotmatrix::DotMatrixRenderer),
    ]
}
