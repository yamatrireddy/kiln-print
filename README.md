# Kiln Print

A local print platform: web, desktop and backend applications talk to a small, authenticated **Print Agent** on the user's machine. The agent prints to any locally reachable printer: office laser/inkjet, thermal label and barcode, receipt/POS, dot-matrix, network and virtual. It covers RAW printing byte for byte, driver-rendered documents, job tracking and silent printing for trusted clients.

> **Status: Phase 1 (foundation).** Windows spooler discovery, RAW and text printing, job engine, authenticated WebSocket/REST API, SQLite persistence. PDF/HTML/image, the TypeScript SDK, TLS/pairing, the dashboard and other operating systems follow in later phases (see [docs/mvp-scope.md](docs/mvp-scope.md)).

## Quick start (Windows)

```bash
cargo run -p kiln-agent -- printers
```

```bash
cargo run -p kiln-agent -- run --mock
```

```bash
cargo run -p kiln-agent -- token
```

`printers` lists what the agent can see. `run --mock` starts the agent on `127.0.0.1:18731`; `--mock` adds simulated label, receipt and dot-matrix printers. `token` prints the local admin token.

Then print from any language. With Node 22+:

```bash
KILN_TOKEN=<token> KILN_PRINTER="Mock Zebra ZD421" node examples/barcode/print-zpl-label.mjs
```

```text
Printing to Mock Zebra ZD421
job f214d890-… accepted (QUEUED/REQUEST_ACCEPTED)
  job.spooled    status=QUEUED delivery=SPOOLER_ACCEPTED
  job.printing   status=PRINTING delivery=SPOOLER_ACCEPTED
  job.completed  status=COMPLETED delivery=SPOOLER_ACCEPTED
finished: COMPLETED SPOOLER_REPORTED_PRINTED
```

Web applications are registered as clients bound to their origin:

```bash
cargo run -p kiln-agent -- new-client --id lab-app --name "Lab Application" --origin https://lab.example.com
```

## What makes it safe to leave running

- **Loopback only**, `Host` validation (DNS-rebinding defence), unknown browser origins refused before the WebSocket upgrade.
- **Every request is authenticated.** Clients carry explicit permissions and a printer allow-list. Silent printing is available only to authenticated, trusted clients.
- **Byte-exact RAW.** Payloads are shared, never copied or re-encoded. Language inspection can read bytes but, by type, cannot modify them.
- **Honest job status.** `status`, `delivery` (`REQUEST_ACCEPTED` → `SPOOLER_ACCEPTED`/`DEVICE_DELIVERED`) and `completion` evidence are separate fields, so "accepted" is never reported as "printed."
- **No duplicate labels.** No automatic retries. Idempotency keys allow safe resubmission. Nothing is resent after a restart.
- **Bounded everything.** Per-printer FIFO queues, a global byte budget, a capped blocking pool, rate limits, payload caps.

## Repository layout

```text
print-core/            kiln-core: model, errors, interfaces, engine, queues, monitor, discovery
protocols/             kiln-protocols: ZPL, EPL, CPCL, TSPL, ESC/POS, ESC/P, RAW (read-only inspection)
renderers/             kiln-renderers: RAW passthrough, text (RAW-encoded / native layout)
providers/windows/     kiln-provider-windows: Winspool + GDI (the only crate with unsafe code)
providers/mock/        kiln-provider-mock: scriptable provider for CI and --mock
providers/linux|macos/ Phase 6 notes
agent/                 kiln-agent: config, security, SQLite, WebSocket/REST API, CLI
sdk/typescript/        TypeScript SDK (Phase 2)
dashboard/             management UI (Phase 5)
examples/              runnable Node examples: barcode/ZPL, dot-matrix/ESC-P, text, raw file
tests/hardware/        hardware test procedures and spooler test printer setup
docs/                  architecture, ADRs, protocol, security, lifecycle, Windows strategy, testing
```

## Documentation

| | |
|---|---|
| [Architecture](docs/architecture.md) | components, flows, threading, extension points |
| [ADR 0001: Technology stack](docs/adr/0001-technology-stack.md) | Rust vs C# vs Java, trade-offs |
| [ADR 0002: User-session agent](docs/adr/0002-user-session-agent.md) | why not a Windows service |
| [ADR 0003: No automatic retries](docs/adr/0003-no-automatic-print-retries.md) | duplicate-output policy |
| [Protocol v1](docs/protocol.md) | envelopes, handshake, methods, events, errors, REST |
| [Security model](docs/security-model.md) | threat model, Phase 1 controls, Phase 4 pairing |
| [Job lifecycle](docs/print-job-lifecycle.md) | statuses, delivery stages, completion limits, restart |
| [Windows strategy](docs/windows-strategy.md) | Win32 API map, v3/v4 drivers, status reliability |
| [MVP scope](docs/mvp-scope.md) | phase plan and Phase 1 checklist |
| [Testing](docs/testing.md) | test layers and scenario matrix |

## Development

```bash
cargo test --workspace
```

```bash
cargo clippy --workspace --all-targets -- -D warnings
```

Spooler tests that submit real jobs (to virtual printers) are `#[ignore]`d. See [tests/hardware/README.md](tests/hardware/README.md).
