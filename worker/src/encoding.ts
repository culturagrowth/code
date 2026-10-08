/**
 * Small encoding helpers shared by the auth and validation code.
 *
 * Everything here uses only APIs that exist both in Cloudflare Workers and in Node 22
 * (WebCrypto, `atob`/`btoa`, `TextEncoder`), so it can be unit tested without a Workers runtime.
 */

const textEncoder = new TextEncoder();

/** Encodes a string as UTF-8. */
export function utf8(text: string): Uint8Array {
  return textEncoder.encode(text);
}

/** Lowercase hexadecimal encoding. */
export function toHex(bytes: Uint8Array): string {
  let out = "";
  for (const byte of bytes) {
    out += byte.toString(16).padStart(2, "0");
  }
  return out;
}

/** Standard base64 (RFC 4648 section 4, with padding). */
export function base64Encode(bytes: Uint8Array): string {
  let binary = "";
  for (const byte of bytes) {
    binary += String.fromCharCode(byte);
  }
  return btoa(binary);
}

const BASE64_SHAPE = /^[A-Za-z0-9+/]*={0,2}$/;

/**
 * Strict decoder for standard base64 with padding.
 *
 * Returns `null` for anything that is not the canonical encoding of some byte string: wrong
 * alphabet, missing padding, whitespace, or non-zero trailing bits. Strictness matters because
 * the encoded value (for example a signature) is also used as a replay-protection key, so two
 * different strings must never decode to the same bytes.
 */
export function base64Decode(text: string): Uint8Array | null {
  if (text.length === 0 || text.length % 4 !== 0 || !BASE64_SHAPE.test(text)) {
    return null;
  }
  let binary: string;
  try {
    binary = atob(text);
  } catch {
    return null;
  }
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i += 1) {
    bytes[i] = binary.charCodeAt(i);
  }
  return base64Encode(bytes) === text ? bytes : null;
}

/** SHA-256 digest of `data`, as lowercase hex. */
export async function sha256Hex(data: Uint8Array): Promise<string> {
  const digest = await crypto.subtle.digest("SHA-256", data);
  return toHex(new Uint8Array(digest));
}

/**
 * Compares two byte strings without an early exit on the first difference.
 * Lengths are not secret, so differing lengths return `false` immediately.
 */
export function timingSafeEqual(a: Uint8Array, b: Uint8Array): boolean {
  if (a.length !== b.length) {
    return false;
  }
  let diff = 0;
  for (let i = 0; i < a.length; i += 1) {
    diff |= (a[i] ?? 0) ^ (b[i] ?? 0);
  }
  return diff === 0;
}
