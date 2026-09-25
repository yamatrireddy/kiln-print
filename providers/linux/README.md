# LinuxPrintProvider (Phase 6)

Planned as the `kiln-provider-cups` crate, which will also serve macOS for RAW and PDF, because macOS uses CUPS.

| Capability | Approach |
|---|---|
| Discovery | IPP `CUPS-Get-Printers` to `localhost:631` (pure Rust IPP client), or libcups `cupsGetDests2` via FFI |
| Status | `printer-state`, `printer-state-reasons` (maps directly to `PrinterCondition`) |
| RAW | IPP `Print-Job` with `document-format=application/vnd.cups-raw`, which bypasses filters (byte-exact) |
| PDF / images | IPP `Print-Job` with `application/pdf`/`image/*`; CUPS filters rasterise for the driver |
| Text | `text/plain` via `texttopdf`, or RAW text mode |
| Job status / cancel | `Get-Job-Attributes` (`job-state` 3–9) / `Cancel-Job` |
| Capabilities | `Get-Printer-Attributes` (`media-supported`, `sides-supported`, `print-color-mode-supported`, `printer-resolution-supported`) |

No engine changes are needed: implement `PrintProvider` and register it in `kiln_agent::build_engine` under `cfg(target_os = "linux")`.
