// Prints a continuous-form invoice on an ESC/P dot-matrix printer as RAW bytes:
// 10 CPI draft text, a condensed table, bold headings, 6 LPI and a form feed.
//
//   KILN_TOKEN=... KILN_PRINTER="Epson LQ-590" node examples/dot-matrix/print-escp-form.mjs
//
// Nothing is rasterised: the printer uses its resident fonts at full speed.
import { connect, choosePrinter } from "../lib/kiln-client.mjs";

const ESC = 0x1b;
const bytes = [];
const cmd = (...b) => bytes.push(...b);
const text = (s) => bytes.push(...Buffer.from(s, "latin1"));
const line = (s = "") => (text(s), cmd(0x0d, 0x0a));

cmd(ESC, 0x40); //               ESC @      initialise
cmd(ESC, 0x32); //               ESC 2      1/6" line spacing (6 LPI)
cmd(ESC, 0x43, 0x00, 11); //     ESC C 0 n  form length 11 inches (tractor feed)
cmd(ESC, 0x50); //               ESC P      10 CPI
cmd(ESC, 0x45); text("KILN PRINT DEMO SUPPLIES"); cmd(ESC, 0x46); line(); // bold on/off
line("Invoice 2026-0042                     25 Sep 2026");
line("-".repeat(50));
cmd(0x0f); //                    SI         condensed (~17 CPI)
line("QTY  ITEM                                   UNIT      TOTAL");
line("  2  Thermal labels 100x150 (roll of 500)    12.50      25.00");
line("  1  Ribbon, wax/resin 110mm x 300m          18.90      18.90");
cmd(0x12); //                    DC2        cancel condensed
line("-".repeat(50));
cmd(ESC, 0x45); line("TOTAL                                    43.90"); cmd(ESC, 0x46);
cmd(0x0c); //                    FF         advance to the next form

const client = await connect({ name: "dot-matrix-example" });
try {
  const printer = await choosePrinter(client, (p) => /epson|lq|lx|fx|dot|matrix/i.test(p.name));
  console.log(`Printing ${bytes.length} bytes of ESC/P to ${printer.name}`);
  const job = await client.call("print.raw", {
    printerId: printer.id,
    language: "ESC/P",
    encoding: "base64",
    data: Buffer.from(bytes).toString("base64"),
    copies: 1,
    jobName: "Invoice 2026-0042",
  });
  const done = await client.waitForJob(job.jobId);
  console.log(`finished: ${done.status}`, done.completion ?? done.error);
} finally {
  client.close();
}
