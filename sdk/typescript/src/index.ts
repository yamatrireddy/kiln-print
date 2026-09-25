/**
 * Kiln Print SDK: print to local and network printers through the Kiln agent.
 *
 * ```ts
 * import { PrintClient } from "@kiln-print/sdk";
 *
 * const client = new PrintClient({ token });
 * await client.connect();
 * const [printer] = await client.getPrinters();
 * const job = await client.printPdf({ printer, data: pdfBytes, copies: 1, options: { duplex: "LONG_EDGE" } });
 * const done = await client.waitForJob(job.jobId);
 * ```
 */
export { PrintClient, DEFAULT_URL } from "./client.ts";
export type {
  ClientEvents,
  ConnectionState,
  DotMatrixPrint,
  HtmlPrint,
  ImagePrint,
  LabelPrint,
  PdfPrint,
  PrintClientOptions,
  PrintRequest,
  PrinterRef,
  RawPrint,
  ReceiptPrint,
  ReconnectOptions,
  TextPrint,
} from "./client.ts";
export { KilnError } from "./errors.ts";
export type { Outcome } from "./errors.ts";
export { TERMINAL_STATUSES } from "./types.ts";
export type * from "./types.ts";
