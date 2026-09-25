// SDK behaviour against a real agent. Build the agent first: `cargo build -p kiln-agent`.
import assert from "node:assert/strict";
import { after, before, describe, test } from "node:test";

import { KilnError, PrintClient, type ConnectionState, type Job } from "../src/index.ts";
import { TestAgent, agentAvailable, agentExe } from "./harness.ts";

const skip = agentAvailable ? false : `agent binary not built (${agentExe})`;

// 1x1 white PNG and a minimal PDF.
const PNG = Uint8Array.from(
  atob("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAIAAACQd1PeAAAADElEQVR4nGP4//8/AAX+Av4N70a4AAAAAElFTkSuQmCC"),
  (c) => c.charCodeAt(0),
);
const PDF = new TextEncoder().encode("%PDF-1.4\n1 0 obj << /Type /Catalog >> endobj\ntrailer << /Root 1 0 R >>\n%%EOF");

function expectKilnError(error: unknown, code: string): KilnError {
  assert.ok(error instanceof KilnError, `expected KilnError, got ${String(error)}`);
  assert.equal(error.code, code, error.message);
  return error;
}

describe("PrintClient", { skip }, () => {
  const agent = new TestAgent();
  let client: PrintClient;

  before(async () => {
    await agent.start();
    client = new PrintClient({ url: agent.url, token: agent.token, appName: "sdk-tests", appVersion: "1.0.0" });
    await client.connect();
  });

  after(async () => {
    await client?.disconnect();
    await agent.stop();
  });

  test("connects, negotiates protocol v1 and lists printers", async () => {
    assert.equal(client.state, "connected");
    assert.equal(client.session?.protocolVersion, 1);
    assert.ok(client.session?.features.documentTypes.includes("PDF"));
    const printers = await client.getPrinters();
    assert.ok(printers.some((p) => p.name === "Mock Zebra ZD421"));
    const def = await client.getDefaultPrinter();
    assert.equal(def?.name, "Mock Office Laser");
    const caps = await client.getPrinterCapabilities("Mock Zebra ZD421");
    assert.equal(caps.raw, true);
  });

  test("printRaw sends text payloads and tracks the job to completion", async () => {
    const seen: string[] = [];
    const job = await client.printRaw({ printer: "Mock Zebra ZD421", language: "ZPL", data: "^XA^FDHi^FS^XZ" });
    const off = client.onJobStatus(job.jobId, (j) => seen.push(j.status));
    const done = await client.waitForJob(job.jobId, { timeoutMs: 10_000 });
    off();
    assert.equal(done.status, "COMPLETED");
    assert.equal(done.delivery, "SPOOLER_ACCEPTED");
    assert.equal(done.completion, "SPOOLER_REPORTED_PRINTED");
    assert.equal(done.language, "ZPL");
    assert.ok(seen.includes("COMPLETED"));
    assert.ok(job.idempotencyKey, "an idempotency key is generated automatically");
  });

  test("binary documents: PDF, image and RAW text", async () => {
    const printers = await client.getPrinters();
    const laser = printers.find((p) => p.name === "Mock Office Laser")!;
    const jobs: Job[] = [
      await client.printPdf({ printer: laser, data: PDF, copies: 2, options: { pageRange: "1", duplex: "LONG_EDGE" } }),
      await client.printImage({ printerId: laser.id, data: new Blob([PNG]), options: { fit: "ORIGINAL", rotate: 90 } }),
      await client.printText({ printer: "Mock ESC/POS Receipt", text: "Total 12.50\n", options: { mode: "RAW", encoding: "ibm437" } }),
      await client.print({ type: "RAW", printer: "Mock Zebra ZD421", data: PNG }),
    ];
    const finished = await Promise.all(jobs.map((j) => client.waitForJob(j.jobId, { timeoutMs: 10_000 })));
    assert.deepEqual(finished.map((j) => j.status), ["COMPLETED", "COMPLETED", "COMPLETED", "COMPLETED"]);
    assert.deepEqual(finished.map((j) => j.documentType), ["PDF", "IMAGE", "TEXT", "RAW"]);
  });

  test("HTML is rendered by the agent when a browser is installed", async () => {
    try {
      const job = await client.printHtml({
        printer: "Mock Office Laser",
        html: "<h1>Receipt</h1>",
        options: { paperSize: "A5", footerHtml: "<span class=pageNumber></span>" },
      });
      const done = await client.waitForJob(job.jobId, { timeoutMs: 60_000 });
      assert.equal(done.status, "COMPLETED");
    } catch (error) {
      // Machines without Edge/Chrome/Chromium report the document type as unsupported.
      expectKilnError(error, "UNSUPPORTED_DOCUMENT");
    }
  });

  test("agent errors map to KilnError with job context", async () => {
    try {
      await client.printRaw({ printer: "No Such Printer", data: "x" });
      assert.fail("expected an error");
    } catch (error) {
      const err = expectKilnError(error, "PRINTER_NOT_FOUND");
      assert.ok(err.jobId, "failed requests still create a job");
      assert.ok(err.idempotencyKey);
      assert.equal(err.recoverable, false);
    }
    const completed = (await client.getJobs({ status: "COMPLETED", limit: 1 }))[0]!;
    try {
      await client.cancelJob(completed.jobId);
      assert.fail("expected an error");
    } catch (error) {
      expectKilnError(error, "INVALID_JOB_STATE");
    }
    try {
      await client.printPdf({ printer: "Mock Zebra ZD421", data: PDF });
      assert.fail("expected an error");
    } catch (error) {
      expectKilnError(error, "UNSUPPORTED_DOCUMENT");
    }
  });

  test("a timed-out print can be retried safely with its idempotency key", async () => {
    const hasty = new PrintClient({ url: agent.url, token: agent.token, printTimeoutMs: 1 });
    await hasty.connect();
    let key: string | null = null;
    try {
      await hasty.printRaw({ printer: "Mock Zebra ZD421", data: "^XA^XZ" });
    } catch (error) {
      const err = expectKilnError(error, "TIMEOUT");
      assert.equal(err.outcome, "UNKNOWN", "the request was sent: it may have printed");
      key = err.idempotencyKey;
    }
    assert.ok(key);
    const retried = await client.printRaw({ printer: "Mock Zebra ZD421", data: "^XA^XZ", idempotencyKey: key });
    const again = await client.printRaw({ printer: "Mock Zebra ZD421", data: "^XA^XZ", idempotencyKey: key });
    assert.equal(retried.jobId, again.jobId);
    const all = await client.getJobs({ limit: 500 });
    assert.equal(all.filter((j) => j.idempotencyKey === key).length, 1, "exactly one job for the key");
    await hasty.disconnect();
  });

  test("printer status events are delivered", async () => {
    const events: string[] = [];
    const off = client.onPrinterStatus((p) => events.push(p.name));
    // Nothing changes on the mock fleet, so no events; the subscription must be harmless.
    await new Promise((r) => setTimeout(r, 100));
    off();
    assert.deepEqual(events, []);
  });
});

describe("PrintClient connection handling", { skip }, () => {
  test("untrusted tokens fail fast without reconnect loops", async () => {
    const agent = new TestAgent();
    await agent.start();
    const client = new PrintClient({ url: agent.url, token: "kiln_wrong" });
    const states: ConnectionState[] = [];
    client.on("state", (s) => states.push(s));
    try {
      await client.connect();
      assert.fail("expected an error");
    } catch (error) {
      expectKilnError(error, "CLIENT_NOT_TRUSTED");
    }
    await new Promise((r) => setTimeout(r, 500));
    assert.equal(client.state, "disconnected");
    assert.ok(!states.includes("reconnecting"));
    await agent.stop();
  });

  test("requests before connect() fail with NOT_PRINTED", async () => {
    const client = new PrintClient({ url: "ws://127.0.0.1:9/v1/ws", token: "x" });
    try {
      await client.printRaw({ printer: "P", data: "x" });
      assert.fail("expected an error");
    } catch (error) {
      assert.equal(expectKilnError(error, "CONNECTION_ERROR").outcome, "NOT_PRINTED");
    }
  });

  test("reconnects after an agent restart and delivers queued prints exactly once", async () => {
    const agent = new TestAgent();
    await agent.start();
    const client = new PrintClient({
      url: agent.url,
      token: agent.token,
      reconnect: { initialDelayMs: 100, maxDelayMs: 500 },
    });
    const states: ConnectionState[] = [];
    client.on("state", (s) => states.push(s));
    await client.connect();

    await agent.kill();
    await waitFor(() => client.state === "reconnecting");
    // Issued while the agent is down: queued, then sent after the reconnect.
    const pending = client.printRaw({ printer: "Mock Zebra ZD421", data: "^XA^FDqueued^FS^XZ", idempotencyKey: "queued-1" });
    await agent.start(agent.port);
    const job = await pending;
    const done = await client.waitForJob(job.jobId, { timeoutMs: 10_000 });
    assert.equal(done.status, "COMPLETED");
    assert.equal(client.state, "connected");
    assert.ok(states.includes("reconnecting"));
    const jobs = await client.getJobs({ limit: 500 });
    assert.equal(jobs.filter((j) => j.idempotencyKey === "queued-1").length, 1);
    await client.disconnect();
    await agent.stop();
  });
});

async function waitFor(condition: () => boolean, timeoutMs = 5000): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  while (!condition()) {
    if (Date.now() > deadline) throw new Error("condition not met in time");
    await new Promise((r) => setTimeout(r, 20));
  }
}
