// Prints an image (PNG, JPEG, BMP, TIFF, GIF) with fit/rotation/DPI options.
//
//   (cd sdk/typescript && npm install && npm run build)
//   KILN_TOKEN=... node examples/image/print-image.mjs photo.jpg "Mock Office Laser" FIT
import { readFile } from "node:fs/promises";
import { KilnError, PrintClient } from "../../sdk/typescript/dist/index.js";

const [file, printer = process.env.KILN_PRINTER, fit = "FIT"] = process.argv.slice(2);
if (!file || !printer) {
  console.error("usage: node examples/image/print-image.mjs <image> <printer> [ORIGINAL|FIT|SHRINK_TO_FIT|FILL]");
  process.exit(2);
}

const client = new PrintClient({ token: process.env.KILN_TOKEN, appName: "image-example" });
await client.connect();
try {
  const job = await client.printImage({
    printer,
    data: await readFile(file),
    options: { fit, align: "CENTER", marginsMm: { top: 10, right: 10, bottom: 10, left: 10 } },
  });
  const done = await client.waitForJob(job.jobId);
  console.log(`finished: ${done.status}`, done.completion ?? done.error?.message);
} catch (error) {
  if (!(error instanceof KilnError)) throw error;
  console.error(String(error));
  process.exitCode = 1;
} finally {
  await client.disconnect();
}
