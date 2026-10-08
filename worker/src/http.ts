/** HTTP helpers: JSON responses, a typed error, and a size-limited body reader. */

/** Maximum accepted request body, in bytes (16 KiB). */
export const MAX_BODY_BYTES = 16 * 1024;

/**
 * An error that maps directly onto an HTTP response.
 * Handlers throw it; the router turns it into `{ error, message }` JSON.
 */
export class HttpError extends Error {
  readonly status: number;
  readonly code: string;
  readonly extra: Record<string, unknown>;
  readonly headers: Record<string, string>;

  /**
   * @param status HTTP status code.
   * @param code Stable machine-readable error code.
   * @param message Human-readable explanation. Must never contain secrets.
   * @param extra Additional JSON fields merged into the response body.
   * @param headers Additional response headers (for example `Allow` or `Retry-After`).
   */
  constructor(
    status: number,
    code: string,
    message: string,
    extra: Record<string, unknown> = {},
    headers: Record<string, string> = {},
  ) {
    super(message);
    this.name = "HttpError";
    this.status = status;
    this.code = code;
    this.extra = extra;
    this.headers = headers;
  }
}

/** Builds a JSON response with `Cache-Control: no-store` and `X-Content-Type-Options: nosniff`. */
export function jsonResponse(
  status: number,
  body: unknown,
  headers: Record<string, string> = {},
): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: {
      "content-type": "application/json; charset=utf-8",
      "cache-control": "no-store",
      "x-content-type-options": "nosniff",
      ...headers,
    },
  });
}

/** Converts an {@link HttpError} into its JSON response. */
export function errorResponse(error: HttpError): Response {
  return jsonResponse(
    error.status,
    { ...error.extra, error: error.code, message: error.message },
    error.headers,
  );
}

/**
 * Reads the whole request body, refusing anything above `maxBytes` with a 413.
 * The `Content-Length` header is checked first, but the stream is also counted because the
 * header can be absent or wrong.
 */
export async function readBody(request: Request, maxBytes: number): Promise<Uint8Array> {
  const declared = request.headers.get("content-length");
  if (declared !== null) {
    const length = Number(declared);
    if (Number.isFinite(length) && length > maxBytes) {
      throw new HttpError(413, "payload_too_large", `request body exceeds ${maxBytes} bytes`);
    }
  }
  if (request.body === null) {
    return new Uint8Array(0);
  }
  const reader = request.body.getReader();
  const chunks: Uint8Array[] = [];
  let total = 0;
  for (;;) {
    const { done, value } = await reader.read();
    if (done) {
      break;
    }
    total += value.byteLength;
    if (total > maxBytes) {
      await reader.cancel().catch(() => undefined);
      throw new HttpError(413, "payload_too_large", `request body exceeds ${maxBytes} bytes`);
    }
    chunks.push(value);
  }
  const body = new Uint8Array(total);
  let offset = 0;
  for (const chunk of chunks) {
    body.set(chunk, offset);
    offset += chunk.byteLength;
  }
  return body;
}

/** Parses a request body as a JSON object (not an array, not `null`). */
export function parseJsonObject(body: Uint8Array): Record<string, unknown> {
  let parsed: unknown;
  try {
    parsed = JSON.parse(new TextDecoder("utf-8", { fatal: true, ignoreBOM: false }).decode(body));
  } catch {
    throw new HttpError(400, "invalid_json", "request body must be valid UTF-8 JSON");
  }
  if (typeof parsed !== "object" || parsed === null || Array.isArray(parsed)) {
    throw new HttpError(400, "invalid_request", "request body must be a JSON object");
  }
  return parsed as Record<string, unknown>;
}
