/**
 * Kiln Print protocol v1 types. They mirror `docs/protocol.md`; unknown fields sent by
 * newer agents are preserved at runtime and simply not typed here.
 */

export type ConnectionType = "LOCAL" | "NETWORK" | "USB" | "SERIAL" | "VIRTUAL";
export type PrinterState = "READY" | "PRINTING" | "PAUSED" | "OFFLINE" | "ERROR" | "UNKNOWN";
export type DocumentType = "RAW" | "TEXT" | "PDF" | "HTML" | "IMAGE" | "LABEL" | "RECEIPT" | "DOT_MATRIX";
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
  /** Command language the printer is known to speak (ZPL, ESC/POS, …), if detected or configured. */
  language: string | null;
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

// ------------------------------------------------------------------ labels, receipts, dot matrix

export type Symbology = "CODE128" | "CODE39" | "EAN13" | "EAN8" | "UPC_A" | "ITF";
export type QrErrorCorrection = "L" | "M" | "Q" | "H";
export type Rotation = 0 | 90 | 180 | 270;

/** One element of a language-neutral label. Positions are in millimetres from the top-left. */
export type LabelElement =
  | { type: "TEXT"; xMm: number; yMm: number; text: string; heightMm?: number; rotation?: Rotation; font?: string }
  | {
      type: "BARCODE";
      xMm: number;
      yMm: number;
      symbology: Symbology;
      data: string;
      heightMm?: number;
      /** Narrow bar width in dots (default 2). */
      moduleWidth?: number;
      humanReadable?: boolean;
      rotation?: Rotation;
    }
  | { type: "QR"; xMm: number; yMm: number; data: string; magnification?: number; errorCorrection?: QrErrorCorrection }
  | { type: "DATA_MATRIX"; xMm: number; yMm: number; data: string; moduleSize?: number }
  | { type: "BOX"; xMm: number; yMm: number; widthMm: number; heightMm: number; thicknessMm?: number }
  /** Commands in the target language, inserted verbatim. */
  | { type: "RAW"; data: string };

/** A label described once and encoded by the agent as ZPL, EPL, TSPL or CPCL. */
export interface LabelDocument {
  widthMm: number;
  heightMm: number;
  /** Printer resolution (default 203). */
  dpi?: number;
  /** ZPL | EPL | TSPL | CPCL. Defaults to the printer's language hint. */
  language?: string;
  gapMm?: number;
  /** 0-30. */
  darkness?: number;
  /** Inches per second. */
  speed?: number;
  elements: LabelElement[];
}

type Align = "LEFT" | "CENTER" | "RIGHT";

export type ReceiptItem =
  | {
      type: "TEXT";
      text: string;
      align?: Align;
      bold?: boolean;
      underline?: boolean;
      doubleWidth?: boolean;
      doubleHeight?: boolean;
      invert?: boolean;
      small?: boolean;
    }
  | { type: "COLUMNS"; left: string; right: string; bold?: boolean }
  | { type: "SEPARATOR"; character?: string }
  | { type: "FEED"; lines?: number }
  | {
      type: "BARCODE";
      symbology: Symbology;
      data: string;
      heightDots?: number;
      moduleWidth?: number;
      humanReadable?: boolean;
      align?: Align;
    }
  | { type: "QR"; data: string; size?: number; errorCorrection?: QrErrorCorrection; align?: Align }
  /** Base64 PNG/JPEG/BMP, dithered to black and white. */
  | { type: "IMAGE"; data: string; align?: Align; maxWidthDots?: number }
  | { type: "CUT"; partial?: boolean; feedLines?: number }
  | { type: "DRAWER"; pin?: 0 | 1 }
  /** Base64 ESC/POS bytes inserted verbatim. */
  | { type: "RAW"; data: string };

/** An ESC/POS receipt. */
export interface ReceiptDocument {
  /** Characters per line: 48 for 80 mm paper (default), 32 for 58 mm. */
  widthChars?: number;
  /** ibm437 (default), ibm850, ibm858, windows-1252, ibm866, … */
  codePage?: string;
  /** Cut at the end (default true). */
  cut?: boolean;
  openDrawer?: boolean;
  items: ReceiptItem[];
}

export type DotMatrixItem =
  | {
      type: "LINE";
      text: string;
      bold?: boolean;
      condensed?: boolean;
      doubleWidth?: boolean;
      underline?: boolean;
      italic?: boolean;
      doubleStrike?: boolean;
    }
  | { type: "LINE_FEED"; lines?: number }
  | { type: "FORM_FEED" }
  /** Base64 bytes sent verbatim (printer-specific escape sequences). */
  | { type: "RAW"; data: string };

/** ESC/P text for dot-matrix printers; never rasterised. */
export interface DotMatrixDocument {
  /** 10 (default), 12, 15, 17 or 20. */
  cpi?: 10 | 12 | 15 | 17 | 20;
  /** Lines per inch (default 6). */
  lpi?: number;
  pins?: 9 | 24;
  quality?: "DRAFT" | "NLQ";
  formLengthLines?: number;
  formLengthInches?: number;
  skipPerforationLines?: number;
  leftMargin?: number;
  rightMargin?: number;
  encoding?: string;
  characterTable?: number;
  initialize?: boolean;
  /** Finish with a form feed (default true). */
  formFeed?: boolean;
  lines: (string | DotMatrixItem)[];
}
