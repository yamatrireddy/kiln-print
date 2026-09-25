//! Document renderers.
//!
//! | Document | Renderer                    | Output                                        |
//! |----------|-----------------------------|-----------------------------------------------|
//! | RAW      | [`raw::RawRenderer`]         | the client's bytes, untouched                 |
//! | TEXT     | [`text::TextRenderer`]       | RAW bytes (encoded) or a native text layout   |
//! | PDF      | [`pdf::PdfRenderer`]         | validated PDF; the provider rasterises/passes |
//! | IMAGE    | [`image::ImageRenderer`]     | decoded RGB raster with rotation applied      |
//! | HTML     | [`html::HtmlRenderer`]       | PDF produced by a sandboxed headless browser  |

#![forbid(unsafe_code)]

use std::sync::Arc;

use kiln_core::renderer::DocumentRenderer;

pub mod encoding;
pub mod html;
pub mod image;
pub mod pdf;
pub mod raw;
pub mod text;

/// Renderers that need no configuration. The HTML renderer is added separately because it
/// depends on a browser installation and administrator settings.
pub fn builtin() -> Vec<Arc<dyn DocumentRenderer>> {
    vec![
        Arc::new(raw::RawRenderer),
        Arc::new(text::TextRenderer),
        Arc::new(pdf::PdfRenderer),
        Arc::new(image::ImageRenderer::default()),
    ]
}
