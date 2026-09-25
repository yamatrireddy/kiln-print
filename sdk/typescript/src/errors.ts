import type { ErrorCode, ErrorPayload } from "./types.ts";

/** Whether resubmitting could print twice. */
export type Outcome = "NOT_PRINTED" | "UNKNOWN";

/**
 * Every failure surfaced by the SDK. `recoverable` means the condition can clear; it does
 * not mean blind retries are safe. Check `outcome`, and retry prints with the same
 * `idempotencyKey` so the agent can deduplicate.
 */
export class KilnError extends Error {
  readonly code: ErrorCode;
  readonly jobId: string | null;
  readonly printerId: string | null;
  readonly recoverable: boolean;
  readonly details: Record<string, unknown> | null;
  /** For print calls: the key to resubmit with so a retry cannot duplicate output. */
  readonly idempotencyKey: string | null;

  constructor(payload: ErrorPayload, idempotencyKey: string | null = null) {
    super(payload.message);
    this.name = "KilnError";
    this.code = payload.errorCode;
    this.jobId = payload.jobId;
    this.printerId = payload.printerId;
    this.recoverable = payload.recoverable;
    this.details = payload.details;
    this.idempotencyKey = idempotencyKey;
  }

  override toString(): string {
    return `KilnError [${this.code}]: ${this.message}`;
  }

  /** `NOT_PRINTED` or `UNKNOWN` when the agent or SDK knows; otherwise undefined. */
  get outcome(): Outcome | undefined {
    const value = this.details?.["outcome"];
    return value === "NOT_PRINTED" || value === "UNKNOWN" ? value : undefined;
  }

  static local(
    code: ErrorCode,
    message: string,
    options: { recoverable?: boolean; outcome?: Outcome; idempotencyKey?: string | null } = {},
  ): KilnError {
    return new KilnError(
      {
        errorCode: code,
        message,
        jobId: null,
        printerId: null,
        recoverable: options.recoverable ?? true,
        details: options.outcome ? { outcome: options.outcome } : null,
      },
      options.idempotencyKey ?? null,
    );
  }
}

/** Errors that reconnecting cannot fix. */
export const FATAL_CONNECT_ERRORS: readonly ErrorCode[] = [
  "CLIENT_NOT_TRUSTED",
  "ACCESS_DENIED",
  "AUTHENTICATION_REQUIRED",
  "UNSUPPORTED_PROTOCOL_VERSION",
];
