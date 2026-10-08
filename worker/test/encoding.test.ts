import { describe, expect, it } from "vitest";
import { base64Decode, base64Encode, sha256Hex, timingSafeEqual, toHex, utf8 } from "../src/encoding.js";

describe("encoding", () => {
  it("hex encodes with zero padding", () => {
    expect(toHex(Uint8Array.from([0, 1, 15, 16, 255]))).toBe("00010f10ff");
    expect(toHex(new Uint8Array(0))).toBe("");
  });

  it("round-trips base64", () => {
    for (const bytes of [[0], [1, 2], [1, 2, 3], [255, 254, 253, 252], Array.from({ length: 32 }, (_, i) => i * 7)]) {
      const input = Uint8Array.from(bytes);
      expect(base64Decode(base64Encode(input))).toEqual(input);
    }
    expect(base64Encode(utf8("hello"))).toBe("aGVsbG8=");
  });

  it("rejects non-canonical or malformed base64", () => {
    for (const text of [
      "",
      "aGVsbG8", // missing padding
      "aGVsbG8==", // wrong padding
      "aGVsbG9=", // non-zero trailing bits ("hello" is aGVsbG8=)
      "aGV sbG8=", // whitespace
      "aGVs\nbG8=",
      "aGVsbG8=\n",
      "a-Vs", // url-safe alphabet
      "aGVs=bG8", // padding in the middle
      "====",
      "é===",
    ]) {
      expect(base64Decode(text), JSON.stringify(text)).toBeNull();
    }
  });

  it("computes sha256 as lowercase hex", async () => {
    expect(await sha256Hex(new Uint8Array(0))).toBe(
      "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
    );
    expect(await sha256Hex(utf8("abc"))).toBe(
      "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
    );
  });

  it("compares byte strings in constant time semantics", () => {
    expect(timingSafeEqual(Uint8Array.from([1, 2, 3]), Uint8Array.from([1, 2, 3]))).toBe(true);
    expect(timingSafeEqual(Uint8Array.from([1, 2, 3]), Uint8Array.from([1, 2, 4]))).toBe(false);
    expect(timingSafeEqual(Uint8Array.from([1, 2, 3]), Uint8Array.from([1, 2]))).toBe(false);
    expect(timingSafeEqual(new Uint8Array(0), new Uint8Array(0))).toBe(true);
  });
});
