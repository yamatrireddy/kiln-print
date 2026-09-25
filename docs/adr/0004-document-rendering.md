# ADR 0004 — Document rendering: OS PDF engine, sandboxed browser for HTML

- **Status:** Accepted. This supersedes the PDF/HTML notes in ADR 0001.
- **Date:** 2026-09-25

## Context

Phase 2 adds PDF, image and HTML printing. ADR 0001 planned to render PDF with PDFium (`pdfium-render`) and HTML with a headless Chromium. Requirements:

- PDF printing must not open an external viewer.
- HTML/CSS must be rendered by a controlled engine, and arbitrary remote HTML must not run JavaScript freely inside the agent.
- Page setup (paper, orientation, margins, duplex, colour, tray) and page ranges must be honoured.
- The agent must stay a small, self-contained install.

## Decision

### PDF

1. **The renderer validates; the provider rasterises.** `PdfRenderer` checks the signature and options and passes the bytes through (`PrintPayload::Pdf`). How a PDF becomes paper is platform knowledge:
   - **Windows:** the built-in **`Windows.Data.Pdf`** engine (Windows 10 and later) renders each selected page straight into memory, at the size it will occupy on paper, capped at `dpi` (default 300). The result goes through the GDI raster path (`StretchDIBits` in ≤8 MB bands) with a driver-validated `DEVMODE`.
   - **Linux/macOS (Phase 6):** CUPS accepts `application/pdf` natively, so the IPP provider sends the PDF with page-range and media attributes, and nothing is rasterised in the agent.
2. **No PDFium on Windows.** The OS engine is maintained and security-patched by Microsoft through Windows Update, adds 0 MB to the install, and needs no third-party binary.

   Trade-offs accepted:
   - The output is raster rather than vector. At 300 dpi text quality is indistinguishable on office printers, and spool files are larger.
   - Windows 10 is the minimum version.
   - Rendering fidelity is Microsoft's (the Edge PDF lineage), not Acrobat's.

   If vector output becomes a requirement (very large engineering drawings, for example), a PDFium- or XPS-based provider path can be added behind the same `PdfPayload` without changing clients.

### Images

The `image` crate (pure Rust) decodes PNG, JPEG, BMP, TIFF (first page) and GIF (first frame):
- pixel and allocation limits are enforced before decoding (decompression-bomb defence);
- rotation is applied;
- transparency is composited onto white;
- physical resolution is read from PNG `pHYs`, JPEG JFIF and BMP headers.

Placement (`ORIGINAL`, `FIT`, `SHRINK_TO_FIT`, `FILL`, percent, `CENTER`/`TOP_LEFT`) is a pure, unit-tested function in `kiln-core`, and the provider applies it with the device's real metrics.

### HTML

The system's **Microsoft Edge or Google Chrome/Chromium** runs headless and is driven over the DevTools protocol. The browser's own `Page.printToPDF` produces a PDF, which then follows the PDF path. Each render works like this:

- a fresh process with a throw-away profile, killed on completion, failure or timeout (default 30 s);
- the document is injected with `Page.setDocumentContent` into `about:blank`, whose origin cannot read `file:` URLs;
- **all network access is blocked** in two independent ways:
  - a dead proxy for every scheme, loopback included (`--proxy-server=127.0.0.1:9 --proxy-bypass-list=<-loopback>`);
  - `Fetch` interception that fails every request;
  - only inline `data:` resources load;
- **JavaScript is disabled** (`Emulation.setScriptExecutionDisabled`) unless an administrator sets `html.javascript = true`. Network isolation applies even then;
- barcodes and QR codes are therefore supplied as inline SVG or `data:` images, and web fonts are embedded as `data:` URIs;
- headers and footers use the DevTools templates (`pageNumber`, `totalPages`, `date`, `title`).

A test serves a local TCP port and renders HTML that points `<link>`, `@font-face`, `<img>`, `<iframe>` and `fetch()` at it, and it asserts zero connections. A control run without the sandbox flags does connect, which proves the test can fail.

We reuse the installed browser because Edge ships with every Windows 10/11 machine, so there are no bundled 150 MB Chromium builds to patch. Edge updates itself. The cost is the dependency itself: machines without Edge, Chrome or Chromium report `HTML` as unavailable (`UNSUPPORTED_DOCUMENT`), and every other document type still works.

## Consequences

- `PrintPayload` gained `Pdf` and `Image`. Providers declare support per printer, and the engine refuses a document type up front (`UNSUPPORTED_DOCUMENT`) when no payload kind the renderer can produce is accepted. This also avoids launching a browser for a label printer.
- `PageSetup` is shared by PDF, image and HTML. The Windows provider validates paper, tray, duplex and colour against `DeviceCapabilitiesW` and rejects unsupported values instead of silently substituting.
- The DevTools port is bound to `127.0.0.1` for the few seconds a render lasts. A local process could attach to that isolated, profile-less, network-less browser during the render. We accept this, because local processes already have the user's rights (see the security model). `--remote-debugging-pipe` would close the gap, and it is tracked for Phase 6 hardening.
