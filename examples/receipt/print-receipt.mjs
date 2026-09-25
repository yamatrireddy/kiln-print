// An ESC/POS receipt: centred header, item columns, QR code, cut and cash drawer.
//
//   (cd sdk/typescript && npm install && npm run build)
//   KILN_TOKEN=... KILN_PRINTER="Mock ESC/POS Receipt" node examples/receipt/print-receipt.mjs
import { KilnError, PrintClient } from "../../sdk/typescript/dist/index.js";

const items = [
  ["Latte large", 3.8],
  ["Croissant", 2.2],
  ["Orange juice", 3.5],
];
const total = items.reduce((sum, [, price]) => sum + price, 0);
const money = (n) => `${n.toFixed(2)} €`;

const receipt = {
  widthChars: 48, // 80 mm paper; use 32 for 58 mm
  codePage: "ibm858", // has the euro sign
  openDrawer: true,
  items: [
    { type: "TEXT", text: "KILN CAFE", align: "CENTER", bold: true, doubleWidth: true, doubleHeight: true },
    { type: "TEXT", text: "42 Sample Street · Exampletown", align: "CENTER", small: true },
    { type: "SEPARATOR", character: "=" },
    ...items.map(([name, price]) => ({ type: "COLUMNS", left: name, right: money(price) })),
    { type: "SEPARATOR" },
    { type: "COLUMNS", left: "TOTAL", right: money(total), bold: true },
    { type: "FEED", lines: 1 },
    { type: "QR", data: "https://example.com/r/2026-0042", size: 6, align: "CENTER" },
    { type: "TEXT", text: "Thank you!", align: "CENTER" },
    { type: "CUT", partial: true, feedLines: 4 },
  ],
};

const client = new PrintClient({ token: process.env.KILN_TOKEN, appName: "receipt-example" });
await client.connect();
try {
  const printer = process.env.KILN_PRINTER ?? "Mock ESC/POS Receipt";
  const job = await client.printReceipt({ printer, receipt, jobName: "Receipt 2026-0042" });
  const done = await client.waitForJob(job.jobId);
  console.log(`${printer}: ${done.status} (${done.completion ?? done.error?.message})`);
} catch (error) {
  if (!(error instanceof KilnError)) throw error;
  console.error(String(error));
  process.exitCode = 1;
} finally {
  await client.disconnect();
}
