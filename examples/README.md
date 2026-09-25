# Examples

Dependency-free Node (22+) scripts that speak protocol v1 through [`lib/kiln-client.mjs`](lib/kiln-client.mjs).

```bash
cargo run -p kiln-agent -- run --mock
```

In a second terminal, export the admin token printed by `kiln-agent token` as `KILN_TOKEN`, then run:

| Script | Shows |
|---|---|
| `barcode/print-zpl-label.mjs` | ZPL label (Code 128 + QR), `utf8` payload, idempotency key, live job events |
| `dot-matrix/print-escp-form.mjs` | ESC/P bytes: 10 CPI, condensed, bold, 6 LPI, form length, form feed |
| `text/print-text.mjs` | TEXT in RENDERED (driver) or RAW (`KILN_TEXT_MODE=RAW`, IBM437) mode |
| `raw/print-file.mjs <file> <printer> [language]` | Any file, byte for byte |
| `pdf/print-pdf.mjs <file.pdf> <printer> [pageRange]` | Silent PDF printing with page setup (uses the SDK) |
| `html/print-html.mjs` | An HTML invoice with a CSS `@page`, SVG barcode, page break and page-number footer (uses the SDK) |
| `image/print-image.mjs <image> <printer> [fit]` | Image placement with margins (uses the SDK) |
| `barcode/print-label.mjs` | A language-neutral shipping label, encoded by the agent as ZPL/EPL/TSPL/CPCL for the printer (uses the SDK) |
| `receipt/print-receipt.mjs` | An ESC/POS receipt with columns, euro prices (IBM858), QR, cut and drawer (uses the SDK) |
| `dot-matrix/print-form.mjs` | The dot-matrix invoice as a DOT_MATRIX document: NLQ, form length, perforation skip, condensed and bold (uses the SDK) |

Select a printer with `KILN_PRINTER="<name or id>"`. The `--mock` flag adds simulated label, receipt, dot-matrix and direct-TCP printers.

The SDK-based examples need a one-time SDK build: `cd sdk/typescript && npm install && npm run build`.
