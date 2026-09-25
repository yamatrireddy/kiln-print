// Sends any file to a printer byte-for-byte (ZPL, EPL, TSPL, CPCL, ESC/POS, PCL, …).
//
//   KILN_TOKEN=... node examples/raw/print-file.mjs <file> <printer name or id> [language]
import { readFile } from "node:fs/promises";
import { connect } from "../lib/kiln-client.mjs";

const [file, printer, language] = process.argv.slice(2);
if (!file || !printer) {
  console.error("usage: node examples/raw/print-file.mjs <file> <printer> [language]");
  process.exit(2);
}

const data = await readFile(file);
const client = await connect({ name: "raw-file-example" });
try {
  const job = await client.call("print.raw", {
    printer,
    language,
    encoding: "base64",
    data: data.toString("base64"),
    jobName: file.split(/[\\/]/).pop(),
  });
  console.log(`job ${job.jobId}: ${data.length} bytes queued`);
  const done = await client.waitForJob(job.jobId);
  console.log(`finished: ${done.status}`, done.completion ?? done.error);
  process.exitCode = done.status === "COMPLETED" ? 0 : 1;
} catch (err) {
  console.error(err.message);
  process.exitCode = 1;
} finally {
  client.close();
}
