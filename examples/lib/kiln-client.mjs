// Minimal protocol-v1 client used by the examples (Node 22+ or any browser).
//
// This is deliberately tiny and dependency-free. Production applications should use the
// TypeScript SDK (Phase 2), which adds reconnection, timeouts and typed errors.

export class KilnError extends Error {
  constructor(error) {
    super(`${error.errorCode}: ${error.message}`);
    this.errorCode = error.errorCode;
    this.jobId = error.jobId;
    this.recoverable = error.recoverable;
    this.details = error.details;
  }
}

export async function connect({
  url = process.env.KILN_URL ?? "ws://127.0.0.1:18731/v1/ws",
  token = process.env.KILN_TOKEN,
  name = "kiln-example",
} = {}) {
  if (!token) {
    throw new Error("Set KILN_TOKEN (run `kiln-agent token` to see the local admin token).");
  }
  const ws = new WebSocket(url);
  const pending = new Map();
  const listeners = new Set();
  let nextId = 0;

  ws.addEventListener("message", (msg) => {
    const data = JSON.parse(msg.data);
    if (data.type === "event") {
      for (const listener of listeners) listener(data);
      return;
    }
    const waiter = pending.get(data.id);
    if (!waiter) return;
    pending.delete(data.id);
    data.ok ? waiter.resolve(data.result) : waiter.reject(new KilnError(data.error));
  });
  ws.addEventListener("close", (ev) => {
    for (const { reject } of pending.values()) reject(new Error(`connection closed (${ev.code} ${ev.reason})`));
    pending.clear();
  });
  await new Promise((resolve, reject) => {
    ws.addEventListener("open", resolve, { once: true });
    ws.addEventListener("error", () => reject(new Error(`cannot connect to ${url}`)), { once: true });
  });

  const call = (method, params = {}) =>
    new Promise((resolve, reject) => {
      const id = `${Date.now().toString(36)}-${++nextId}`;
      pending.set(id, { resolve, reject });
      ws.send(JSON.stringify({ protocolVersion: 1, type: "request", id, method, params }));
    });

  const session = await call("session.hello", {
    protocolVersions: [1],
    client: { name, version: "1.0.0" },
    auth: { type: "token", token },
  });

  return {
    session,
    call,
    onEvent(listener) {
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
    /** Resolves with the final job once it reaches COMPLETED, FAILED or CANCELLED. */
    waitForJob(jobId, { timeoutMs = 60_000 } = {}) {
      return new Promise((resolve, reject) => {
        const timer = setTimeout(() => {
          off();
          reject(new Error(`job ${jobId} did not finish within ${timeoutMs} ms`));
        }, timeoutMs);
        const off = this.onEvent((ev) => {
          if (ev.data?.jobId !== jobId) return;
          console.log(`  ${ev.event.padEnd(14)} status=${ev.data.status} delivery=${ev.data.delivery}`);
          if (["job.completed", "job.failed", "job.cancelled"].includes(ev.event)) {
            clearTimeout(timer);
            off();
            resolve(ev.data);
          }
        });
      });
    },
    close: () => ws.close(1000),
  };
}

/** Picks a printer by `KILN_PRINTER` (name or id) or falls back to `predicate`. */
export async function choosePrinter(client, predicate = () => true) {
  const printers = await client.call("printers.list");
  const wanted = process.env.KILN_PRINTER;
  const printer = wanted
    ? printers.find((p) => p.id === wanted || p.name.toLowerCase() === wanted.toLowerCase())
    : printers.find(predicate);
  if (!printer) {
    const names = printers.map((p) => `  - ${p.name} (${p.id})`).join("\n");
    throw new Error(`No matching printer. Set KILN_PRINTER to one of:\n${names}`);
  }
  return printer;
}
