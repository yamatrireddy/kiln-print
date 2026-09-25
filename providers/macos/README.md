# MacPrintProvider (Phase 6)

macOS printing is built on CUPS. RAW, PDF, status and cancellation reuse the CUPS/IPP provider (see `../linux/README.md`). A thin macOS layer adds:

- default-printer and printer-name resolution consistent with System Settings (`PMServerCreatePrinterList`, `PMPrinterGetName`);
- native rendering where CUPS filters are insufficient (`PMPrintSession` with a Core Graphics PDF context);
- packaging as a signed and notarised LaunchAgent, never a LaunchDaemon (see ADR 0002).
