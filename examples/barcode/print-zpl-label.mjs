// Prints a ZPL shipping label (Code 128 + QR) as RAW bytes.
//
//   KILN_TOKEN=... KILN_PRINTER="Zebra ZD421" node examples/barcode/print-zpl-label.mjs
//
// The ZPL is sent with encoding "utf8" (the JSON string's bytes) — convenient for
// text-based label languages. Binary payloads use encoding "base64".
import { readFile } from "node:fs/promises";
import { connect, choosePrinter } from "../lib/kiln-client.mjs";

const zpl = await readFile(new URL("./shipping-label.zpl", import.meta.url), "utf8");
const client = await connect({ name: "zpl-label-example" });
try {
  const printer = await choosePrinter(client, (p) => /zebra|zpl|label/i.test(p.name));
  console.log(`Printing to ${printer.name}`);
  const job = await client.call("print.raw", {
    printerId: printer.id,
    language: "ZPL",
    encoding: "utf8",
    data: zpl,
    jobName: "Shipping label 1Z999",
    // Resubmitting with the same key after a network hiccup never prints twice.
    idempotencyKey: `label-${Date.now()}`,
  });
  console.log(`job ${job.jobId} accepted (${job.status}/${job.delivery})`);
  if (job.warnings.length) console.log("warnings:", job.warnings);
  const done = await client.waitForJob(job.jobId);
  console.log(`finished: ${done.status}`, done.completion ?? done.error);
} finally {
  client.close();
}
