// Prints a PDF silently (no viewer) with page setup, using the TypeScript SDK.
//
//   (cd sdk/typescript && npm install && npm run build)
//   KILN_TOKEN=... node examples/pdf/print-pdf.mjs invoice.pdf "HP LaserJet" "1-2"
import { readFile } from "node:fs/promises";
import { PrintClient, KilnError } from "../../sdk/typescript/dist/index.js";

const [file, printer = process.env.KILN_PRINTER, pageRange] = process.argv.slice(2);
if (!file || !printer) {
  console.error("usage: node examples/pdf/print-pdf.mjs <file.pdf> <printer> [pageRange]");
  process.exit(2);
}

const client = new PrintClient({ token: process.env.KILN_TOKEN, appName: "pdf-example" });
await client.connect();
try {
  const job = await client.printPdf({
    printer,
    data: await readFile(file),
    copies: 1,
    jobName: file.split(/[\\/]/).pop(),
    options: { pageRange, scale: "SHRINK_TO_FIT", duplex: "LONG_EDGE" },
  });
  console.log(`job ${job.jobId} ${job.status}/${job.delivery}`);
  client.onJobStatus(job.jobId, (j) => console.log(`  ${j.status.padEnd(10)} ${j.delivery}${j.condition ? ` (${j.condition})` : ""}`));
  const done = await client.waitForJob(job.jobId);
  console.log(`finished: ${done.status}`, done.completion ?? done.error?.message);
} catch (error) {
  if (error instanceof KilnError) console.error(String(error));
  else throw error;
  process.exitCode = 1;
} finally {
  await client.disconnect();
}
