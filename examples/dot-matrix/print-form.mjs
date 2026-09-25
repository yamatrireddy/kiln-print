// A continuous-form invoice on an ESC/P dot-matrix printer, described as a DOT_MATRIX
// document: the agent emits pitch, spacing, form length and styles as ESC/P commands.
// Nothing is rasterised; the printer uses its resident fonts at full speed.
// (See print-escp-form.mjs for the same form built byte by byte.)
//
//   (cd sdk/typescript && npm install && npm run build)
//   KILN_TOKEN=... KILN_PRINTER="Mock Epson LQ-590 Dot Matrix" node examples/dot-matrix/print-form.mjs
import { KilnError, PrintClient } from "../../sdk/typescript/dist/index.js";

const document = {
  cpi: 10,
  lpi: 6,
  pins: 24,
  quality: "NLQ",
  formLengthInches: 11, // tractor-feed form length
  skipPerforationLines: 3, // continuous paper
  encoding: "ibm437",
  lines: [
    { type: "LINE", text: "KILN PRINT DEMO SUPPLIES", bold: true, doubleWidth: true },
    "Invoice 2026-0042                     25 Sep 2026",
    "-".repeat(50),
    { type: "LINE", text: "QTY  ITEM                                   UNIT      TOTAL", condensed: true },
    { type: "LINE", text: "  2  Thermal labels 100x150 (roll of 500)    12.50      25.00", condensed: true },
    { type: "LINE", text: "  1  Ribbon, wax/resin 110mm x 300m          18.90      18.90", condensed: true },
    "-".repeat(50),
    { type: "LINE", text: "TOTAL                                    43.90", bold: true },
    { type: "LINE_FEED", lines: 2 },
    "Payable within 30 days.",
  ],
};

const client = new PrintClient({ token: process.env.KILN_TOKEN, appName: "dot-matrix-form-example" });
await client.connect();
try {
  const printer = process.env.KILN_PRINTER ?? "Mock Epson LQ-590 Dot Matrix";
  const job = await client.printDotMatrix({ printer, document, copies: 2, jobName: "Invoice 2026-0042" });
  const done = await client.waitForJob(job.jobId);
  console.log(`${printer}: ${done.status} (${done.completion ?? done.error?.message})`);
} catch (error) {
  if (!(error instanceof KilnError)) throw error;
  console.error(String(error));
  process.exitCode = 1;
} finally {
  await client.disconnect();
}
