import { describe, expect, it } from "vitest";
import {
  clipPrefix,
  isQuality,
  isUuid,
  manifestKey,
  MAX_OBJECT_KEY_BYTES,
  objectKey,
  validateObjectKey,
} from "../src/keys.js";

// Same fixtures as the Rust tests in crates/duoclip-proto ("00000000-..." style uuids).
const CREW = "00000000-0000-0000-0000-000000000001";
const CLIP = "00000000-0000-0000-0000-000000000002";
const POV = "00000000-0000-0000-0000-000000000003";

describe("object key golden strings (must match duoclip-proto)", () => {
  it("builds chunk keys as clips/{crew}/{clip}/{pov}/{quality}/{index:06}.bin", () => {
    expect(objectKey(CREW, CLIP, POV, "proxy", 7)).toBe(
      "clips/00000000-0000-0000-0000-000000000001/00000000-0000-0000-0000-000000000002/00000000-0000-0000-0000-000000000003/proxy/000007.bin",
    );
    expect(objectKey(CREW, CLIP, POV, "full", 0)).toBe(
      "clips/00000000-0000-0000-0000-000000000001/00000000-0000-0000-0000-000000000002/00000000-0000-0000-0000-000000000003/full/000000.bin",
    );
    expect(objectKey(CREW, CLIP, POV, "full", 123456)).toMatch(/\/full\/123456\.bin$/);
  });

  it("does not truncate indices wider than six digits, like Rust's {:06}", () => {
    expect(objectKey(CREW, CLIP, POV, "full", 1_000_000)).toMatch(/\/full\/1000000\.bin$/);
    expect(objectKey(CREW, CLIP, POV, "full", 4_294_967_295)).toMatch(/\/full\/4294967295\.bin$/);
  });

  it("builds manifest keys as .../{quality}/manifest.bin", () => {
    expect(manifestKey(CREW, CLIP, POV, "proxy")).toBe(
      "clips/00000000-0000-0000-0000-000000000001/00000000-0000-0000-0000-000000000002/00000000-0000-0000-0000-000000000003/proxy/manifest.bin",
    );
    expect(manifestKey(CREW, CLIP, POV, "full")).toMatch(/\/full\/manifest\.bin$/);
  });

  it("builds the clip prefix as clips/{crew}/{clip}/", () => {
    expect(clipPrefix(CREW, CLIP)).toBe(
      "clips/00000000-0000-0000-0000-000000000001/00000000-0000-0000-0000-000000000002/",
    );
  });

  it("every key lives under its clip prefix and passes the structural validation", () => {
    const prefix = clipPrefix(CREW, CLIP);
    for (const key of [
      objectKey(CREW, CLIP, POV, "proxy", 3),
      objectKey(CREW, CLIP, POV, "full", 4_294_967_295),
      manifestKey(CREW, CLIP, POV, "full"),
    ]) {
      expect(key.startsWith(prefix)).toBe(true);
      expect(validateObjectKey(key)).toBeNull();
      expect(key.length).toBeLessThanOrEqual(MAX_OBJECT_KEY_BYTES);
    }
  });
});

describe("builders reject malformed input", () => {
  it("rejects uuids that are not lowercase hyphenated", () => {
    const upper = CREW.replace("0000", "ABCD");
    expect(() => objectKey(upper, CLIP, POV, "full", 0)).toThrow(RangeError);
    expect(() => objectKey(CREW, "../../etc", POV, "full", 0)).toThrow(RangeError);
    expect(() => objectKey(CREW, CLIP, "00000000000000000000000000000003", "full", 0)).toThrow(RangeError);
    expect(() => manifestKey(CREW, CLIP, `${POV}/x`, "full")).toThrow(RangeError);
    expect(() => clipPrefix("", CLIP)).toThrow(RangeError);
  });

  it("rejects bad qualities and indices", () => {
    expect(() => objectKey(CREW, CLIP, POV, "hd" as never, 0)).toThrow(RangeError);
    expect(() => manifestKey(CREW, CLIP, POV, "FULL" as never)).toThrow(RangeError);
    expect(() => objectKey(CREW, CLIP, POV, "full", -1)).toThrow(RangeError);
    expect(() => objectKey(CREW, CLIP, POV, "full", 1.5)).toThrow(RangeError);
    expect(() => objectKey(CREW, CLIP, POV, "full", 4_294_967_296)).toThrow(RangeError);
    expect(() => objectKey(CREW, CLIP, POV, "full", Number.NaN)).toThrow(RangeError);
  });
});

describe("validateObjectKey", () => {
  const good = objectKey(CREW, CLIP, POV, "full", 1);

  it("accepts a well-formed key", () => {
    expect(validateObjectKey(good)).toBeNull();
  });

  it.each([
    ["wrong prefix", "other/a/b.bin"],
    ["missing prefix", "a/clips/b.bin"],
    ["parent traversal", "clips/../secret"],
    ["dots in a name", "clips/a..b"],
    ["double slash", "clips//a"],
    ["space", "clips/a b"],
    ["percent", "clips/a%2fb"],
    ["backslash", "clips\\a"],
    ["non-ascii", "clips/á"],
    ["newline", "clips/a\nb"],
    ["too long", `clips/${"a".repeat(MAX_OBJECT_KEY_BYTES)}`],
  ])("rejects %s", (_name, key) => {
    expect(validateObjectKey(key)).not.toBeNull();
  });

  it("allows exactly 512 bytes", () => {
    expect(validateObjectKey(`clips/${"a".repeat(MAX_OBJECT_KEY_BYTES - 6)}`)).toBeNull();
  });
});

describe("isUuid / isQuality", () => {
  it("accepts lowercase hyphenated uuids of any version", () => {
    expect(isUuid("00000000-0000-0000-0000-000000000000")).toBe(true);
    expect(isUuid("123e4567-e89b-42d3-a456-426614174000")).toBe(true);
  });

  it("rejects everything else", () => {
    for (const value of [
      "123E4567-E89B-42D3-A456-426614174000",
      "123e4567e89b42d3a456426614174000",
      "123e4567-e89b-42d3-a456-42661417400",
      "123e4567-e89b-42d3-a456-4266141740000",
      " 123e4567-e89b-42d3-a456-426614174000",
      "123e4567-e89b-42d3-a456-426614174000\n",
      "{123e4567-e89b-42d3-a456-426614174000}",
      "",
      null,
      undefined,
      42,
      {},
    ]) {
      expect(isUuid(value)).toBe(false);
    }
  });

  it("knows the two qualities", () => {
    expect(isQuality("proxy")).toBe(true);
    expect(isQuality("full")).toBe(true);
    expect(isQuality("Full")).toBe(false);
    expect(isQuality(undefined)).toBe(false);
  });
});
