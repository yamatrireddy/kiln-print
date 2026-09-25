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

Select a printer with `KILN_PRINTER="<name or id>"`. The `--mock` flag adds simulated label, receipt, dot-matrix and direct-TCP printers.

PDF, HTML and image examples arrive with Phase 2.
