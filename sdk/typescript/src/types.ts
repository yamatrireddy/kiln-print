/**
 * Kiln Print protocol v1 types. They mirror `docs/protocol.md`; unknown fields sent by
 * newer agents are preserved at runtime and simply not typed here.
 */

export type ConnectionType = "LOCAL" | "NETWORK" | "USB" | "SERIAL" | "VIRTUAL";
export type PrinterState = "READY" | "PRINTING" | "PAUSED" | "OFFLINE" | "ERROR" | "UNKNOWN";
export type DocumentType = "RAW" | "TEXT" | "PDF" | "HTML" | "IMAGE";
export type Orientation = "PORTRAIT" | "LANDSCAPE";

export interface PaperSize {
  id: string;
  name: string;
  widthMm: number;
  heightMm: number;
}

export interface PrinterCapabilities {
  paperSizes: PaperSize[] | null;
  color: boolean | null;
  duplex: boolean | null;
  resolutions: { xDpi: number; yDpi: number }[] | null;
  maxCopies: number | null;
  collate: boolean | null;
  orientations: Orientation[] | null;
  trays: { id: string; name: string }[] | null;
  /** Whether byte-for-byte RAW data is accepted. */
  raw: boolean | null;
  datatypes: string[] | null;
  /** Document types this agent can deliver to the printer. */
  documentTypes: DocumentType[];
}

export interface Printer {
  id: string;
  name: string;
  displayName: string;
  provider: string;
  type: ConnectionType;
  driver: string | null;
  port: string | null;
  location: string | null;
  default: boolean;
  /** Best effort: many USB drivers report online while unplugged. */
  online: boolean;
  status: PrinterState;
  conditions: string[];
  queuedJobs: number | null;
  capabilities?: PrinterCapabilities;
}

export type JobStatus =
  | "RECEIVED"
  | "VALIDATING"
  | "QUEUED"
  | "PRINTING"
  | "COMPLETED"
  | "FAILED"
  | "CANCELLED";

/** How far the document has travelled; distinguishes "accepted" from "spooled". */
export type DeliveryStage = "REQUEST_ACCEPTED" | "SUBMITTING" | "SPOOLER_ACCEPTED" | "DEVICE_DELIVERED";

/** Why a job is COMPLETED. None of these proves paper came out (see the lifecycle doc). */
export type CompletionEvidence = "SPOOLER_REPORTED_PRINTED" | "SPOOLER_JOB_RETIRED" | "BYTES_DELIVERED";

export const TERMINAL_STATUSES: readonly JobStatus[] = ["COMPLETED", "FAILED", "CANCELLED"];

export interface Job {
  jobId: string;
  clientId: string;
  printerId: string | null;
  printerName: string | null;
  documentType: DocumentType;
  language: string | null;
  jobName: string | null;
  status: JobStatus;
  delivery: DeliveryStage;
  copies: number;
  sizeBytes: number;
  spoolerJobId: number | null;
  /** Condition holding the job right now (e.g. PAPER_OUT); not a failure. */
  condition: ErrorCode | null;
  completion: CompletionEvidence | null;
  warnings: string[];
  idempotencyKey: string | null;
  createdAt: string;
  updatedAt: string;
  queuedAt: string | null;
  submittedAt: string | null;
  startedAt: string | null;
  completedAt: string | null;
  error: ErrorPayload | null;
}

export type ErrorCode =
  | "PRINTER_NOT_FOUND"
  | "PRINTER_OFFLINE"
  | "PRINTER_BUSY"
  | "PAPER_OUT"
  | "PAPER_JAM"
  | "ACCESS_DENIED"
  | "CLIENT_NOT_TRUSTED"
  | "AUTHENTICATION_REQUIRED"
  | "INVALID_PAYLOAD"
  | "PAYLOAD_TOO_LARGE"
  | "UNSUPPORTED_DOCUMENT"
  | "UNSUPPORTED_OPERATION"
  | "UNSUPPORTED_PROTOCOL_VERSION"
  | "PRINT_FAILED"
  | "SPOOLER_ERROR"
  | "CONNECTION_ERROR"
  | "TIMEOUT"
  | "QUEUE_FULL"
  | "RATE_LIMITED"
  | "JOB_NOT_FOUND"
  | "INVALID_JOB_STATE"
  | "INTERNAL_ERROR";

export interface ErrorPayload {
  errorCode: ErrorCode;
  message: string;
  jobId: string | null;
  printerId: string | null;
  recoverable: boolean;
  details: Record<string, unknown> | null;
}

export interface QueueSummary {
  printerId: string;
  printerName: string;
  online: boolean;
  status: PrinterState;
  agentQueued: number;
  spoolerActive: number;
  spoolerTotal: number | null;
}

export interface QueueEntry {
  spoolerJobId: number;
  documentName: string | null;
  status: string[];
  statusText: string | null;
  position: number | null;
  totalPages: number | null;
  pagesPrinted: number | null;
  sizeBytes: number | null;
  submittedAt: string | null;
  kilnJob: Job | null;
}

export interface PrinterQueue {
  printerId: string;
  printerName: string;
  agentQueue: Job[];
  spoolerQueue: QueueEntry[];
}

export interface LanguageInfo {
  id: string;
  name: string;
  family: "LABEL" | "RECEIPT" | "DOT_MATRIX" | "GENERIC";
  aliases: string[];
}

export interface SessionInfo {
  protocolVersion: number;
  sessionId: string;
  agent: { name: string; version: string };
  client: {
    clientId: string;
    name: string;
    kind: "ADMIN" | "CLIENT";
    permissions: string[];
    printers: string[];
    origin: string | null;
  };
  limits: { maxDocumentBytes: number; maxMessageBytes: number; maxCopies: number };
  features: { documentTypes: DocumentType[]; languages: LanguageInfo[] };
  heartbeatSeconds: number;
}

// ------------------------------------------------------------------ options

export type PaperRequest = string | { widthMm: number; heightMm: number };

export interface Margins {
  top: number;
  right: number;
  bottom: number;
  left: number;
}

/** Page setup shared by PDF, image and HTML printing. Unset = printer default. */
export interface PageSetupOptions {
  /** "A4", "Letter", a driver paper id, or explicit dimensions. */
  paperSize?: PaperRequest;
  /** Unset: follow the content (landscape pages print landscape). */
  orientation?: Orientation;
  marginsMm?: Margins;
  duplex?: "SIMPLEX" | "LONG_EDGE" | "SHORT_EDGE";
  color?: "COLOR" | "MONOCHROME";
  /** Tray id or name from `PrinterCapabilities.trays`. */
  tray?: string;
}

export interface TextOptions {
  /** RENDERED (driver layout, default) or RAW (encoded bytes, dot-matrix/receipt). */
  mode?: "RENDERED" | "RAW";
  encoding?: string;
  lineEnding?: "CRLF" | "LF" | "CR";
  formFeed?: boolean;
  fontFamily?: string;
  fontSize?: number;
  bold?: boolean;
  alignment?: "LEFT" | "CENTER" | "RIGHT";
  marginsMm?: Margins;
  orientation?: Orientation;
  wrap?: boolean;
  tabWidth?: number;
}

export interface PdfOptions extends PageSetupOptions {
  /** e.g. "1-3,5,8-". */
  pageRange?: string;
  /** "FIT" | "SHRINK_TO_FIT" (default) | "ACTUAL_SIZE", or a percentage. */
  scale?: "FIT" | "SHRINK_TO_FIT" | "ACTUAL_SIZE" | number;
  /** Rasterisation cap (Windows). */
  dpi?: number;
}

export interface ImageOptions extends PageSetupOptions {
  fit?: "ORIGINAL" | "FIT" | "SHRINK_TO_FIT" | "FILL";
  /** Percentage of the original size; overrides `fit`. */
  scale?: number;
  rotate?: 0 | 90 | 180 | 270;
  /** Image resolution for ORIGINAL/scale sizing (default: file metadata, else 96). */
  dpi?: number;
  align?: "CENTER" | "TOP_LEFT";
}

export interface HtmlOptions extends PageSetupOptions {
  pageRange?: string;
  /** Layout zoom, 10-200 percent. */
  scale?: number;
  printBackground?: boolean;
  preferCssPageSize?: boolean;
  /** Templates; elements with class pageNumber, totalPages, date, title are filled in. */
  headerHtml?: string;
  footerHtml?: string;
  dpi?: number;
}

export type RawEncoding = "base64" | "binary" | "hex" | "utf8" | "latin1";

/** Binary document input: bytes, a Blob, or a base64 string. */
export type BinaryInput = Uint8Array | ArrayBuffer | Blob | string;

export interface JobFilter {
  status?: JobStatus | JobStatus[];
  printerId?: string;
  clientId?: string;
  since?: Date | string;
  until?: Date | string;
  limit?: number;
  offset?: number;
}
