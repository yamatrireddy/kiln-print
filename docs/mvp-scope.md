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

## Later phases

| Phase | Scope | Notes and first steps |
|---|---|---|
| 2 | ✅ Done (see above) | |
| 3 | Label/receipt/dot-matrix **builders** (ZPL/EPL/TSPL/ESC/POS/ESC/P command builders, CPI/LPI/condensed/bold/form length), direct **TCP 9100** provider, opt-in device-status queries (`~HS`, `DLE EOT`) | Builders produce new documents; they never rewrite client bytes |
| 4 | TLS (`wss://localhost`), pairing with Allow once / Always / Deny, signed challenges, trusted clients in SQLite, revocation | Same `ClientAuthenticator` interface |
| 5 | Dashboard (served by the agent, admin session), connected clients, queues, history with filters (application, printer, status, date), cancel | APIs already exist: `clients.list`, `queue.*`, `jobs.list` filters, `GET /v1/audit` |
| 6 | `LinuxPrintProvider` (CUPS/IPP), `MacPrintProvider`, serial provider, installers (MSI/pkg/deb/rpm), autostart, tray, per-session ports | see `providers/linux/README.md`, `providers/macos/README.md` |
