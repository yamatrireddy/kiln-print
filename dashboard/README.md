# Dashboard (Phase 5)

A static web app served by the agent at `http://localhost:18731/` and authenticated with the local admin session. It will show:

- **Connected applications:** `GET /v1/clients` (client id, name, origin, connected time, last activity, permissions, allowed printers, silent-print permission) with Revoke (Phase 4 trust store).
- **Printers:** `GET /v1/printers` plus `printer.*` events (default printer, status, conditions).
- **Jobs:** `GET /v1/jobs` with filters for application (`clientId`), printer, status and date/time (`since`/`until`), and live `job.*` events. Each job shows its id, application, printer, document type, submitted time, status, delivery stage, copies and error details. Cancel uses `DELETE /v1/jobs/{id}`.
- **Queues:** `GET /v1/queue` and `/v1/queue/{printerId}`, covering both the agent queue and the OS spooler queue.
- **Audit:** `GET /v1/audit`.

Every API it needs already exists in Phase 1.
