# MVP scope and phases

## Phase 1: foundation (this iteration)

| Item | Status | Where |
|---|---|---|
| Architecture decision document | ✅ | `docs/adr/0001…0003`, `docs/architecture.md` |
| Technology selection with trade-offs | ✅ | `docs/adr/0001-technology-stack.md` |
| Component diagram and communication flow | ✅ | `docs/architecture.md` |
| Security and trust model | ✅ Phase 1 controls implemented; Phase 4 design documented | `docs/security-model.md`, `agent/src/security/` |
| Printer abstraction interfaces | ✅ `PrintProvider`, `PrinterDiscoveryProvider`, `DocumentRenderer`, `PrinterProtocol`, `JobRepository`, `ClientAuthenticator` | `print-core/src/{provider,renderer,protocol,repository}.rs`, `agent/src/security/mod.rs` |
| Print-job lifecycle | ✅ status plus delivery stage plus completion evidence; restart reconciliation; idempotency | `docs/print-job-lifecycle.md`, `print-core/src/model/job.rs`, `print-core/src/engine/` |
| Repository structure | ✅ | see `README.md` |
| Windows implementation strategy | ✅ | `docs/windows-strategy.md` |
| Core interfaces and engine | ✅ per-printer ordered queues, byte budget, bounded blocking pool, watchdog, monitor with backoff, discovery events | `print-core/` |
| Windows printer discovery | ✅ status, conditions, type, default, driver model, datatypes, capabilities, queue | `providers/windows/` |
| Windows RAW printing | ✅ byte-exact, abort on failure, v4 detection | `providers/windows/src/raw.rs` |
| Text printing | ✅ RAW mode (strict encodings incl. IBM437) and RENDERED mode (GDI layout) | `renderers/src/text.rs`, `providers/windows/src/gdi.rs` |
| Job model | ✅ | `print-core/src/model/job.rs` |
| Basic WebSocket API | ✅ plus REST, handshake, auth, events | `agent/src/api/` |
| Persistence | ✅ SQLite (WAL) jobs and audit, migrations, retention | `agent/src/persistence.rs` |
| Structured logging | ✅ console plus rotating JSON files, no payloads | `agent/src/logging.rs` |
| Printer-language registry | ✅ ZPL, EPL, CPCL, TSPL, ESC/POS, ESC/P, RAW with read-only inspection (languages were pulled forward from Phase 3 because they are cheap and make RAW safer) | `protocols/` |
| Mock provider and CI tests | ✅ 117 automated tests | `providers/mock/`, `*/tests/` |

**Explicitly not in Phase 1:** TLS, pairing/consent UI, PDF/HTML/image, TypeScript SDK, dashboard, direct TCP/serial, Linux/macOS, installers.

## Phase 2: documents and SDK (this iteration)

| Item | Status | Where |
|---|---|---|
| PDF printing without a viewer: page range, scale, orientation, paper, duplex, colour, tray, copies | ✅ Windows `Windows.Data.Pdf` → GDI | `renderers/src/pdf.rs`, `providers/windows/src/{pdf,raster,devmode}.rs` |
| Image printing: PNG, JPEG, BMP, TIFF, GIF; original/fit/shrink/fill, scale, rotate, DPI, align, paper | ✅ | `renderers/src/image.rs`, `print-core/src/model/page.rs` |
| HTML/CSS printing: CSS, `@page` size/margins, page breaks, headers/footers, fonts, SVG barcodes; sandboxed, no JavaScript, no network | ✅ headless Edge/Chrome over DevTools | `renderers/src/html/` |
| Document sources: inline, path, URL (admin allow-lists) | ✅ disabled by default | `agent/src/sources.rs` |
| Printer capabilities (full) and default paper | ✅ | `providers/windows/src/capabilities.rs` |
| Job monitoring via spooler change notifications | ✅ printed-vs-deleted after the job leaves the queue | `providers/windows/src/watch.rs` |
| TypeScript SDK with reconnection, idempotent resend, timeouts, typed errors, event buffering | ✅ 11 tests against a real agent | `sdk/typescript/` |
| API: `print.pdf`, `print.image`, `print.html`, REST `/v1/print/{pdf,image,html}` | ✅ | `agent/src/api/` |
| Decision record | ✅ | `docs/adr/0004-document-rendering.md` |

## Phase 3: printer languages, dot-matrix, network RAW (this iteration)

| Item | Status | Where |
|---|---|---|
| Language-neutral `LABEL` documents encoded as ZPL, EPL, TSPL or CPCL (text, 6 barcode symbologies, QR, Data Matrix, boxes, RAW) with injection-safe escaping | ✅ golden-tested | `print-core/src/model/label.rs`, `protocols/src/label/` |
| ESC/POS `RECEIPT` documents: styles, columns, barcodes, QR, dithered logos, cut, drawer, code pages | ✅ | `renderers/src/receipt.rs`, `protocols/src/escpos_commands.rs` |
| Dot-matrix `DOT_MATRIX` documents: CPI 10/12/15/17/20, LPI (native or n/180, n/216), draft/NLQ, form length, perforation skip, margins, bold/condensed/double-width/underline/italic/double-strike, character tables, raw escapes, multi-copy | ✅ never rasterised | `renderers/src/dotmatrix.rs`, `protocols/src/escp_commands.rs` |
| Code pages IBM850/IBM858 (plus IBM437, WHATWG) | ✅ | `protocols/src/encoding.rs` |
| Printer language hints (Windows driver heuristics, TCP config) | ✅ | `providers/windows/src/status.rs` |
| Direct RAW TCP (9100) provider: admin-configured only, byte-exact, `BYTES_DELIVERED`, ZPL/ESC-POS device status | ✅ | `providers/tcp/` |
| API `print.label` / `print.receipt` / `print.dotmatrix`, REST, SDK helpers | ✅ | `agent/src/api/`, `sdk/typescript/` |
| Decision record | ✅ | `docs/adr/0005-label-receipt-dot-matrix-and-direct-tcp.md` |

## Later phases

| Phase | Scope | Notes and first steps |
|---|---|---|
| 2 | ✅ Done (see above) | |
| 3 | ✅ Done (see above) | |
| 4 | TLS (`wss://localhost`), pairing with Allow once / Always / Deny, signed challenges, trusted clients in SQLite, revocation | Same `ClientAuthenticator` interface |
| 5 | Dashboard (served by the agent, admin session), connected clients, queues, history with filters (application, printer, status, date), cancel | APIs already exist: `clients.list`, `queue.*`, `jobs.list` filters, `GET /v1/audit` |
| 6 | `LinuxPrintProvider` (CUPS/IPP), `MacPrintProvider`, serial provider, installers (MSI/pkg/deb/rpm), autostart, tray, per-session ports | see `providers/linux/README.md`, `providers/macos/README.md` |
