# ADR 0005 — Label, receipt and dot-matrix documents; direct TCP printing

- **Status:** Accepted
- **Date:** 2026-09-26

## Context

Phase 3 covers barcode/label printers (ZPL, EPL, TSPL, CPCL), receipt printers (ESC/POS), dot-matrix printers (ESC/P) and network RAW printing. Before Phase 3, applications could send these printers RAW bytes only, which means every application had to know every printer's command language. The requirements add:

- Dot-matrix output must stay text plus escape sequences (never PDF).
- Direct TCP printing must be separate from the spooler and explicitly configured.
- Additional languages must be addable without changing the engine.

## Decision

### 1. Three structured document types, encoded agent-side to RAW

| Type | Model | Encoded as |
|---|---|---|
| `LABEL` | size, dpi, darkness/speed/gap, and elements: `TEXT`, `BARCODE` (CODE128, CODE39, EAN13, EAN8, UPC_A, ITF), `QR`, `DATA_MATRIX`, `BOX`, `RAW` | ZPL, EPL, TSPL or CPCL, chosen by the document's `language` or the printer's language hint |
| `RECEIPT` | width, code page, cut, drawer, and items: `TEXT` (align, bold, underline, double size, invert, small), `COLUMNS`, `SEPARATOR`, `FEED`, `BARCODE`, `QR`, `IMAGE` (dithered logo), `CUT`, `DRAWER`, `RAW` | ESC/POS |
| `DOT_MATRIX` | cpi (10/12/15/17/20), lpi, pins, draft/NLQ, form length (lines or inches), perforation skip, margins, encoding/character table, and lines: text or `LINE` with bold/condensed/double-width/underline/italic/double-strike, `LINE_FEED`, `FORM_FEED`, `RAW` | ESC/P |

- **Output is always a RAW payload.** It goes through the normal RAW path (spooler `RAW` datatype or direct TCP), so it is byte-exact from encoder to device. Nothing is rasterised.
- **RAW documents remain available.** Applications that already generate ZPL or ESC/POS keep sending bytes, and the structured types are an optional convenience.
- **Label encoding is an extension point.** `PrinterProtocol::encode_label` returns `None` for languages without label support. A new label language (DPL, IPL, SBPL, …) implements it and registers with the engine; neither the engine nor the renderer changes.
- **Command builders are small functions returning exact bytes** (`escpos_commands`, `escp_commands`). Documents are assembled from them, and golden tests pin every sequence.
- **Injection safety.** Client text is escaped per language, so it can never terminate a field or start a command:
  - ZPL `^FH` hex escapes for `^`, `~` and `_`, and `>0` inside Code 128;
  - EPL `\"` and `\\`;
  - TSPL `\["]`;
  - control characters flattened in line-oriented languages.

  A text of `^XZ~JR` prints literally. `RAW` elements are the explicit escape hatch.
- **Validation happens before printing:** barcode data per symbology (EAN lengths and digits, Code 39 character set, ITF even length), coordinates inside the label, sizes and ranges. Text that the chosen code page cannot represent is an error, never a `?`.
- **Font sizing.** Resident bitmap fonts plus integer magnification are chosen to match the requested height at the label's dpi. On ties, the larger font with less magnification wins.
- **Check digits.** EAN/UPC data is sent without its check digit and the printer computes it. A wrong supplied digit is never printed.

### 2. Printer language hints

`Printer.language` is a best-effort hint:
- the Windows provider derives it from driver and queue names (ZDesigner → ZPL, "(EPL)" → EPL, TSC → TSPL, Epson TM/"Receipt" → ESC/POS, Epson LQ/LX/FX/DFX → ESC/P);
- direct TCP printers get it from configuration.

The document's own `language` always wins. When neither exists, the request fails with `UNSUPPORTED_DOCUMENT`, listing the valid languages, rather than guessing.

### 3. Direct RAW TCP provider (`kiln-provider-tcp`)

- **Only configured printers exist.** `[[network_printers]]` in `agent.toml` lists name, host, port (default 9100), language, optional status protocol and timeouts. No API accepts an address. A client can print only to printers an administrator has named, and client printer scopes apply as usual.
- **Delivery semantics** follow ADR 0003:
  - connection failure → `PRINTER_OFFLINE`, outcome `NOT_PRINTED`, recoverable;
  - failure while writing → outcome `UNKNOWN`, not recoverable, never retried;
  - success means every byte (× copies) was written, the connection was half-closed, and the printer had up to 2 s to drain it → `COMPLETED` with `BYTES_DELIVERED`.
- **Device status**, when configured:
  - `ZPL` sends `~HS` (paper out, paused, head open, ribbon out, temperature/RAM errors);
  - `ESC/POS` sends `DLE EOT 1/2/4` (offline, cover open, paper end or near end, error).

  Results are cached for 10 s and never queried while a job is being written, because many printers accept one connection at a time. This gives the dashboard device truth that the Windows spooler usually cannot provide for USB printers.
- **Ordering and concurrency** come from the engine's per-printer FIFO, so one connection is used at a time per printer.

## Consequences

- There are three new document types and methods (`print.label`, `print.receipt`, `print.dotmatrix`), with SDK helpers.
- Label encoders for EPL and CPCL cover the widely supported subset. Data Matrix is refused on EPL/CPCL, and 180°/270° barcodes are refused on CPCL. Byte sequences are pinned by golden tests, but physical verification on each printer family is still pending (see `tests/hardware/README.md`).
- Images on labels (ZPL `^GF`, TSPL `BITMAP`) are not in this phase; receipts do support dithered logos.
- Network printers are trusted by configuration. Pointing an entry at a non-printer service (even on loopback) lets authorised clients send bytes to it, which is why the list is admin-only and never client-controlled.
