// Prints plain text two ways:
//  * RENDERED — laid out by the printer driver (font, size, alignment, margins);
//  * RAW      — encoded bytes for dot-matrix / receipt printers (resident font).
//
//   KILN_TOKEN=... KILN_PRINTER="Microsoft Print to PDF" node examples/text/print-text.mjs
import { connect, choosePrinter } from "../lib/kiln-client.mjs";

const client = await connect({ name: "text-example" });
try {
  const printer = await choosePrinter(client, (p) => p.default);
  const caps = await client.call("printers.capabilities", { printerId: printer.id });
  console.log(`Printer: ${printer.name} — accepts ${caps.documentTypes.join(", ")}; raw=${caps.raw}`);

  const text = [
    "Kiln Print text example",
    "",
    "Item\tQty\tPrice",
    "Labels\t2\t25.00",
    "Ribbon\t1\t18.90",
    "",
    "Long lines wrap at word boundaries when RENDERED mode lays the text out for the page width.",
  ].join("\n");

  const mode = caps.raw === false ? "RENDERED" : process.env.KILN_TEXT_MODE ?? "RENDERED";
  const job = await client.call("print.text", {
    printerId: printer.id,
    text,
    options:
      mode === "RAW"
        ? { mode: "RAW", encoding: "ibm437", lineEnding: "CRLF", formFeed: true }
        : { mode: "RENDERED", fontFamily: "Consolas", fontSize: 11, marginsMm: { top: 15, right: 15, bottom: 15, left: 20 } },
    jobName: "Kiln text example",
  });
  console.log(`job ${job.jobId} accepted in ${mode} mode`);
  const done = await client.waitForJob(job.jobId);
  console.log(`finished: ${done.status}`, done.completion ?? done.error);
} finally {
  client.close();
}
