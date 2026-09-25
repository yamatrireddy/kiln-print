# @kiln-print/sdk

TypeScript/JavaScript client for the Kiln Print agent. It works in browsers and in Node 22+, and has no runtime dependencies.

```ts
import { PrintClient, KilnError } from "@kiln-print/sdk";

const client = new PrintClient({ token: myClientToken, appName: "Lab Application" });
await client.connect();

const printers = await client.getPrinters();
const zebra = printers.find((p) => p.name.startsWith("ZDesigner"))!;

// Label printers: RAW bytes, delivered unchanged. Strings are sent as text.
const label = await client.printRaw({ printer: zebra, language: "ZPL", data: "^XA^FO50,50^FDHello^FS^XZ" });

// Office printers: PDF, image, HTML, text.
const invoice = await client.printPdf({
  printer: "HP LaserJet",
  data: pdfBytes, // Uint8Array | ArrayBuffer | Blob | base64 string
  copies: 2,
  options: { pageRange: "1-2", paperSize: "A4", duplex: "LONG_EDGE", scale: "SHRINK_TO_FIT" },
});

const done = await client.waitForJob(invoice.jobId);
console.log(done.status, done.delivery, done.completion);
```

## API

| Method | Notes |
|---|---|
| `connect()` / `disconnect()` | `connect` resolves with the session (permissions, limits, supported languages). |
| `getPrinters()`, `getPrinter(p)`, `getDefaultPrinter()`, `getPrinterCapabilities(p)` | `p` is a printer name or a `Printer` object. |
| `print(request)` | Generic: `{ type: "RAW" \| "TEXT" \| "PDF" \| "IMAGE" \| "HTML", ... }`. |
| `printRaw`, `printText`, `printPdf`, `printImage`, `printHtml` | Each resolves with the created `Job` (status `QUEUED`, delivery `REQUEST_ACCEPTED`). |
| `getJobs(filter)`, `getJob(id)`, `cancelJob(id)`, `waitForJob(id)` | Filter by status, printer, client, `since`/`until`. |
| `getQueues()`, `getQueue(p)` | Agent queue and OS spooler queue. |
| `onJobStatus(listener)`, `onJobStatus(jobId, listener)` | Per-job subscriptions replay the latest known state. |
| `onPrinterStatus(listener)` | `printer.connected` / `disconnected` / `status.changed`. |
| `on("state" \| "connected" \| "disconnected" \| "error" \| "lagged", listener)` | Connection lifecycle. |

Choose the target with `printer` (a name or a `Printer`) or `printerId`.

## Handled for you

- **Handshake and version negotiation** (`session.hello`, protocol v1).
- **Reconnection** with jittered exponential backoff (`reconnect: { initialDelayMs, maxDelayMs, maxAttempts }`, or `false`). Reconnection stops on errors it cannot fix (`CLIENT_NOT_TRUSTED`, `ACCESS_DENIED`).
- **Requests made while reconnecting** are queued and sent once the connection is back.
- **Duplicate protection.** Every print gets an `idempotencyKey`, generated unless you pass one. Prints lost with a dropped connection are resent automatically, and the agent returns the original job instead of printing twice. `cancelJob` is never resent.
- **Timeouts.** `requestTimeoutMs` (30 s) applies to queries and `printTimeoutMs` (120 s) to prints.
- **Request ids and response matching.** Responses may arrive out of order.
- **Event ordering.** Job events that arrive before the response carrying the `jobId` are cached, and later subscribers get them. After a reconnect or a `session.lagged` notice, watched jobs are re-fetched.
- **Typed errors.** `KilnError` carries `code`, `message`, `jobId`, `printerId`, `recoverable`, `details`, `outcome` (`NOT_PRINTED` | `UNKNOWN`) and `idempotencyKey`.

```ts
try {
  await client.printPdf({ printer: "Front Desk", data });
} catch (e) {
  if (e instanceof KilnError && e.code === "TIMEOUT" && e.outcome === "UNKNOWN") {
    // Safe: the agent de-duplicates by key, so this cannot print twice.
    await client.printPdf({ printer: "Front Desk", data, idempotencyKey: e.idempotencyKey! });
  }
}
```

## Development

```bash
npm install
```

```bash
npm run build
```

```bash
npm test
```

The tests start a real agent (`target/debug/kiln-agent`, built with `cargo build -p kiln-agent`) with simulated printers.
