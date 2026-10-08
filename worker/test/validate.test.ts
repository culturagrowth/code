import { describe, expect, it } from "vitest";
import { HttpError, MAX_BODY_BYTES, parseJsonObject, readBody } from "../src/http.js";
import {
  parseClipRegistration,
  parseCreateCrew,
  parseDeviceRegistration,
  parseDownloadRequest,
  parseJoin,
  parseUploadRequest,
} from "../src/validate.js";

const U1 = "00000000-0000-0000-0000-000000000001";
const U2 = "00000000-0000-0000-0000-000000000002";
const KEY = btoa(String.fromCharCode(...new Uint8Array(32).fill(7)));
const MIB = 1024 * 1024;

function rejects(fn: () => unknown, status = 400, code?: string): void {
  try {
    fn();
  } catch (error) {
    expect(error).toBeInstanceOf(HttpError);
    expect((error as HttpError).status).toBe(status);
    if (code !== undefined) {
      expect((error as HttpError).code).toBe(code);
    }
    return;
  }
  throw new Error("expected a validation error");
}

describe("parseDeviceRegistration", () => {
  const ok = { device_id: U1, public_key_b64: KEY, display_name: "Alice" };

  it("accepts a valid body and trims the name", () => {
    expect(parseDeviceRegistration({ ...ok, display_name: "  Alice  " })).toEqual(ok);
  });

  it("accepts 1 and 32 character names, counting code points", () => {
    expect(parseDeviceRegistration({ ...ok, display_name: "a" }).display_name).toBe("a");
    expect(parseDeviceRegistration({ ...ok, display_name: "x".repeat(32) }).display_name).toHaveLength(32);
    expect(() => parseDeviceRegistration({ ...ok, display_name: "😀".repeat(32) })).not.toThrow();
  });

  it.each([
    ["missing id", { ...ok, device_id: undefined }],
    ["uppercase id", { ...ok, device_id: U1.replace("0000", "ABCD") }],
    ["non-string id", { ...ok, device_id: 5 }],
    ["missing key", { ...ok, public_key_b64: undefined }],
    ["31-byte key", { ...ok, public_key_b64: btoa("a".repeat(31)) }],
    ["33-byte key", { ...ok, public_key_b64: btoa("a".repeat(33)) }],
    ["not base64", { ...ok, public_key_b64: "not base64!!" }],
    ["url-safe base64", { ...ok, public_key_b64: KEY.replace("B", "-") }],
    ["empty name", { ...ok, display_name: "" }],
    ["blank name", { ...ok, display_name: "   " }],
    ["33-char name", { ...ok, display_name: "x".repeat(33) }],
    ["control chars", { ...ok, display_name: "a\nb" }],
    ["bidi override", { ...ok, display_name: "a‮b" }],
    ["lone surrogate", { ...ok, display_name: "a\ud800b" }],
    ["number name", { ...ok, display_name: 12 }],
  ])("rejects %s", (_name, body) => {
    rejects(() => parseDeviceRegistration(body as Record<string, unknown>));
  });
});

describe("parseCreateCrew / parseJoin", () => {
  it("validates crew names", () => {
    expect(parseCreateCrew({ name: " Squad " })).toBe("Squad");
    rejects(() => parseCreateCrew({}));
    rejects(() => parseCreateCrew({ name: "" }));
    rejects(() => parseCreateCrew({ name: "x".repeat(49) }));
    expect(parseCreateCrew({ name: "x".repeat(48) })).toHaveLength(48);
  });

  it("normalizes and validates invite codes", () => {
    expect(parseJoin({ code: "ABCDEF2345" })).toBe("ABCDEF2345");
    expect(parseJoin({ code: " abcdef2345 " })).toBe("ABCDEF2345");
    for (const code of ["", "ABC", "ABCDEF234", "ABCDEF23456", "ABCDEF2341", "ABCDEF2340", "ABCDEF-345", 12345]) {
      rejects(() => parseJoin({ code }));
    }
    rejects(() => parseJoin({}));
  });

  it("folds case for ASCII letters only", () => {
    // "ı" (dotless i) and "ſ" (long s) upper-case to I and S with toUpperCase(); they must not.
    for (const code of ["ıBCDEF2345", "ABCDEF234ſ", "abcdef234ſ", "ABCDEFGHIK".replace("I", "ı")]) {
      rejects(() => parseJoin({ code }), 400, "invalid_request");
    }
    expect(parseJoin({ code: "zzyyxx2233" })).toBe("ZZYYXX2233");
  });
});

describe("parseClipRegistration", () => {
  it("defaults ttl_s to 72 hours", () => {
    expect(parseClipRegistration({ clip_id: U1, crew_id: U2 })).toEqual({ clip_id: U1, crew_id: U2, ttl_s: 259_200 });
  });

  it("accepts ttl_s between 1 and 259200", () => {
    expect(parseClipRegistration({ clip_id: U1, crew_id: U2, ttl_s: 1 }).ttl_s).toBe(1);
    expect(parseClipRegistration({ clip_id: U1, crew_id: U2, ttl_s: 259_200 }).ttl_s).toBe(259_200);
  });

  it.each([
    ["ttl 0", { ttl_s: 0 }],
    ["ttl negative", { ttl_s: -5 }],
    ["ttl too long", { ttl_s: 259_201 }],
    ["ttl fractional", { ttl_s: 1.5 }],
    ["ttl string", { ttl_s: "60" }],
    ["ttl null", { ttl_s: null }],
    ["ttl NaN-like", { ttl_s: 1e400 }],
    ["bad clip id", { clip_id: "nope" }],
    ["uppercase crew id", { crew_id: "00000000-0000-0000-0000-00000000000A" }],
    ["missing crew id", { crew_id: undefined }],
  ])("rejects %s", (_name, patch) => {
    rejects(() => parseClipRegistration({ clip_id: U1, crew_id: U2, ...patch }));
  });
});

describe("parseUploadRequest", () => {
  const ok = {
    pov: U1,
    quality: "full",
    indices: [0, 1, 2],
    sizes: [10, 20, 30],
    manifest: true,
    manifest_size: 500,
  };

  it("accepts a valid request", () => {
    expect(parseUploadRequest(ok)).toEqual(ok);
  });

  it("defaults manifest to false and allows a manifest-only request", () => {
    expect(parseUploadRequest({ ...ok, manifest: undefined, manifest_size: undefined })).toMatchObject({
      manifest: false,
      manifest_size: null,
    });
    expect(
      parseUploadRequest({ pov: U1, quality: "proxy", indices: [], sizes: [], manifest: true, manifest_size: 1 }),
    ).toEqual({
      pov: U1,
      quality: "proxy",
      indices: [],
      sizes: [],
      manifest: true,
      manifest_size: 1,
    });
  });

  it("ignores manifest_size when no manifest is requested", () => {
    expect(parseUploadRequest({ ...ok, manifest: false, manifest_size: "junk" }).manifest_size).toBeNull();
  });

  it("requires an exact manifest_size (at most 1 MiB) whenever the manifest is requested", () => {
    rejects(() => parseUploadRequest({ ...ok, manifest_size: undefined }), 400, "invalid_request");
    for (const manifest_size of [0, -1, 1.5, "500", null, Number.NaN, 1e400]) {
      rejects(() => parseUploadRequest({ ...ok, manifest_size }), 400, "invalid_request");
    }
    expect(parseUploadRequest({ ...ok, manifest_size: MIB }).manifest_size).toBe(MIB);
    rejects(() => parseUploadRequest({ ...ok, manifest_size: MIB + 1 }), 413, "manifest_too_large");
  });

  it("accepts exactly 64 indices and the 64 MiB size cap", () => {
    const indices = Array.from({ length: 64 }, (_, i) => i);
    const sizes = indices.map(() => 64 * MIB);
    expect(parseUploadRequest({ ...ok, indices, sizes }).indices).toHaveLength(64);
    expect(parseUploadRequest({ ...ok, indices: [999_999], sizes: [1] }).indices).toEqual([999_999]);
  });

  it("rejects 65 indices with too_many_indices", () => {
    const indices = Array.from({ length: 65 }, (_, i) => i);
    rejects(() => parseUploadRequest({ ...ok, indices, sizes: indices.map(() => 1) }), 400, "too_many_indices");
  });

  it("rejects chunks above 64 MiB with 413", () => {
    rejects(() => parseUploadRequest({ ...ok, indices: [0], sizes: [64 * MIB + 1] }), 413, "chunk_too_large");
  });

  it.each([
    ["bad pov", { pov: "x" }],
    ["bad quality", { quality: "hd" }],
    ["missing quality", { quality: undefined }],
    ["indices not array", { indices: "0,1" }],
    ["negative index", { indices: [-1, 1, 2] }],
    ["fractional index", { indices: [0.5, 1, 2] }],
    ["string index", { indices: ["0", 1, 2] }],
    ["index too large", { indices: [1_000_000, 1, 2] }],
    ["duplicate indices", { indices: [0, 0, 2] }],
    ["sizes missing", { sizes: undefined }],
    ["sizes shorter", { sizes: [10, 20] }],
    ["sizes longer", { sizes: [10, 20, 30, 40] }],
    ["zero size", { sizes: [0, 20, 30] }],
    ["negative size", { sizes: [-1, 20, 30] }],
    ["fractional size", { sizes: [1.5, 20, 30] }],
    ["manifest not boolean", { manifest: "yes" }],
    ["nothing requested", { indices: [], sizes: [], manifest: false }],
  ])("rejects %s", (_name, patch) => {
    rejects(() => parseUploadRequest({ ...ok, ...patch }));
  });
});

describe("parseDownloadRequest", () => {
  it("accepts indices and/or the manifest", () => {
    expect(parseDownloadRequest({ pov: U1, quality: "full", indices: [3] })).toEqual({
      pov: U1,
      quality: "full",
      indices: [3],
      manifest: false,
    });
    expect(parseDownloadRequest({ pov: U1, quality: "proxy", indices: [], manifest: true }).manifest).toBe(true);
  });

  it("applies the same limits as uploads", () => {
    rejects(() => parseDownloadRequest({ pov: U1, quality: "full", indices: [] }));
    rejects(() => parseDownloadRequest({ pov: U1, quality: "full", indices: [1, 1] }));
    rejects(() => parseDownloadRequest({ pov: U1, quality: "x", indices: [1] }));
    rejects(
      () => parseDownloadRequest({ pov: U1, quality: "full", indices: Array.from({ length: 65 }, (_, i) => i) }),
      400,
      "too_many_indices",
    );
    expect(
      parseDownloadRequest({ pov: U1, quality: "full", indices: Array.from({ length: 64 }, (_, i) => i) }).indices,
    ).toHaveLength(64);
  });
});

describe("parseJsonObject", () => {
  const enc = (text: string) => new TextEncoder().encode(text);

  it("parses objects", () => {
    expect(parseJsonObject(enc('{"a":1}'))).toEqual({ a: 1 });
  });

  it.each(["", "not json", "[]", "null", "123", '"str"', "{", "ÿþ"])("rejects %j", (text) => {
    rejects(() => parseJsonObject(enc(text)));
  });

  it("rejects invalid UTF-8", () => {
    rejects(() => parseJsonObject(Uint8Array.from([0x7b, 0x22, 0xff, 0x22, 0x3a, 0x31, 0x7d])), 400, "invalid_json");
  });
});

describe("readBody", () => {
  it("returns the body and treats no body as empty", async () => {
    const body = await readBody(new Request("https://x.test/", { method: "POST", body: "hello" }), MAX_BODY_BYTES);
    expect(new TextDecoder().decode(body)).toBe("hello");
    expect((await readBody(new Request("https://x.test/"), MAX_BODY_BYTES)).length).toBe(0);
  });

  it("allows exactly 16 KiB and rejects one byte more with 413", async () => {
    const exact = new Request("https://x.test/", { method: "POST", body: "a".repeat(MAX_BODY_BYTES) });
    expect((await readBody(exact, MAX_BODY_BYTES)).length).toBe(MAX_BODY_BYTES);
    const over = new Request("https://x.test/", { method: "POST", body: "a".repeat(MAX_BODY_BYTES + 1) });
    await expect(readBody(over, MAX_BODY_BYTES)).rejects.toMatchObject({ status: 413 });
  });

  it("counts the stream even when Content-Length lies or is missing", async () => {
    const stream = new ReadableStream<Uint8Array>({
      start(controller) {
        controller.enqueue(new Uint8Array(MAX_BODY_BYTES));
        controller.enqueue(new Uint8Array(1));
        controller.close();
      },
    });
    const request = new Request("https://x.test/", {
      method: "POST",
      body: stream,
      headers: { "content-length": "5" },
      duplex: "half",
    } as RequestInit);
    await expect(readBody(request, MAX_BODY_BYTES)).rejects.toMatchObject({ status: 413 });
  });
});
