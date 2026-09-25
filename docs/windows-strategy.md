# Windows implementation strategy

Everything Windows-specific lives in `providers/windows` (`kiln-provider-windows`), behind `WindowsPrintProvider`. It uses Microsoft's `windows` crate (0.62). Every `unsafe` call carries a `SAFETY:` comment, and every other crate is `#![forbid(unsafe_code)]`.

## API map

| Capability | API | Module |
|---|---|---|
| Enumerate printers | `EnumPrintersW(PRINTER_ENUM_LOCAL \| PRINTER_ENUM_CONNECTIONS, level 2)` | `discovery.rs` |
| Default printer | `GetDefaultPrinterW` | `discovery.rs` |
| Status and conditions | `PRINTER_INFO_2W.Status` / `.Attributes` → `status::printer_state` | `status.rs` (pure, unit-tested) |
| Connection type | port/driver/attribute heuristics → `LOCAL/USB/SERIAL/NETWORK/VIRTUAL` | `status.rs` |
| Driver model | `GetPrinterDriverW(level 8).cVersion` (3 = v3, 4 = v4) | `discovery.rs` (cached per printer and driver) |
| Accepted datatypes | `EnumPrintProcessorDatatypesW` | `discovery.rs` |
| Capabilities | `DeviceCapabilitiesW` (papers, sizes, bins, resolutions, color, duplex, copies, collate, orientation) | `capabilities.rs` |
| RAW printing | `OpenPrinterW` → `StartDocPrinterW(level 1, "RAW")` → per copy `StartPagePrinter`/`WritePrinter` (64 KiB chunks)/`EndPagePrinter` → `EndDocPrinter`; `AbortPrinter` on any failure | `raw.rs` |
| Text printing | GDI: `DocumentPropertiesW` (orientation) → `CreateDCW("WINSPOOL")` → `StartDocW` → `TextOutW` → `EndDoc`; `AbortDoc` on failure | `gdi.rs` + `layout.rs` |
| PDF printing | `Windows.Data.Pdf` (`PdfDocument`, `RenderWithOptionsToStreamAsync`) → raster → GDI | `pdf.rs`, `raster.rs` |
| Image printing | decoded RGB → GDI `StretchDIBits` in ≤ 8 MB bands, `HALFTONE`, clip to area | `raster.rs` |
| Page setup | `DocumentPropertiesW` merge of paper (`dmPaperSize`/custom), orientation, duplex, colour, tray, validated against `DeviceCapabilitiesW` | `devmode.rs` |
| Default paper | `CreateICW` + `PHYSICALWIDTH/HEIGHT` | `capabilities.rs` |
| Job status | `GetJobW(level 1)`; `ERROR_INVALID_PARAMETER` = the job is no longer in the queue | `jobs.rs` |
| Completion tracking | `FindFirstPrinterChangeNotification(PRINTER_CHANGE_JOB, JOB_NOTIFY_FIELD_STATUS)`, with per-printer watcher threads that exit after 2 minutes idle | `watch.rs`, `status::resolve_job_state` |
| Queue inspection | `EnumJobsW(level 1)` | `jobs.rs` |
| Cancellation | `SetJobW(JOB_CONTROL_DELETE)` | `jobs.rs` |

Spooler buffers are allocated 8-byte aligned (`ffi::Buffer`), because `PRINTER_INFO_2W` and `JOB_INFO_1W` contain pointers. Every size-query call goes through `ffi::query`, which retries if the required size grows between calls (for example, a job arrives mid-enumeration).

## RAW printing and the v3/v4 driver split

- **v3 drivers** (Zebra ZDesigner, Epson ESC/P, TSC, Generic / Text Only, most POS drivers) route the `RAW` datatype straight to the port monitor. The device receives exactly the bytes written. This is the main path for barcode, label, receipt and dot-matrix printing.
- **v4 drivers** (class drivers, "Microsoft Print to PDF", many new inbox drivers) use an XPS pipeline, and RAW data is not passed through reliably. The provider reports `capabilities.raw = false` for them. The engine then refuses RAW jobs up front with `UNSUPPORTED_OPERATION`, instead of letting them disappear silently. `TEXT` in `RENDERED` mode (GDI) still works on v4 printers.
- If `StartDocPrinterW` rejects the datatype anyway (`ERROR_INVALID_DATATYPE`), the error maps to `UNSUPPORTED_OPERATION` with an explanatory message.
- **Copies:** RAW data bypasses the driver, so `DEVMODE.dmCopies` does not apply. The provider writes the payload N times inside one spool job, one "page" per copy, which is byte-exact and atomic. Label languages also have their own quantity commands (`^PQ`, `P<n>`, `PRINT n`), which pass through untouched.

## Text printing (GDI)

- Fonts are chosen by point size (`lfHeight = -pt × dpiY / 72`). The default face is Courier New, because text documents are usually columnar.
- Margins are measured from the physical paper edge, then corrected by `PHYSICALOFFSETX/Y` and clamped to the printable area.
- Word wrapping and pagination (`layout.rs`) are pure functions tested without GDI. The measuring function is `GetTextExtentPoint32W` on the printer DC.
- Copies are rendered as repeated page sequences (collated), which does not depend on driver support.
- Orientation is merged through the driver with `DocumentPropertiesW(DM_IN_BUFFER | DM_OUT_BUFFER)`, so driver-private DEVMODE data stays consistent.

This path is verified against the real spooler by `providers/windows/tests/spooler.rs::gdi_text_job_renders_through_the_spooler`. It prints to "Microsoft Print to PDF" with output redirected to a temp file, and asserts a 4-page PDF (2 pages × 2 copies, landscape).

## Status reliability (document for integrators)

| Signal | Reliability |
|---|---|
| `PRINTER_STATUS_*` for network (TCP/IP) printers with SNMP enabled | Good: offline, paper out, jams and door open are usually reported |
| USB printers | Poor. Many report `READY` when unplugged, and a queued job shows `PRINTING` indefinitely. "Use printer offline" (`PRINTER_ATTRIBUTE_WORK_OFFLINE`) is reported. |
| WSD printers | Moderate |
| `JOB_STATUS_PRINTED` / `COMPLETE` | Means all data went to the port. Not physical output. |
| Job disappearance | Normal completion: Windows deletes printed jobs unless "Keep printed documents" is on |

The engine therefore treats `online` as best-effort. It never refuses to queue a job because a printer *says* it is offline, since the spooler holds the job until the printer returns.

## Why not a Windows Service

See [ADR 0002](adr/0002-user-session-agent.md). GDI printing from session 0 is unsupported, and per-user printer connections are invisible to service accounts. The agent runs per user at logon.

## PDF, images and HTML (Phase 2)

- **PDF:** see [ADR 0004](adr/0004-document-rendering.md).
  - Each selected page is rendered by `Windows.Data.Pdf` directly at its destination size in device pixels, capped at `dpi` (default 300), then drawn with `StretchDIBits`.
  - Orientation follows the first selected page unless set.
  - `FIT`/`SHRINK_TO_FIT` lay out inside the printable area. `ACTUAL_SIZE`/percent lay out on the physical page (true position), and explicit margins lay out inside the paper minus those margins.
  - Copies re-render pages (collated), independent of driver copy support.
- **Images:** decoded and rotated by the renderer, and placed by `kiln_core::model::place` with the device's real DPI and printable area.
- **HTML:** rendered to PDF by a sandboxed headless Edge/Chrome. Edge ships with Windows 10 and 11.
- **Why raster:** it works identically with v3, v4 and XPS drivers and with thermal label printers, where vector output is often poor. At 300 dpi text is indistinguishable on office printers.
- **Page setup** is validated against the driver. Unknown paper or trays fail with `INVALID_PAYLOAD` (listing what is available), and duplex or colour on printers without them fail with `UNSUPPORTED_OPERATION`. Nothing is spooled when this fails.

## Roadmap for the Windows provider

| Phase | Work |
|---|---|
| 3 | Direct TCP 9100 provider (separate from the spooler; explicitly configured printers only) |
| 3 | Optional XPS print path (`IXpsPrintJob`) for vector PDF output on v4 drivers |
| 3 | Direct TCP 9100 provider (separate from the spooler; explicitly configured printers only) |
| 6 | Serial provider (`serialport` crate) for COM-attached legacy printers; per-session port selection; MSI (WiX) installer with code signing; tray icon; autostart registration |

## Local verification on this machine (2026-09-25)

- Discovery: "Microsoft Print to PDF" (port `PORTPROMPT:`, v4) and "OneNote (Desktop)" (port `nul:`, v4) were both detected as `VIRTUAL` with `raw = false`, and `RAW, NT EMF 1.00x, TEXT, XPS2GDI` datatypes were listed.
- RAW to OneNote was correctly refused with `UNSUPPORTED_OPERATION`.
- A GDI text job to Microsoft Print to PDF produced a valid 4-page PDF through the spooler.
- Phase 2: a two-page PDF printed 2 copies (4 pages), a page range with A5 landscape page setup (MediaBox 595 × 420 pt verified), a 3 × 1.5 in image at actual size (auto-landscape), an unknown paper and a corrupt PDF rejected before spooling, and a job verified to have left the queue still reported `Printed` through change-notification history.
- Byte-exact RAW through the spooler needs a v3 printer. Run `tests/hardware/setup-windows-test-printer.ps1` (administrator) to create "Kiln Test Raw" (Generic / Text Only → local file port), then the ignored test `raw_job_reaches_the_port_byte_for_byte`.
