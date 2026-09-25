# Wire protocol v1

JSON over WebSocket (`/v1/ws`) and REST (`/v1/*`) on the agent's loopback listener (default `127.0.0.1:18731`). Phase 4 adds TLS (`wss://localhost:18731`). The message shapes will not change.

## Versioning

- Every envelope carries `protocolVersion`. The current and only version is `1`.
- The client lists the versions it supports in `session.hello`. The agent chooses the highest mutual version, or replies `UNSUPPORTED_PROTOCOL_VERSION` with `details.supported`.
- Within a major version, changes are additive only: new methods, new optional fields, new events, new error codes. Clients must ignore unknown fields and events. Request parameters are strict (`deny_unknown_fields`), so a misspelt option fails loudly instead of printing with defaults.

## Envelopes

```jsonc
// client → agent
{ "protocolVersion": 1, "type": "request", "id": "r-42", "method": "print.raw", "params": { … } }

// agent → client (exactly one per request id)
{ "protocolVersion": 1, "type": "response", "id": "r-42", "ok": true,  "result": { … } }
{ "protocolVersion": 1, "type": "response", "id": "r-42", "ok": false, "error": { "errorCode": "PRINTER_NOT_FOUND", … } }

// agent → client (unsolicited)
{ "protocolVersion": 1, "type": "event", "event": "job.completed", "seq": 17,
  "timestamp": "2026-09-25T14:00:00.8Z", "data": { …job… } }
```

- `id` is 1–128 characters and unique within a session. A repeated id is rejected (`INVALID_PAYLOAD`), which guards against replayed frames.
- `seq` increases by one per event within a session. A gap, or a `session.lagged` event with `{ missedEvents }`, means events were dropped for a slow consumer. Resynchronise with `jobs.get` or `jobs.list`.
- Requests on one connection are processed concurrently (up to 16 in flight). Responses may arrive out of order, so match them by `id`.
- **Ordering caveat:** a job's `job.created` and `job.queued` events can arrive *before* the response that returns its `jobId`. Buffer unknown-job events briefly or reconcile with `jobs.get`.
- Only text frames are accepted. Binary frames get an `INVALID_PAYLOAD` response.

## Handshake

The first message must be `session.hello` and must arrive within `server.handshake_timeout_secs` (default 10 s). Anything else, a bad token, a disallowed origin or a version mismatch gets an error response, and the connection is closed with code 1008.

```jsonc
{ "protocolVersion": 1, "type": "request", "id": "1", "method": "session.hello", "params": {
    "protocolVersions": [1],
    "client": { "name": "Lab Application", "version": "4.2.0" },
    "auth": { "type": "token", "token": "kiln_…" } } }
```

Result:

```jsonc
{
  "protocolVersion": 1,
  "sessionId": "8f0c…",
  "agent": { "name": "kiln-agent", "version": "0.1.0" },
  "client": { "clientId": "lab", "name": "Lab Application", "kind": "CLIENT",
              "permissions": ["printers.read", "print", "jobs.read"], "printers": ["Zebra ZD421"],
              "origin": "https://lab.example.com" },
  "limits": { "maxDocumentBytes": 67108864, "maxMessageBytes": 89544024, "maxCopies": 999 },
  "features": { "documentTypes": ["RAW", "TEXT"], "languages": [ { "id": "ZPL", … }, … ] },
  "heartbeatSeconds": 30
}
```

The agent pings every `heartbeatSeconds` and closes connections that are silent for three intervals.

## Methods

| Method | Permission | Params | Result |
|---|---|---|---|
| `session.ping` | — | — | `{ time }` |
| `printers.list` | `printers.read` | — | `Printer[]` (only printers in the client's scope) |
| `printers.default` | `printers.read` | — | `Printer \| null` |
| `printers.get` | `printers.read` | `printerId` or `printer` (name) | `Printer` with `capabilities` |
| `printers.capabilities` | `printers.read` | `printerId` or `printer` | `PrinterCapabilities` |
| `print.submit` | `print` | `type` (`RAW`, `TEXT`, `PDF`, `IMAGE`, `HTML`, `LABEL`, `RECEIPT`, `DOT_MATRIX`) plus that type's fields | `Job` |
| `print.raw` | `print` | see below | `Job` |
| `print.text` | `print` | see below | `Job` |
| `print.pdf` | `print` | see below | `Job` |
| `print.image` | `print` | see below | `Job` |
| `print.html` | `print` | see below | `Job` |
| `print.label` | `print` | see below | `Job` |
| `print.receipt` | `print` | see below | `Job` |
| `print.dotmatrix` | `print` | see below | `Job` |
| `jobs.list` | `jobs.read` | `status` (string, comma list or array), `printerId`, `clientId`\*, `since`, `until`, `limit` (≤ 500), `offset` | `Job[]`, newest first |
| `jobs.get` | `jobs.read` | `jobId` | `Job` |
| `jobs.cancel` | `jobs.cancel` (own), `jobs.cancel.all` | `jobId` | `Job` |
| `queue.list` | `queue.read` | — | `QueueSummary[]` |
| `queue.get` | `queue.read` | `printerId` or `printer` | `{ agentQueue: Job[], spoolerQueue: QueueEntry[] }` |
| `clients.list` | `clients.read` | — | configured clients with live sessions |

\* `clientId` is honoured only with `jobs.read.all`. Other clients always see only their own jobs. A job that exists but belongs to someone else is reported as `JOB_NOT_FOUND`, and an out-of-scope printer as `PRINTER_NOT_FOUND`.

### `print.raw`

```jsonc
{
  "printerId": "windows-5c1e…",     // or "printer": "Zebra ZD421"  (exactly one)
  "data": "XlhBXkZPNTAsNTBeRkRIaV5GU15YWg==",
  "encoding": "base64",              // base64 (default, alias "binary") | hex | utf8 | latin1
  "language": "ZPL",                 // optional: RAW, ZPL, EPL, CPCL, TSPL, ESC/POS, ESC/P (+ aliases)
  "copies": 1,                       // 1..maxCopies; RAW copies = payload written N times in one spool job
  "jobName": "Order 1001 label",     // shown in the OS queue; control characters stripped
  "idempotencyKey": "order-1001/label-1"
}
```

`idempotencyKey` (1–128 printable ASCII characters, scoped per client) names exactly one job, permanently, for as long as that job's record is retained. Resubmitting with the same key returns the original job in whatever state it is in, including `FAILED`. To try again after a failure, use a new key. A reused key never prints twice.

Bytes reach the device exactly as decoded. `language` only selects inspection. Warnings (for example "no ^XZ format end") are returned in `job.warnings`, or rejected as errors when `jobs.strict_languages` is on.

### `print.text`

```jsonc
{
  "printer": "Epson LQ-590",
  "text": "Line 1\nLine 2\fPage 2",
  "options": {
    "mode": "RAW",                   // RENDERED (default, driver/GDI) | RAW (encoded bytes)
    "encoding": "ibm437",            // RAW: utf-8, ibm437, windows-1252, iso-8859-x, shift_jis, …
    "lineEnding": "CRLF",            // RAW: CRLF | LF | CR
    "formFeed": true,                // RAW: append FF (eject / next top-of-form)
    "fontFamily": "Courier New",     // RENDERED
    "fontSize": 10, "bold": false,   // RENDERED
    "alignment": "LEFT",             // RENDERED: LEFT | CENTER | RIGHT
    "marginsMm": { "top": 10, "right": 10, "bottom": 10, "left": 10 },
    "orientation": "PORTRAIT",       // RENDERED
    "wrap": true, "tabWidth": 8
  }
}
```

RAW text encoding is strict. A character the target encoding cannot represent is an `INVALID_PAYLOAD` error, never a silent `?`.

### Page setup (PDF, image, HTML)

These fields go inside `options`. Each is optional, and an unset field means "the printer's default". Values the printer does not support are rejected, never silently replaced.

| Field | Values |
|---|---|
| `paperSize` | a name (`"A4"`, `"Letter"`), a driver paper id from `capabilities.paperSizes`, or `{ "widthMm": 100, "heightMm": 150 }` |
| `orientation` | `PORTRAIT` \| `LANDSCAPE`. Unset follows the content: landscape pages print landscape. |
| `marginsMm` | `{ top, right, bottom, left }`. Unset uses the printer's minimum margins. |
| `duplex` | `SIMPLEX` \| `LONG_EDGE` \| `SHORT_EDGE` |
| `color` | `COLOR` \| `MONOCHROME` |
| `tray` | a tray id or name from `capabilities.trays` |

### Document sources (PDF, image)

Give exactly one of:

- `data` plus `encoding` (default `base64`);
- `path`: an absolute local file path, honoured only under `sources.allowed_paths`;
- `url`: honoured only under `sources.allowed_url_prefixes`, with no redirects and within the size limit.

Both allow-lists are empty by default, so path and URL sources are refused with `ACCESS_DENIED`. The same error is returned whether a file is missing or outside the allowed folders, so the answer never reveals whether a file exists.

### `print.pdf`

```jsonc
{
  "printer": "HP LaserJet",
  "data": "JVBERi0xLjcK…",                // or "path" / "url"
  "copies": 2,
  "options": {
    "pageRange": "1-3,5,8-",              // printed in the order given
    "scale": "SHRINK_TO_FIT",             // FIT | SHRINK_TO_FIT (default) | ACTUAL_SIZE | percentage
    "dpi": 300,                           // rasterisation cap on Windows (72-1200)
    "paperSize": "A4", "duplex": "LONG_EDGE", "color": "MONOCHROME"
  }
}
```

No viewer is opened. On Windows, pages are rendered by the OS PDF engine at their printed size and sent through the driver. See [ADR 0004](adr/0004-document-rendering.md).

### `print.image`

```jsonc
{
  "printer": "Label Printer (driver)",
  "data": "iVBORw0KGgo…",                 // PNG, JPEG, BMP, TIFF (first page), GIF (first frame)
  "options": {
    "fit": "FIT",                         // ORIGINAL | FIT (default) | SHRINK_TO_FIT | FILL
    "scale": 50,                          // % of original size; overrides fit
    "rotate": 90,                         // 0 | 90 | 180 | 270, clockwise
    "dpi": 203,                           // image resolution for ORIGINAL/scale (default: file metadata, else 96)
    "align": "CENTER",                    // CENTER | TOP_LEFT
    "paperSize": { "widthMm": 100, "heightMm": 150 }
  }
}
```

Transparency prints as white paper. Images larger than 80 megapixels are refused with `PAYLOAD_TOO_LARGE` before decoding.

### `print.html`

```jsonc
{
  "printer": "Front Desk",
  "html": "<!doctype html><style>@page { size: A5; margin: 12mm }</style><h1>Receipt</h1>…",
  "options": {
    "paperSize": "Letter",                // used unless the CSS @page size wins (preferCssPageSize, default true)
    "marginsMm": { "top": 10, "right": 10, "bottom": 10, "left": 10 },
    "scale": 100,                         // layout zoom, 10-200 %
    "printBackground": true,
    "pageRange": "1-2",
    "headerHtml": "<div style='font-size:8px'><span class='title'></span></div>",
    "footerHtml": "<div style='font-size:8px'>Page <span class='pageNumber'></span>/<span class='totalPages'></span></div>"
  }
}
```

HTML is rendered by a sandboxed headless Edge, Chrome or Chromium. **JavaScript is disabled, and no network request of any kind leaves the renderer**, including to localhost. Supply barcodes and QR codes as inline SVG or `data:` images, and fonts as `data:` URIs or system fonts. Without paper settings, the printer's default paper is used. If no browser is installed, HTML is reported as unsupported.

### `print.label`

The label is described once and encoded by the agent as ZPL, EPL, TSPL or CPCL ([ADR 0005](adr/0005-label-receipt-dot-matrix-and-direct-tcp.md)).

```jsonc
{
  "printer": "Dock Zebra",
  "copies": 2,
  "label": {
    "widthMm": 100, "heightMm": 150, "dpi": 203,
    "language": "ZPL",                    // optional; defaults to the printer's `language` hint
    "gapMm": 3, "darkness": 20, "speed": 4,
    "elements": [
      { "type": "TEXT", "xMm": 5, "yMm": 5, "text": "Ship to: Jane", "heightMm": 5, "rotation": 0 },
      { "type": "BARCODE", "xMm": 5, "yMm": 20, "symbology": "CODE128", "data": "1Z999AA1",
        "heightMm": 20, "moduleWidth": 2, "humanReadable": true },
      { "type": "QR", "xMm": 60, "yMm": 60, "data": "https://…", "magnification": 6, "errorCorrection": "M" },
      { "type": "DATA_MATRIX", "xMm": 5, "yMm": 60, "data": "LOT42", "moduleSize": 4 },
      { "type": "BOX", "xMm": 2, "yMm": 2, "widthMm": 96, "heightMm": 146, "thicknessMm": 0.5 },
      { "type": "RAW", "data": "^FO10,10^GB50,50,50^FS" }   // verbatim, in the label's language
    ]
  }
}
```

Symbologies are `CODE128`, `CODE39`, `EAN13`, `EAN8`, `UPC_A` and `ITF`. Data is validated per symbology, and EAN/UPC check digits are computed by the printer. Client text is escaped for each language and cannot inject commands. Elements a language cannot express are refused with `UNSUPPORTED_OPERATION`: Data Matrix on EPL/CPCL, and 180°/270° barcodes on CPCL.

### `print.receipt`

```jsonc
{
  "printer": "Front Counter",
  "receipt": {
    "widthChars": 48,                     // 48 for 80 mm, 32 for 58 mm
    "codePage": "ibm858",                 // ibm437 (default), ibm850, ibm858, windows-1252, ibm866
    "cut": true, "openDrawer": false,
    "items": [
      { "type": "TEXT", "text": "KILN CAFE", "align": "CENTER", "bold": true, "doubleWidth": true, "doubleHeight": true },
      { "type": "COLUMNS", "left": "Latte", "right": "3.80 €" },
      { "type": "SEPARATOR", "character": "=" },
      { "type": "BARCODE", "symbology": "EAN13", "data": "400638133393", "heightDots": 80 },
      { "type": "QR", "data": "https://…", "size": 6, "align": "CENTER" },
      { "type": "IMAGE", "data": "<base64 PNG>", "align": "CENTER" },   // dithered to 1-bit
      { "type": "FEED", "lines": 2 },
      { "type": "CUT", "partial": true, "feedLines": 3 },
      { "type": "DRAWER", "pin": 0 },
      { "type": "RAW", "data": "<base64 ESC/POS>" }
    ]
  }
}
```

### `print.dotmatrix`

ESC/P text for impact printers, sent as RAW bytes. It is never rasterised.

```jsonc
{
  "printer": "Epson LQ-590",
  "copies": 2,
  "document": {
    "cpi": 10,                            // 10, 12, 15, 17, 20
    "lpi": 6, "pins": 24,                 // 6 and 8 are native; others use n/180" (24-pin) or n/216" (9-pin)
    "quality": "NLQ",                     // DRAFT | NLQ
    "formLengthInches": 11,               // or formLengthLines
    "skipPerforationLines": 3,            // continuous paper
    "leftMargin": 5, "rightMargin": 80,
    "encoding": "ibm437", "characterTable": 1,
    "initialize": true, "formFeed": true,
    "lines": [
      "plain text line",
      { "type": "LINE", "text": "TOTAL 43.90", "bold": true, "condensed": false, "doubleWidth": false,
        "underline": false, "italic": false, "doubleStrike": false },
      { "type": "LINE_FEED", "lines": 2 },
      { "type": "FORM_FEED" },
      { "type": "RAW", "data": "<base64 escape sequence>" }
    ]
  }
}
```

### Direct network printers

Printers listed under `[[network_printers]]` in `agent.toml` appear in `printers.list` with `type: "NETWORK"`, `port: "tcp://host:9100"` and their configured `language`. They accept RAW-producing documents (`RAW`, `TEXT` in RAW mode, `LABEL`, `RECEIPT`, `DOT_MATRIX`). A job completes with `delivery: DEVICE_DELIVERED` and `completion: BYTES_DELIVERED` once every byte is written. With `status = "ZPL"` or `"ESC/POS"`, `online`, `status` and `conditions` come from the printer itself. Clients cannot address hosts that are not configured.

## Events

| Event | Payload | When |
|---|---|---|
| `job.created` | `Job` | request recorded (`RECEIVED`) |
| `job.queued` | `Job` | validated, rendered and placed in the printer's queue |
| `job.spooled` | `Job` | the OS spooler accepted it (`delivery = SPOOLER_ACCEPTED`) |
| `job.printing` | `Job` | the spooler reports printing |
| `job.updated` | `Job` | non-status change, e.g. `condition` became `PAPER_OUT` or cleared |
| `job.completed` / `job.failed` / `job.cancelled` | `Job` | terminal |
| `printer.connected` / `printer.disconnected` | `Printer` | discovery found or lost a printer |
| `printer.status.changed` | `Printer` | online, status, conditions or default changed |
| `session.lagged` | `{ missedEvents }` | this session missed events |

## Errors

`{ errorCode, message, jobId, printerId, recoverable, details }`. `message` is human-readable and never contains stack traces or file paths.

| Code | Meaning | REST |
|---|---|---|
| `AUTHENTICATION_REQUIRED` | no or invalid handshake / bearer token | 401 |
| `CLIENT_NOT_TRUSTED` | unknown credentials | 401 |
| `ACCESS_DENIED` | missing permission, printer outside scope, or origin not allowed | 403 |
| `PRINTER_NOT_FOUND`, `JOB_NOT_FOUND` | not found, or not visible to this client | 404 |
| `INVALID_PAYLOAD` | malformed JSON, unknown field, bad encoding, bad option | 400 |
| `INVALID_JOB_STATE` | e.g. cancelling a completed job | 409 |
| `PAYLOAD_TOO_LARGE` | document over `maxDocumentBytes` | 413 |
| `UNSUPPORTED_DOCUMENT`, `UNSUPPORTED_OPERATION`, `UNSUPPORTED_PROTOCOL_VERSION` | not available for this printer or agent | 422 |
| `RATE_LIMITED` | per-client rate or concurrency limit | 429 |
| `QUEUE_FULL`, `PRINTER_BUSY`, `PRINTER_OFFLINE`, `PAPER_OUT`, `PAPER_JAM` | printer or queue condition | 503 |
| `SPOOLER_ERROR`, `PRINT_FAILED`, `CONNECTION_ERROR` | OS or device failure | 502 |
| `TIMEOUT` | submission watchdog expired (outcome unknown) | 504 |
| `INTERNAL_ERROR` | bug; details are in the agent log | 500 |

`recoverable: true` means the condition can clear. It does **not** mean an automatic retry is safe. Check `details.outcome` (`NOT_PRINTED` or `UNKNOWN`) and see [ADR 0003](adr/0003-no-automatic-print-retries.md).

A request that fails validation still creates a job (status `FAILED`), and the error carries its `jobId`. Every request is auditable.

## REST mapping

All REST routes require `Authorization: Bearer <token>`, and responses use the same `{ protocolVersion, ok, result | error }` body. No CORS headers are sent, so browser apps must use the WebSocket API.

| REST | Equivalent |
|---|---|
| `GET /v1/health` (no auth) | liveness: `{ status, protocolVersions }` |
| `GET /v1/printers` · `/v1/printers/default` · `/v1/printers/{id}` · `/v1/printers/{id}/capabilities` | `printers.*` |
| `POST /v1/print` · `/v1/print/raw` · `/text` · `/pdf` · `/image` · `/html` · `/label` · `/receipt` · `/dotmatrix` → **202** | `print.*` |
| `GET /v1/jobs?status=&printerId=&clientId=&since=&until=&limit=&offset=` | `jobs.list` |
| `GET /v1/jobs/{id}` · `DELETE /v1/jobs/{id}` | `jobs.get` · `jobs.cancel` |
| `GET /v1/queue` · `/v1/queue/{printerId}` | `queue.list` · `queue.get` |
| `GET /v1/clients` | `clients.list` |
| `GET /v1/audit?limit=` | audit log (local administrator only) |
