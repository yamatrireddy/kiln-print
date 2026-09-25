# ADR 0003 — Never automatically retry a submission that may have reached a device

- **Status:** Accepted
- **Date:** 2026-09-25

## Context

Label and receipt printers are unforgiving: a duplicate shipping label, pharmacy label or cheque is a real-world incident, not a cosmetic bug. Printing failures often happen after some bytes have already left the machine: USB stalls, spooler RPC errors, driver timeouts, the agent being killed.

## Decision

1. The engine **never retries** `PrintProvider::submit`. A failed or timed-out submission ends the job as `FAILED` and records its outcome in `error.details.outcome`:
   - `NOT_PRINTED`: nothing was handed to the spooler or device. Resubmitting is safe, and `recoverable` is `true`.
   - `UNKNOWN`: the handover may have been partial or complete. `recoverable` is `false`, and a human or the application must decide.
2. After an agent restart, jobs are **never resent**. Payloads are not persisted, and jobs are reconciled instead (see `print-job-lifecycle.md`).
3. Clients get **idempotent submission** instead of retries. A client that is unsure whether a request arrived (for example after a reconnect) resubmits with the same `idempotencyKey`. The agent returns the original job rather than printing again.
4. Providers must make submissions **all-or-nothing** where the platform allows it. The Windows provider calls `AbortPrinter` on any failure after `StartDocPrinter`, so a partially written spool file never prints.

## Consequences

- Some transient failures surface to the application where a naive retry would have "just worked." That is deliberate. The error model tells the application whether a retry is safe.
- A future "safe retry" (Phase 3+) may retry only errors a provider explicitly flags as pre-transmission, such as a TCP connection refused before any byte was written. It will be opt-in per printer.
