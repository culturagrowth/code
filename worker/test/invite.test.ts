import { describe, expect, it } from "vitest";
import {
  generateInviteCode,
  INVITE_ALPHABET,
  INVITE_CODE_LENGTH,
  INVITE_TTL_MS,
  INVITE_USES,
} from "../src/invite.js";
import { seededRandom } from "./helpers/harness.js";

function rng(seed: number) {
  const random = seededRandom(seed);
  return (length: number) => Uint8Array.from({ length }, () => Math.floor(random() * 256));
}

describe("invite codes", () => {
  it("has the SPEC parameters", () => {
    expect(INVITE_CODE_LENGTH).toBe(10);
    expect(INVITE_TTL_MS).toBe(24 * 3600 * 1000);
    expect(INVITE_USES).toBe(5);
    expect(INVITE_ALPHABET).toBe("ABCDEFGHIJKLMNOPQRSTUVWXYZ23456789");
  });

  it("generates 10 characters of [A-Z2-9]", () => {
    const next = rng(42);
    for (let i = 0; i < 200; i += 1) {
      expect(generateInviteCode(next)).toMatch(/^[A-Z2-9]{10}$/);
    }
  });

  it("is deterministic for a fixed random source and varies across draws", () => {
    expect(generateInviteCode(rng(7))).toBe(generateInviteCode(rng(7)));
    const next = rng(7);
    const codes = new Set(Array.from({ length: 100 }, () => generateInviteCode(next)));
    expect(codes.size).toBe(100);
  });

  it("uses rejection sampling: bytes above the largest multiple of 34 are skipped", () => {
    // 238 = 34 * 7 is the first rejected value. Feed 255s (rejected) then valid bytes.
    const calls: number[] = [];
    const source = (length: number) => {
      calls.push(length);
      return calls.length === 1
        ? new Uint8Array(length).fill(255)
        : Uint8Array.from({ length }, (_, i) => i); // 0..19 -> 'A'..'T'
    };
    expect(generateInviteCode(source)).toBe("ABCDEFGHIJ");
    expect(calls.length).toBe(2);
  });

  it("covers the whole alphabet roughly uniformly", () => {
    const next = rng(99);
    const counts = new Map<string, number>();
    for (let i = 0; i < 400; i += 1) {
      for (const ch of generateInviteCode(next)) {
        counts.set(ch, (counts.get(ch) ?? 0) + 1);
      }
    }
    expect(counts.size).toBe(INVITE_ALPHABET.length);
    for (const count of counts.values()) {
      // Expected 4000 / 34 ~ 118 per character.
      expect(count).toBeGreaterThan(60);
      expect(count).toBeLessThan(200);
    }
  });
});
