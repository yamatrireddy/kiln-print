// Prints an HTML invoice rendered by the agent's sandboxed headless browser.
// Barcodes/QR codes are inline SVG: JavaScript and network access are disabled.
//
//   (cd sdk/typescript && npm install && npm run build)
//   KILN_TOKEN=... KILN_PRINTER="Mock Office Laser" node examples/html/print-html.mjs
import { KilnError, PrintClient } from "../../sdk/typescript/dist/index.js";

const html = /* html */ `<!doctype html>
<html><head><style>
  @page { size: A4; margin: 15mm; }
  body { font-family: "Segoe UI", Arial, sans-serif; color: #222; }
  h1 { color: #0b4f8a; margin: 0 0 4mm; }
  table { width: 100%; border-collapse: collapse; margin-top: 6mm; }
  th, td { border-bottom: 1px solid #ccc; padding: 2mm; text-align: left; }
  td.num, th.num { text-align: right; }
  .terms { break-before: page; }
</style></head><body>
  <h1>Invoice 2026-0042</h1>
  <div>Kiln Print Demo Supplies · 25 Sep 2026</div>
  <svg width="220" height="50" aria-label="barcode">
    ${Array.from({ length: 40 }, (_, i) => `<rect x="${i * 5}" y="0" width="${(i * 7) % 3 + 1}" height="40"/>`).join("")}
  </svg>
  <table>
    <tr><th>Item</th><th class="num">Qty</th><th class="num">Total</th></tr>
    <tr><td>Thermal labels 100x150</td><td class="num">2</td><td class="num">25.00</td></tr>
    <tr><td>Ribbon wax/resin 110mm</td><td class="num">1</td><td class="num">18.90</td></tr>
    <tr><th>Total</th><td></td><th class="num">43.90</th></tr>
  </table>
  <div class="terms"><h2>Terms</h2><p>Payable within 30 days.</p></div>
</body></html>`;

const client = new PrintClient({ token: process.env.KILN_TOKEN, appName: "html-example" });
await client.connect();
try {
  const printer = process.env.KILN_PRINTER ?? (await client.getDefaultPrinter())?.name;
  const job = await client.printHtml({
    printer,
    html,
    jobName: "Invoice 2026-0042",
    options: {
      footerHtml: `<div style="font-size:8px;width:100%;text-align:center">Page <span class="pageNumber"></span> of <span class="totalPages"></span></div>`,
    },
  });
  const done = await client.waitForJob(job.jobId);
  console.log(`${printer}: ${done.status}`, done.completion ?? done.error?.message);
} catch (error) {
  if (!(error instanceof KilnError)) throw error;
  console.error(String(error));
  process.exitCode = 1;
} finally {
  await client.disconnect();
}
