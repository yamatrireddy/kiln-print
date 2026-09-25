// A shipping label described once and encoded by the agent for whatever the printer
// speaks: ZPL (Zebra), EPL, TSPL (TSC) or CPCL. The language comes from the printer's
// hint, or set label.language explicitly.
//
//   (cd sdk/typescript && npm install && npm run build)
//   KILN_TOKEN=... KILN_PRINTER="Mock Zebra ZD421" node examples/barcode/print-label.mjs
import { KilnError, PrintClient } from "../../sdk/typescript/dist/index.js";

const label = {
  widthMm: 100,
  heightMm: 150,
  dpi: 203,
  elements: [
    { type: "TEXT", xMm: 5, yMm: 5, text: "Kiln Print Demo Shipping", heightMm: 5 },
    { type: "BOX", xMm: 4, yMm: 12, widthMm: 92, heightMm: 0.5, thicknessMm: 0.5 },
    { type: "TEXT", xMm: 5, yMm: 16, text: "Ship to: Jane Example", heightMm: 4 },
    { type: "TEXT", xMm: 5, yMm: 22, text: "42 Sample Street, 12345 Exampletown", heightMm: 3 },
    { type: "BARCODE", xMm: 5, yMm: 32, symbology: "CODE128", data: "1Z999AA10123456784", heightMm: 20 },
    { type: "QR", xMm: 60, yMm: 60, data: "https://example.com/track/1Z999AA10123456784", magnification: 6 },
  ],
};

const client = new PrintClient({ token: process.env.KILN_TOKEN, appName: "label-example" });
await client.connect();
try {
  const printer = process.env.KILN_PRINTER ?? "Mock Zebra ZD421";
  const job = await client.printLabel({ printer, label, copies: 1, jobName: "Shipping label 1Z999" });
  const done = await client.waitForJob(job.jobId);
  console.log(`${printer}: ${done.status} as ${done.language} (${done.completion ?? done.error?.message})`);
} catch (error) {
  if (!(error instanceof KilnError)) throw error;
  console.error(String(error));
  process.exitCode = 1;
} finally {
  await client.disconnect();
}
