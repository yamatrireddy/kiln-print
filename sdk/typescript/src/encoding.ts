import type { BinaryInput } from "./types.ts";

const CHUNK = 0x8000;

/** Base64-encodes bytes in browsers and Node without depending on `Buffer`. */
export function bytesToBase64(bytes: Uint8Array): string {
  let binary = "";
  for (let i = 0; i < bytes.length; i += CHUNK) {
    binary += String.fromCharCode(...bytes.subarray(i, i + CHUNK));
  }
  return btoa(binary);
}

/**
 * Normalises binary input to a base64 string. Strings are assumed to already be base64.
 */
export async function toBase64(input: BinaryInput): Promise<string> {
  if (typeof input === "string") return input;
  if (input instanceof Uint8Array) return bytesToBase64(input);
  if (input instanceof ArrayBuffer) return bytesToBase64(new Uint8Array(input));
  if (typeof Blob !== "undefined" && input instanceof Blob) {
    return bytesToBase64(new Uint8Array(await input.arrayBuffer()));
  }
  throw new TypeError("expected Uint8Array, ArrayBuffer, Blob or base64 string");
}

/** RFC 4122 v4 identifier, used for idempotency keys and request ids. */
export function randomId(): string {
  const cryptoApi = globalThis.crypto;
  if (cryptoApi?.randomUUID) return cryptoApi.randomUUID();
  const bytes = new Uint8Array(16);
  cryptoApi.getRandomValues(bytes);
  bytes[6] = (bytes[6]! & 0x0f) | 0x40;
  bytes[8] = (bytes[8]! & 0x3f) | 0x80;
  const hex = [...bytes].map((b) => b.toString(16).padStart(2, "0")).join("");
  return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20)}`;
}
