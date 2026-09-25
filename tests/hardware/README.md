# Hardware tests

These tests use real printers and real paper, so they are kept out of CI. CI covers the same logic with `kiln-provider-mock` (see `docs/testing.md`).

## 1. Spooler tests without a physical printer

```powershell
# Administrator PowerShell: creates "Kiln Test Raw" (Generic / Text Only on NUL:)
.\tests\hardware\setup-windows-test-printer.ps1
```

```powershell
$env:KILN_TEST_RAW_PRINTER = "Kiln Test Raw"
cargo test -p kiln-provider-windows --test spooler -- --ignored --test-threads=1
```

- `raw_job_reaches_the_port_byte_for_byte`: 2 copies of a 200 KB payload containing every byte value go through the real spooler, and the output file must equal the input exactly.
- `gdi_text_job_renders_through_the_spooler`: text rendered through "Microsoft Print to PDF" to a temp file, asserting 4 pages.

## 2. Printer matrix

Start the agent (`cargo run -p kiln-agent -- run`), then export the token printed by `kiln-agent token` as `KILN_TOKEN`, and set `KILN_PRINTER` for each printer.

| Category | Example device | Commands | Expected |
|---|---|---|---|
| Office laser/inkjet | any v3/v4 driver | `node examples/text/print-text.mjs` | Text with margins, wrapping and correct font. `capabilities` lists paper sizes and trays. On v4 drivers RAW is refused with `UNSUPPORTED_OPERATION`. |
| Zebra-compatible label | ZD421/GK420 (ZDesigner v3 driver) | `node examples/barcode/print-zpl-label.mjs` | Label with Code 128 and QR. `job.completed`. `copies: 3` prints 3 labels. |
| ESC/POS thermal | Epson TM-T20/T88 | `node examples/raw/print-file.mjs receipt.bin "<printer>" ESC/POS` | Receipt prints, and a cut command in the file cuts. |
| Dot-matrix | Epson LQ/LX/FX series | `node examples/dot-matrix/print-escp-form.mjs`, and `print-text.mjs` with `KILN_TEXT_MODE=RAW` | Resident font, condensed table, bold, form feed to the next top-of-form on tractor paper. |
| PDF virtual printer | Microsoft Print to PDF | `print-text.mjs` (RENDERED) | A save dialog appears (the `PORTPROMPT:` port), and the saved PDF contains the text. |

## 3. Failure scenarios (run per category where applicable)

| Scenario | Procedure | Expected |
|---|---|---|
| Printer unavailable | Unplug USB / power off, then submit | The job stays `QUEUED`/`PRINTING` with `condition` if the spooler reports it. Otherwise it completes when the device returns. Nothing is duplicated. |
| Paper out | Remove media, submit | `job.updated` with `condition: PAPER_OUT` (network printers with SNMP; USB often reports nothing). Printing resumes after reload. |
| Printer disconnected | Delete the printer in Settings while the agent runs | `printer.disconnected` within about 5 s. New jobs return `PRINTER_NOT_FOUND`. |
| Large RAW payload | 50 MB file via `print-file.mjs` | Completes. Memory returns to baseline afterwards. |
| Multiple simultaneous jobs | Run 3 examples in parallel to 2 printers | Per-printer order kept. Printer B not delayed by printer A. |
| Client disconnect during printing | Ctrl+C the example right after `accepted` | The job still completes. `GET /v1/jobs/{id}` shows it. |
| Agent restart | Kill the agent while a job is spooled, then start it | The job settles (`COMPLETED`, `SPOOLER_JOB_RETIRED`). Nothing reprints. |
| Cancel | Pause the printer in Windows, submit, `DELETE /v1/jobs/{id}` | `CANCELLED`, and the job disappears from the Windows queue. |
| Queue congestion | Pause the printer, submit more than `queue_capacity_per_printer` jobs | The spooler accepts them in order. The agent-side queue never exceeds its bound. |

Record the results (date, agent version, driver name and version, port type, outcome) in the release checklist.
