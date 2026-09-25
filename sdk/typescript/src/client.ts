import { bytesToBase64, randomId, toBase64 } from "./encoding.ts";
import { FATAL_CONNECT_ERRORS, KilnError } from "./errors.ts";
import type {
  BinaryInput,
  DotMatrixDocument,
  LabelDocument,
  ReceiptDocument,
  ErrorPayload,
  HtmlOptions,
  ImageOptions,
  Job,
  JobFilter,
  PdfOptions,
  Printer,
  PrinterCapabilities,
  PrinterQueue,
  QueueSummary,
  RawEncoding,
  SessionInfo,
  TextOptions,
} from "./types.ts";
import { TERMINAL_STATUSES } from "./types.ts";

export const DEFAULT_URL = "ws://127.0.0.1:18731/v1/ws";
const PROTOCOL_VERSIONS = [1];
const JOB_CACHE_SIZE = 1000;

type WebSocketCtor = new (url: string) => WebSocket;

export interface ReconnectOptions {
  /** First retry delay (default 500 ms), doubled per attempt with jitter. */
  initialDelayMs?: number;
  /** Upper bound for the delay (default 15 s). */
  maxDelayMs?: number;
  /** Give up after this many consecutive failures (default: never). */
  maxAttempts?: number;
}

export interface PrintClientOptions {
  /** Agent WebSocket URL (default `ws://127.0.0.1:18731/v1/ws`). */
  url?: string;
  /** Client token, or a function returning one (called on every (re)connect). */
  token: string | (() => string | Promise<string>);
  appName?: string;
  appVersion?: string;
  /** Timeout for non-print requests (default 30 s). */
  requestTimeoutMs?: number;
  /** Timeout for print submissions, which may include rendering (default 120 s). */
  printTimeoutMs?: number;
  /** Automatic reconnection (default on). */
  reconnect?: boolean | ReconnectOptions;
  /** WebSocket implementation for runtimes without a global one. */
  WebSocket?: WebSocketCtor;
}

export type ConnectionState = "disconnected" | "connecting" | "connected" | "reconnecting";

/** Selects a printer by id, by name, or with a `Printer` object from `getPrinters()`. */
export type PrinterRef = { printerId: string; printer?: never } | { printer: string | Printer; printerId?: never };

interface JobCommon {
  copies?: number;
  jobName?: string;
  /** Generated automatically when omitted. Reuse it to retry safely. */
  idempotencyKey?: string;
}

export type RawPrint = PrinterRef &
  JobCommon & {
    /** Strings are sent as text (UTF-8) unless `encoding` says otherwise; bytes as base64. */
    data: BinaryInput;
    encoding?: RawEncoding;
    /** ZPL, EPL, CPCL, TSPL, ESC/POS, ESC/P or RAW. */
    language?: string;
  };
export type TextPrint = PrinterRef & JobCommon & { text: string; options?: TextOptions };
type Source = { data: BinaryInput; path?: never; url?: never } | { path: string; data?: never; url?: never } | { url: string; data?: never; path?: never };
export type PdfPrint = PrinterRef & JobCommon & Source & { options?: PdfOptions };
export type ImagePrint = PrinterRef & JobCommon & Source & { options?: ImageOptions };
export type HtmlPrint = PrinterRef & JobCommon & { html: string; options?: HtmlOptions };
export type LabelPrint = PrinterRef & JobCommon & { label: LabelDocument };
export type ReceiptPrint = PrinterRef & JobCommon & { receipt: ReceiptDocument };
export type DotMatrixPrint = PrinterRef & JobCommon & { document: DotMatrixDocument };

export type PrintRequest =
  | ({ type: "RAW" } & RawPrint)
  | ({ type: "TEXT" } & TextPrint)
  | ({ type: "PDF" } & PdfPrint)
  | ({ type: "IMAGE" } & ImagePrint)
  | ({ type: "HTML" } & HtmlPrint)
  | ({ type: "LABEL" } & LabelPrint)
  | ({ type: "RECEIPT" } & ReceiptPrint)
  | ({ type: "DOT_MATRIX" } & DotMatrixPrint);

export interface ClientEvents {
  state: ConnectionState;
  connected: SessionInfo;
  disconnected: { code: number; reason: string };
  error: KilnError;
  /** The agent dropped events for this session; job state was re-synchronised. */
  lagged: { missedEvents: number };
}

interface Pending {
  method: string;
  params: unknown;
  resolve: (value: unknown) => void;
  reject: (error: KilnError) => void;
  timer: ReturnType<typeof setTimeout>;
  /** Safe to send again after a reconnect (reads, and prints with an idempotency key). */
  resendable: boolean;
  isPrint: boolean;
  idempotencyKey: string | null;
  socket: WebSocket | null;
}

type Listener<T> = (value: T) => void;

export class PrintClient {
  readonly #options: Required<Omit<PrintClientOptions, "WebSocket" | "reconnect" | "appVersion">> & {
    appVersion: string | undefined;
    reconnect: Required<ReconnectOptions> | null;
  };
  readonly #WebSocket: WebSocketCtor;
  #ws: WebSocket | null = null;
  #state: ConnectionState = "disconnected";
  #session: SessionInfo | undefined;
  #connecting: Promise<SessionInfo> | null = null;
  #intentionalClose = false;
  #requestCounter = 0;
  #inFlight = new Map<string, Pending>();
  #queue: Pending[] = [];
  #jobs = new Map<string, Job>();
  #jobListeners = new Set<Listener<Job>>();
  #jobIdListeners = new Map<string, Set<Listener<Job>>>();
  #printerListeners = new Set<Listener<Printer>>();
  #listeners = new Map<keyof ClientEvents, Set<Listener<unknown>>>();

  constructor(options: PrintClientOptions) {
    const WS = options.WebSocket ?? (globalThis as { WebSocket?: WebSocketCtor }).WebSocket;
    if (!WS) throw new TypeError("no WebSocket implementation available; pass options.WebSocket");
    this.#WebSocket = WS;
    const reconnect =
      options.reconnect === false
        ? null
        : {
            initialDelayMs: 500,
            maxDelayMs: 15_000,
            maxAttempts: Number.POSITIVE_INFINITY,
            ...(typeof options.reconnect === "object" ? options.reconnect : {}),
          };
    this.#options = {
      url: options.url ?? DEFAULT_URL,
      token: options.token,
      appName: options.appName ?? "kiln-sdk-client",
      appVersion: options.appVersion,
      requestTimeoutMs: options.requestTimeoutMs ?? 30_000,
      printTimeoutMs: options.printTimeoutMs ?? 120_000,
      reconnect,
    };
  }

  get state(): ConnectionState {
    return this.#state;
  }

  /** Handshake result of the current session (permissions, limits, languages). */
  get session(): SessionInfo | undefined {
    return this.#session;
  }

  // ---------------------------------------------------------------- connection

  /** Connects and authenticates. Resolves with the session; safe to call repeatedly. */
  connect(): Promise<SessionInfo> {
    if (this.#state === "connected" && this.#session) return Promise.resolve(this.#session);
    if (this.#connecting) return this.#connecting;
    this.#intentionalClose = false;
    this.#setState("connecting");
    this.#connecting = this.#open()
      .then((session) => {
        this.#afterConnect(session);
        return session;
      })
      .catch((error: KilnError) => {
        this.#setState("disconnected");
        this.#failAll(error);
        throw error;
      })
      .finally(() => {
        this.#connecting = null;
      });
    return this.#connecting;
  }

  /** Closes the connection. Queued requests fail with CONNECTION_ERROR. */
  async disconnect(): Promise<void> {
    this.#intentionalClose = true;
    const ws = this.#ws;
    this.#ws = null;
    this.#setState("disconnected");
    this.#failAll(KilnError.local("CONNECTION_ERROR", "the client disconnected"));
    if (ws && ws.readyState <= 1) {
      await new Promise<void>((resolve) => {
        ws.addEventListener("close", () => resolve(), { once: true });
        ws.close(1000, "client disconnect");
        setTimeout(resolve, 1000);
      });
    }
  }

  /** Subscribes to client lifecycle events. Returns an unsubscribe function. */
  on<K extends keyof ClientEvents>(event: K, listener: Listener<ClientEvents[K]>): () => void {
    let set = this.#listeners.get(event);
    if (!set) this.#listeners.set(event, (set = new Set()));
    set.add(listener as Listener<unknown>);
    return () => set.delete(listener as Listener<unknown>);
  }

  #emit<K extends keyof ClientEvents>(event: K, value: ClientEvents[K]): void {
    for (const listener of this.#listeners.get(event) ?? []) {
      try {
        listener(value);
      } catch {
        // Listener errors must not break the connection.
      }
    }
  }

  #setState(state: ConnectionState): void {
    if (this.#state === state) return;
    this.#state = state;
    this.#emit("state", state);
  }

  async #open(): Promise<SessionInfo> {
    const token = typeof this.#options.token === "function" ? await this.#options.token() : this.#options.token;
    return new Promise<SessionInfo>((resolve, reject) => {
      let settled = false;
      let ws: WebSocket;
      try {
        ws = new this.#WebSocket(this.#options.url);
      } catch (e) {
        reject(KilnError.local("CONNECTION_ERROR", `cannot open ${this.#options.url}: ${String(e)}`));
        return;
      }
      const helloId = `hello-${randomId()}`;
      const fail = (error: KilnError) => {
        if (settled) return;
        settled = true;
        clearTimeout(timer);
        try {
          ws.close();
        } catch {
          /* already closed */
        }
        reject(error);
      };
      const timer = setTimeout(
        () => fail(KilnError.local("TIMEOUT", "the agent did not complete the handshake")),
        this.#options.requestTimeoutMs,
      );
      ws.addEventListener("open", () => {
        ws.send(
          JSON.stringify({
            protocolVersion: 1,
            type: "request",
            id: helloId,
            method: "session.hello",
            params: {
              protocolVersions: PROTOCOL_VERSIONS,
              client: { name: this.#options.appName, version: this.#options.appVersion },
              auth: { type: "token", token },
            },
          }),
        );
      });
      ws.addEventListener("message", (event) => {
        if (!settled) {
          const message = parse(event.data);
          if (message?.id !== helloId) return;
          settled = true;
          clearTimeout(timer);
          if (message.ok) {
            this.#ws = ws;
            resolve(message.result as SessionInfo);
          } else {
            try {
              ws.close();
            } catch {
              /* ignore */
            }
            reject(new KilnError(message.error as ErrorPayload));
          }
          return;
        }
        this.#onMessage(ws, event.data);
      });
      ws.addEventListener("error", () =>
        fail(KilnError.local("CONNECTION_ERROR", `cannot connect to the Kiln agent at ${this.#options.url}`)),
      );
      ws.addEventListener("close", (event) => {
        if (!settled) {
          fail(KilnError.local("CONNECTION_ERROR", `connection closed during handshake (${event.code})`));
          return;
        }
        this.#onClose(ws, event.code, event.reason);
      });
    });
  }

  #afterConnect(session: SessionInfo): void {
    this.#session = session;
    this.#setState("connected");
    this.#emit("connected", session);
    const queued = this.#queue.splice(0);
    for (const pending of queued) this.#send(pending);
    void this.#resyncWatchedJobs();
  }

  #onClose(ws: WebSocket, code: number, reason: string): void {
    if (ws !== this.#ws) return;
    this.#ws = null;
    this.#emit("disconnected", { code, reason });
    // Requests sent on the dead socket: resend if safe, otherwise their outcome is unknown.
    for (const [id, pending] of this.#inFlight) {
      if (pending.socket !== ws) continue;
      this.#inFlight.delete(id);
      pending.socket = null;
      if (pending.resendable && !this.#intentionalClose && this.#options.reconnect) {
        this.#queue.push(pending);
      } else {
        clearTimeout(pending.timer);
        pending.reject(
          KilnError.local("CONNECTION_ERROR", "the connection was lost before the agent replied", {
            outcome: pending.isPrint ? "UNKNOWN" : undefined,
            idempotencyKey: pending.idempotencyKey,
          }),
        );
      }
    }
    if (this.#intentionalClose || !this.#options.reconnect) {
      this.#setState("disconnected");
      this.#failAll(KilnError.local("CONNECTION_ERROR", `the connection closed (${code} ${reason})`));
      return;
    }
    void this.#reconnectLoop();
  }

  async #reconnectLoop(): Promise<void> {
    const policy = this.#options.reconnect;
    if (!policy) return;
    this.#setState("reconnecting");
    for (let attempt = 0; attempt < policy.maxAttempts; attempt++) {
      const base = Math.min(policy.maxDelayMs, policy.initialDelayMs * 2 ** Math.min(attempt, 16));
      await sleep(base * (0.5 + Math.random() / 2));
      if (this.#intentionalClose) return;
      try {
        const session = await this.#open();
        if (this.#intentionalClose) return;
        this.#afterConnect(session);
        return;
      } catch (error) {
        const err = error as KilnError;
        if (FATAL_CONNECT_ERRORS.includes(err.code)) {
          this.#setState("disconnected");
          this.#emit("error", err);
          this.#failAll(err);
          return;
        }
      }
    }
    this.#setState("disconnected");
    const err = KilnError.local("CONNECTION_ERROR", "could not reconnect to the Kiln agent");
    this.#emit("error", err);
    this.#failAll(err);
  }

  #failAll(error: KilnError): void {
    const pending = [...this.#queue, ...this.#inFlight.values()];
    this.#queue = [];
    this.#inFlight.clear();
    for (const p of pending) {
      clearTimeout(p.timer);
      // Queued requests never reached the agent.
      const outcome = p.socket === null ? "NOT_PRINTED" : "UNKNOWN";
      p.reject(
        new KilnError(
          { ...toPayload(error), details: p.isPrint ? { outcome } : error.details },
          p.idempotencyKey,
        ),
      );
    }
  }

  // ---------------------------------------------------------------- requests

  #request<T>(
    method: string,
    params: unknown,
    options: { resendable: boolean; isPrint?: boolean; idempotencyKey?: string | null; timeoutMs?: number } = {
      resendable: true,
    },
  ): Promise<T> {
    return new Promise<T>((resolve, reject) => {
      const isPrint = options.isPrint ?? false;
      const idempotencyKey = options.idempotencyKey ?? null;
      if (this.#state === "disconnected") {
        reject(
          KilnError.local("CONNECTION_ERROR", "not connected; call connect() first", {
            outcome: isPrint ? "NOT_PRINTED" : undefined,
            idempotencyKey,
          }),
        );
        return;
      }
      const timeoutMs = options.timeoutMs ?? (isPrint ? this.#options.printTimeoutMs : this.#options.requestTimeoutMs);
      const pending: Pending = {
        method,
        params,
        resolve: resolve as (value: unknown) => void,
        reject,
        resendable: options.resendable,
        isPrint,
        idempotencyKey,
        socket: null,
        timer: setTimeout(() => {
          const sent = pending.socket !== null;
          this.#queue = this.#queue.filter((p) => p !== pending);
          for (const [id, p] of this.#inFlight) if (p === pending) this.#inFlight.delete(id);
          reject(
            KilnError.local("TIMEOUT", `${method} timed out after ${timeoutMs} ms`, {
              outcome: isPrint ? (sent ? "UNKNOWN" : "NOT_PRINTED") : undefined,
              idempotencyKey,
            }),
          );
        }, timeoutMs),
      };
      if (this.#state === "connected" && this.#ws) this.#send(pending);
      else this.#queue.push(pending);
    });
  }

  #send(pending: Pending): void {
    const ws = this.#ws;
    if (!ws) {
      this.#queue.push(pending);
      return;
    }
    const id = `r${++this.#requestCounter}-${randomId().slice(0, 8)}`;
    pending.socket = ws;
    this.#inFlight.set(id, pending);
    ws.send(JSON.stringify({ protocolVersion: 1, type: "request", id, method: pending.method, params: pending.params }));
  }

  #onMessage(ws: WebSocket, raw: unknown): void {
    const message = parse(raw);
    if (!message) return;
    if (message.type === "event") {
      this.#onEvent(message.event as string, message.data);
      return;
    }
    const pending = this.#inFlight.get(message.id as string);
    if (!pending || pending.socket !== ws) return;
    this.#inFlight.delete(message.id as string);
    clearTimeout(pending.timer);
    if (message.ok) pending.resolve(message.result);
    else pending.reject(new KilnError(message.error as ErrorPayload, pending.idempotencyKey));
  }

  #onEvent(name: string, data: unknown): void {
    if (name.startsWith("job.")) {
      this.#recordJob(data as Job);
    } else if (name.startsWith("printer.")) {
      for (const listener of this.#printerListeners) safeCall(listener, data as Printer);
    } else if (name === "session.lagged") {
      this.#emit("lagged", data as { missedEvents: number });
      void this.#resyncWatchedJobs();
    }
  }

  #recordJob(job: Job): void {
    const known = this.#jobs.get(job.jobId);
    if (known) {
      const a = timeKey(known.updatedAt);
      const b = timeKey(job.updatedAt);
      const same = known.status === job.status && known.delivery === job.delivery && known.condition === job.condition;
      if (a > b || (a === b && same)) return; // stale or unchanged
    }
    if (known && TERMINAL_STATUSES.includes(known.status)) return; // terminal is final
    this.#jobs.delete(job.jobId);
    this.#jobs.set(job.jobId, job);
    if (this.#jobs.size > JOB_CACHE_SIZE) {
      const oldest = this.#jobs.keys().next().value;
      if (oldest !== undefined) this.#jobs.delete(oldest);
    }
    for (const listener of this.#jobListeners) safeCall(listener, job);
    for (const listener of this.#jobIdListeners.get(job.jobId) ?? []) safeCall(listener, job);
  }

  /** After a reconnect or dropped events, fetch the latest state of jobs being watched. */
  async #resyncWatchedJobs(): Promise<void> {
    for (const jobId of this.#jobIdListeners.keys()) {
      try {
        this.#recordJob(await this.getJob(jobId));
      } catch {
        // The job may be gone from history; waiters keep their own timeout.
      }
    }
  }

  // ---------------------------------------------------------------- printers

  getPrinters(): Promise<Printer[]> {
    return this.#request("printers.list", {});
  }

  /** Printer details including capabilities. */
  getPrinter(printer: string | Printer): Promise<Printer> {
    return this.#request("printers.get", printerParams(printer));
  }

  getDefaultPrinter(): Promise<Printer | null> {
    return this.#request("printers.default", {});
  }

  getPrinterCapabilities(printer: string | Printer): Promise<PrinterCapabilities> {
    return this.#request("printers.capabilities", printerParams(printer));
  }

  /** Called for `printer.connected`, `printer.disconnected` and `printer.status.changed`. */
  onPrinterStatus(listener: Listener<Printer>): () => void {
    this.#printerListeners.add(listener);
    return () => this.#printerListeners.delete(listener);
  }

  // ---------------------------------------------------------------- printing

  print(request: PrintRequest): Promise<Job> {
    switch (request.type) {
      case "RAW":
        return this.printRaw(request);
      case "TEXT":
        return this.printText(request);
      case "PDF":
        return this.printPdf(request);
      case "IMAGE":
        return this.printImage(request);
      case "HTML":
        return this.printHtml(request);
      case "LABEL":
        return this.printLabel(request);
      case "RECEIPT":
        return this.printReceipt(request);
      case "DOT_MATRIX":
        return this.printDotMatrix(request);
    }
  }

  async printRaw(request: RawPrint): Promise<Job> {
    let data: string;
    let encoding: RawEncoding;
    if (typeof request.data === "string") {
      data = request.data;
      encoding = request.encoding ?? "utf8";
    } else {
      data = await toBase64(request.data);
      encoding = "base64";
    }
    return this.#print("print.raw", request, { data, encoding, language: request.language });
  }

  printText(request: TextPrint): Promise<Job> {
    return this.#print("print.text", request, { text: request.text, options: request.options });
  }

  async printPdf(request: PdfPrint): Promise<Job> {
    return this.#print("print.pdf", request, { ...(await sourceParams(request)), options: request.options });
  }

  async printImage(request: ImagePrint): Promise<Job> {
    return this.#print("print.image", request, { ...(await sourceParams(request)), options: request.options });
  }

  printHtml(request: HtmlPrint): Promise<Job> {
    return this.#print("print.html", request, { html: request.html, options: request.options });
  }

  /** A label described once; the agent encodes it as ZPL, EPL, TSPL or CPCL. */
  printLabel(request: LabelPrint): Promise<Job> {
    return this.#print("print.label", request, { label: request.label });
  }

  /** An ESC/POS receipt (text styles, columns, barcodes, QR, logo, cut, drawer). */
  printReceipt(request: ReceiptPrint): Promise<Job> {
    return this.#print("print.receipt", request, { receipt: request.receipt });
  }

  /** ESC/P text for dot-matrix printers (pitch, spacing, forms), sent as RAW bytes. */
  printDotMatrix(request: DotMatrixPrint): Promise<Job> {
    return this.#print("print.dotmatrix", request, { document: request.document });
  }

  #print(method: string, request: PrinterRef & JobCommon, body: Record<string, unknown>): Promise<Job> {
    const idempotencyKey = request.idempotencyKey ?? randomId();
    const params = stripUndefined({
      ...targetParams(request),
      ...body,
      copies: request.copies,
      jobName: request.jobName,
      idempotencyKey,
    });
    // With an idempotency key, resending after a reconnect cannot print twice: the agent
    // returns the original job.
    return this.#request<Job>(method, params, { resendable: true, isPrint: true, idempotencyKey }).then((job) => {
      this.#recordJob(job);
      return this.#jobs.get(job.jobId) ?? job;
    });
  }

  // ---------------------------------------------------------------- jobs

  getJobs(filter: JobFilter = {}): Promise<Job[]> {
    return this.#request(
      "jobs.list",
      stripUndefined({
        ...filter,
        since: filter.since instanceof Date ? filter.since.toISOString() : filter.since,
        until: filter.until instanceof Date ? filter.until.toISOString() : filter.until,
      }),
    );
  }

  getJob(jobId: string): Promise<Job> {
    return this.#request("jobs.get", { jobId });
  }

  /** Not retried automatically after a lost connection: check `getJob` instead. */
  cancelJob(jobId: string): Promise<Job> {
    return this.#request<Job>("jobs.cancel", { jobId }, { resendable: false }).then((job) => {
      this.#recordJob(job);
      return job;
    });
  }

  /**
   * Job status updates: for all visible jobs, or for one job. Subscribing to a job the
   * client already heard about replays its latest known state immediately.
   */
  onJobStatus(listener: Listener<Job>): () => void;
  onJobStatus(jobId: string, listener: Listener<Job>): () => void;
  onJobStatus(a: string | Listener<Job>, b?: Listener<Job>): () => void {
    if (typeof a === "function") {
      this.#jobListeners.add(a);
      return () => this.#jobListeners.delete(a);
    }
    const jobId = a;
    const listener = b as Listener<Job>;
    let set = this.#jobIdListeners.get(jobId);
    if (!set) this.#jobIdListeners.set(jobId, (set = new Set()));
    set.add(listener);
    const known = this.#jobs.get(jobId);
    if (known) queueMicrotask(() => safeCall(listener, known));
    return () => {
      set.delete(listener);
      if (set.size === 0) this.#jobIdListeners.delete(jobId);
    };
  }

  /** Resolves when the job reaches COMPLETED, FAILED or CANCELLED. */
  waitForJob(jobId: string, options: { timeoutMs?: number } = {}): Promise<Job> {
    return new Promise<Job>((resolve, reject) => {
      const done = (job: Job) => {
        if (!TERMINAL_STATUSES.includes(job.status)) return;
        clearTimeout(timer);
        unsubscribe();
        resolve(job);
      };
      const timeoutMs = options.timeoutMs ?? 5 * 60_000;
      const timer = setTimeout(() => {
        unsubscribe();
        reject(KilnError.local("TIMEOUT", `job ${jobId} did not finish within ${timeoutMs} ms`));
      }, timeoutMs);
      const unsubscribe = this.onJobStatus(jobId, done);
      // Covers events that happened before we subscribed and were not cached.
      this.getJob(jobId).then((job) => this.#recordJob(job), () => {});
    });
  }

  // ---------------------------------------------------------------- queues

  getQueues(): Promise<QueueSummary[]> {
    return this.#request("queue.list", {});
  }

  getQueue(printer: string | Printer): Promise<PrinterQueue> {
    return this.#request("queue.get", printerParams(printer));
  }
}

// ------------------------------------------------------------------ helpers

function parse(raw: unknown): Record<string, unknown> | null {
  if (typeof raw !== "string") return null;
  try {
    return JSON.parse(raw) as Record<string, unknown>;
  } catch {
    return null;
  }
}

function printerParams(printer: string | Printer): { printerId: string } | { printer: string } {
  // Strings are names; ids come from Printer objects or getPrinters().
  return typeof printer === "string" ? { printer } : { printerId: printer.id };
}

function targetParams(ref: PrinterRef): { printerId: string } | { printer: string } {
  if (ref.printerId !== undefined) return { printerId: ref.printerId };
  if (ref.printer === undefined) throw new TypeError("printerId or printer is required");
  return printerParams(ref.printer);
}

async function sourceParams(source: Source): Promise<Record<string, unknown>> {
  if (source.data !== undefined) {
    return { data: await toBase64(source.data), encoding: "base64" };
  }
  if (source.path !== undefined) return { path: source.path };
  if (source.url !== undefined) return { url: source.url };
  throw new TypeError("one of data, path or url is required");
}

function stripUndefined<T extends Record<string, unknown>>(value: T): T {
  return Object.fromEntries(Object.entries(value).filter(([, v]) => v !== undefined)) as T;
}

function toPayload(error: KilnError): ErrorPayload {
  return {
    errorCode: error.code,
    message: error.message,
    jobId: error.jobId,
    printerId: error.printerId,
    recoverable: error.recoverable,
    details: error.details,
  };
}

/**
 * Orders agent timestamps at microsecond precision. RFC 3339 strings with a variable
 * number of fractional digits do not sort correctly as text.
 */
export function timeKey(iso: string): number {
  const match = /^(.*?)(?:\.(\d+))?Z$/.exec(iso);
  if (!match) return Date.parse(iso) * 1000;
  const micros = Number.parseInt((match[2] ?? "").padEnd(6, "0").slice(0, 6), 10);
  return Date.parse(`${match[1]}Z`) * 1000 + micros;
}

function safeCall<T>(listener: Listener<T>, value: T): void {
  try {
    listener(value);
  } catch {
    // A throwing listener must not affect others or the connection.
  }
}

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

export { bytesToBase64 };
