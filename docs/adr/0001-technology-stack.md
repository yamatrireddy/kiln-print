# ADR 0001 — Technology stack

- **Status:** Accepted. The PDF/HTML rendering choices were revised in [ADR 0004](0004-document-rendering.md).
- **Date:** 2026-09-25
- **Deciders:** Kiln Print maintainers

## Context

The Print Agent is an always-on process on end-user machines (lab benches, POS terminals, warehouse PCs, office desktops). It must:

1. Talk to each OS print system at a low level: Windows spooler (Winspool/GDI/XPS), CUPS on Linux and macOS, and the native macOS print APIs.
2. Send RAW bytes byte for byte, and talk to serial ports and raw TCP sockets.
3. Serve an authenticated, TLS-capable WebSocket/REST API on loopback.
4. Run unattended for months with a small, predictable memory and thread footprint.
5. Ship as a signed, self-contained installer on Windows first, then macOS and Linux.
6. Render PDF, HTML and images in later phases.

The browser/client SDK is TypeScript regardless of this decision. This ADR covers the agent only.

## Options evaluated

Rust (1.85+, tokio), C# (.NET 9) and Java (21 LTS).

| Criterion | Rust | C# / .NET | Java |
|---|---|---|---|
| **Windows spooler** (`OpenPrinter`, `WritePrinter`, `GetJob`, `SetJob`, GDI) | Full. Microsoft's `windows` crate projects all of Win32. | Full, via P/Invoke. `System.Drawing.Printing` and `System.Printing` only exist on Windows. | Partial. `javax.print` hides spooler job ids, so there is no per-job status or cancel. Anything deeper needs JNA. |
| **Linux CUPS** | IPP to `localhost:631` (pure Rust) or FFI to libcups | P/Invoke to libcups, or IPP over HTTP | `javax.print` is CUPS-backed and portable, but it is the lowest common denominator |
| **macOS printing** | CUPS/IPP, plus FFI to `PMPrintSession` for native rendering | Same approach, via P/Invoke | `javax.print` |
| **RAW byte printing** | Trivial | Trivial | `DocFlavor.BYTE_ARRAY.AUTOSENSE` works, but job tracking is weak |
| **Serial ports** | `serialport` crate (mature) | `System.IO.Ports` (cross-platform package) | jSerialComm (bundles JNI) |
| **WebSocket and TLS** | axum/hyper plus rustls: mature, no OpenSSL dependency | Kestrel: excellent | Netty/Jetty: excellent |
| **Idle memory (typical for this class of service)** | ~5–15 MB | ~40–80 MB | ~80–150 MB |
| **Distribution size** | Single ~5–10 MB static executable | Self-contained ~60–80 MB (trimmed ~30 MB). ASP.NET NativeAOT has limits. | jlink/jpackage runtime ~40–60 MB |
| **Background / autostart** | Any model (see ADR 0002). `windows-service` crate if ever needed. | Worker Service | procrun/WinSW |
| **PDF rendering (Phase 2)** | `pdfium-render` (PDFium, BSD-licensed) | PDFium wrappers, or `Windows.Data.Pdf` | PDFBox (pure Java): the strongest option here |
| **HTML rendering (Phase 2)** | Headless Chromium over CDP (`chromiumoxide`), or WebView2 | PuppeteerSharp/Playwright, WebView2: strongest here | Playwright-java |
| **Native UI (consent prompt, tray)** | `TaskDialogIndirect`/tray via `windows`; small but manual | WinForms/WPF: fastest to build | Swing/JavaFX: dated look, heavy |
| **Memory safety** | Safe by default. `unsafe` is confined to provider crates and forbidden elsewhere at compile time. | Managed | Managed |
| **Hiring / familiarity** | Smaller talent pool, steeper learning curve | Large pool | Large pool |
| **Development speed on Windows** | Good | Best | Good |

**Measured on the Phase 1 build** (Windows 11, release profile, `--mock` plus the Windows provider, 2026-09-25):

| Metric | Value |
|---|---|
| `kiln-agent.exe` size | 7.1 MB, self-contained (includes SQLite and the HTTP/WebSocket stack) |
| Idle | 19.0 MB working set, 7.1 MB private, 14 threads |
| After 800 jobs × 256 KiB (200 MB of payload) across 5 printers | 28.7 MB working set, 16.2 MB private, 24 threads, no growth between rounds |

## Decision

**Rust** for the Print Agent and every agent-side crate. **TypeScript** for the client SDK.

Why Rust wins here:

- **Footprint and predictability.** The agent runs all day on low-spec POS and lab machines. A single-digit-MB, GC-free process with an explicitly bounded thread pool (`max_blocking_threads = 32`, per-subsystem semaphores) is the best fit for "multiple applications and printers simultaneously, without uncontrolled thread creation."
- **Low-level access on every OS without a lowest-common-denominator layer.** We need spooler job ids, `GetJob` status bits, `AbortPrinter` on partial writes, and driver-version introspection. Java's portable abstraction deliberately hides these. C# matches Rust on Windows but needs the same hand-written FFI as Rust on Linux and macOS.
- **Distribution.** One small signed executable per platform, with no runtime to install or patch, and no OpenSSL (TLS uses rustls).
- **Safety at the FFI boundary.** Parsing untrusted input from any local process, and doing pointer-heavy spooler work, is exactly where memory-safety bugs become security bugs. Rust keeps parsing and business logic in `#![forbid(unsafe_code)]` crates, and every `unsafe` block sits in `kiln-provider-windows` with a written safety justification.

## Consequences and trade-offs accepted

- **C# is a credible alternative.** It would be the right choice for a .NET-heavy team focused mainly on Windows. The architecture (wire protocol, provider/renderer/protocol interfaces, job lifecycle) is language-agnostic, so this decision can be revisited without changing clients.
- **Native UI costs more.** The first-connection consent prompt ("Application X wants permission…") will use `TaskDialogIndirect` on Windows and the platform equivalent elsewhere. The dashboard is a web UI served by the agent, so it needs no native toolkit.
- **HTML rendering** (Phase 2) will drive a sandboxed headless Chromium (or WebView2 on Windows) as a separate process with JavaScript and network disabled by default. This works equally well from any of the three languages.
- **PDF rendering** will use PDFium via `pdfium-render`, with the PDFium binary shipped alongside the agent (BSD-3-Clause).
- **Contributor ramp-up** is longer than for C# or Java. It is mitigated by keeping the engine small and heavily tested, and by isolating platform code.

## Rejected

- **Java.** `javax.print` would give quick portability, but it cannot provide per-job spooler tracking or cancellation, which are core requirements. Working around it needs JNA on every platform, which removes the portability advantage. It also has the heaviest runtime footprint.
- **C#.** It was a close second. It lost on footprint, distribution size and cross-platform FFI parity, not on capability.
