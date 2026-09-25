# @kiln-print/sdk (Phase 2)

The TypeScript SDK implements [`docs/protocol.md`](../../docs/protocol.md) for browsers and Node.

```ts
const client = new PrintClient({ token, url: "ws://127.0.0.1:18731/v1/ws" });
await client.connect();
const printers = await client.getPrinters();
const job = await client.printRaw({ printerId: printers[0].id, language: "ZPL", data: zpl, encoding: "utf8" });
client.onJobStatus(job.jobId, (j) => console.log(j.status, j.delivery));
```

The planned surface is `connect`, `disconnect`, `getPrinters`, `getDefaultPrinter`, `getPrinterCapabilities`, `print`, `printPdf`, `printHtml`, `printImage`, `printText`, `printRaw`, `getJobs`, `getJob`, `cancelJob`, `onPrinterStatus` and `onJobStatus`. The SDK will also handle these automatically:

- reconnection with backoff, and re-subscription;
- the `session.hello` handshake, including protocol-version negotiation;
- request ids and per-call timeouts;
- mapping errors to a typed `KilnError` (`errorCode`, `recoverable`, `details.outcome`);
- buffering events that arrive before their job's response (see "ordering caveat" in the protocol);
- automatic `idempotencyKey`s for print calls, so retrying after a reconnect cannot duplicate output.

Until then, [`examples/lib/kiln-client.mjs`](../../examples/lib/kiln-client.mjs) is a minimal, dependency-free reference client.
