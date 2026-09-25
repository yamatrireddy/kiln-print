# Testing

| Layer | Location | Runs in CI | What it covers |
|---|---|---|---|
| Unit | `#[cfg(test)]` modules in every crate | yes (all OSes) | job transitions, error serialisation, byte budget, printer-id stability, language inspection, encoders (IBM437, strict WHATWG), text layout/wrap/pagination, Windows flag mapping, origin/Host parsing, token auth, rate limiting, config validation, SQLite repository |
| Engine integration | `print-core/tests/engine.rs` | yes | full lifecycle with events, byte-exact RAW, direct delivery, validation-creates-failed-job, payload/copies limits, printer scope, strict languages, provider failure without retry, **slow printer does not block others**, **per-printer ordering**, **queue congestion** (depth and byte budget), cancel queued/spooled/external, **idempotency**, blocking conditions, submission timeout (outcome UNKNOWN), **restart reconciliation**, shutdown, discovery events, discovery failure tolerance |
| Agent end-to-end | `agent/tests/api.rs` | yes | real agent on an ephemeral port: WebSocket and REST, handshake rules, bad token (audited), version negotiation, handshake timeout, **unknown origins refused before upgrade**, **DNS rebinding**, origin binding, permissions and printer scope, event isolation, malformed/duplicate/unknown-field requests, rate limiting, payload limits, idempotent resubmission across reconnect, **jobs across an agent restart** |
| Windows spooler | `providers/windows/tests/spooler.rs` | Windows runners; spool-submitting tests are `#[ignore]` | enumeration stability, capabilities and queues of every printer, error mapping; ignored: GDI text → PDF, PDF rasterisation (copies, page range, A5 landscape), image placement, completion via change notifications (all to the Microsoft Print to PDF virtual printer), byte-exact RAW through a v3 driver; non-ignored: unknown paper and corrupt PDF rejected before spooling, default paper |
| HTML rendering | `renderers/tests/html.rs` | yes, where a browser exists | `@page` size, page breaks, page ranges, footer templates, **no network or loopback access from documents** |
| SDK | `sdk/typescript/test/*.test.ts` | yes (`cargo build -p kiln-agent`, then `npm test`) | handshake, all print helpers, typed errors, timeout plus safe retry by idempotency key, bad token without reconnect loop, **reconnect after agent restart with queued print delivered exactly once**, timestamp ordering |
| Hardware | `tests/hardware/README.md` | no (manual) | real office, Zebra, ESC/POS, dot-matrix and PDF printers |

## Commands

```bash
cargo test --workspace
```

```bash
cargo clippy --workspace --all-targets -- -D warnings
```

```bash
cargo test -p kiln-provider-windows --test spooler -- --ignored --test-threads=1
```

## Required-scenario matrix

| Scenario | Covered by |
|---|---|
| Printer unavailable / offline | `kiln-provider-mock` `set_online(false)` → `PRINTER_OFFLINE`; hardware checklist |
| Printer disconnected | `discovery_emits_printer_events`; restart test with a removed printer |
| Invalid printer | `validation_failures_still_create_a_failed_job`, REST 404 |
| Large PDF | raster path bands pages into ≤ 8 MB slices; hardware checklist (50-page PDF) |
| Large RAW payload | `base64_payload_is_byte_exact_end_to_end` (70 KB); `raw_job_reaches_the_port_byte_for_byte` (2 × 200 KB through the spooler); limits tests |
| Multiple simultaneous jobs | ordering, isolation and congestion tests |
| Client disconnect during printing | `idempotent_resubmission_returns_the_same_job` (jobs continue and are recoverable by key) |
| Agent restart | `restart_reconciles_unfinished_jobs_without_reprinting`, `jobs_survive_an_agent_restart` |
| Unauthorized client | bad token, unknown origin, admin-from-browser, permissions and scope tests |
| Malformed payload | `malformed_and_duplicate_requests`, encoding tests |
| Job cancellation | queued, spooled and external-deletion tests; REST 409 on terminal |
| Printer queue congestion | `full_queue_applies_backpressure`, `byte_budget_applies_backpressure` |
