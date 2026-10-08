import { describe, expect, it } from "vitest";
import {
  additionalBytes,
  chargedBytes,
  DEFAULT_LIMITS,
  evaluateQuota,
  limitsFromEnv,
  MANIFEST_INDEX,
  MAX_CHUNK_BYTES,
  MAX_CLIP_BYTES,
  MAX_DAILY_BYTES,
  MAX_INDICES_PER_REQUEST,
  MAX_MANIFEST_BYTES,
  MAX_TTL_S,
  MIN_CHARGED_BYTES,
  utcDay,
} from "../src/quota.js";

const MIB = 1024 * 1024;
const GIB = 1024 * MIB;

describe("quota constants", () => {
  it("match the SPEC", () => {
    expect(MAX_INDICES_PER_REQUEST).toBe(64);
    expect(MAX_CHUNK_BYTES).toBe(64 * MIB);
    expect(MAX_CLIP_BYTES).toBe(1.5 * GIB);
    expect(MAX_CLIP_BYTES).toBe(1_610_612_736);
    expect(MAX_DAILY_BYTES).toBe(10 * GIB);
    expect(MAX_TTL_S).toBe(259_200);
  });
});

describe("utcDay", () => {
  it("formats the UTC calendar day", () => {
    expect(utcDay(Date.UTC(2026, 9, 8, 0, 0, 0))).toBe("2026-10-08");
    expect(utcDay(Date.UTC(2026, 9, 8, 23, 59, 59, 999))).toBe("2026-10-08");
    expect(utcDay(Date.UTC(2026, 9, 9, 0, 0, 0))).toBe("2026-10-09");
    expect(utcDay(0)).toBe("1970-01-01");
  });
});

describe("chargedBytes (per-object floor)", () => {
  it("charges at least 256 KiB per object so tiny objects cannot be used to flood R2", () => {
    expect(MIN_CHARGED_BYTES).toBe(256 * 1024);
    expect(chargedBytes(1)).toBe(MIN_CHARGED_BYTES);
    expect(chargedBytes(MIN_CHARGED_BYTES)).toBe(MIN_CHARGED_BYTES);
    expect(chargedBytes(MIN_CHARGED_BYTES + 1)).toBe(MIN_CHARGED_BYTES + 1);
    expect(chargedBytes(MAX_CHUNK_BYTES)).toBe(MAX_CHUNK_BYTES);
  });

  it("bounds the number of objects per clip and per device-day", () => {
    expect(MAX_CLIP_BYTES / MIN_CHARGED_BYTES).toBe(6144);
    expect(MAX_DAILY_BYTES / MIN_CHARGED_BYTES).toBe(40_960);
  });

  it("keeps the manifest small and its pseudo index out of the real chunk range", () => {
    expect(MAX_MANIFEST_BYTES).toBe(MIB);
    expect(MANIFEST_INDEX).toBeLessThan(0);
  });
});

describe("additionalBytes", () => {
  const BIG = 10 * MIB;

  it("charges the full (floored) size for new chunks", () => {
    expect(additionalBytes(new Map(), [0, 1, 2], [BIG, 2 * BIG, 3 * BIG])).toBe(6 * BIG);
    expect(additionalBytes(new Map(), [0, 1, 2], [10, 20, 30])).toBe(3 * MIN_CHARGED_BYTES);
  });

  it("charges nothing for a retry with the same sizes", () => {
    const existing = new Map([
      [0, BIG],
      [1, 2 * BIG],
    ]);
    expect(additionalBytes(existing, [0, 1], [BIG, 2 * BIG])).toBe(0);
    // Tiny chunks were charged the floor, so retrying them is free too.
    const tiny = new Map([[0, MIN_CHARGED_BYTES]]);
    expect(additionalBytes(tiny, [0], [10])).toBe(0);
  });

  it("charges only the growth, never a refund for shrinking", () => {
    const existing = new Map([
      [0, BIG],
      [1, 2 * BIG],
    ]);
    expect(additionalBytes(existing, [0, 1, 2], [BIG + 5, BIG, 7 * MIB])).toBe(5 + 0 + 7 * MIB);
  });

  it("handles empty requests", () => {
    expect(additionalBytes(new Map(), [], [])).toBe(0);
  });

  it("charges the manifest like a chunk", () => {
    expect(additionalBytes(new Map(), [MANIFEST_INDEX], [100])).toBe(MIN_CHARGED_BYTES);
    expect(additionalBytes(new Map([[MANIFEST_INDEX, MIN_CHARGED_BYTES]]), [MANIFEST_INDEX], [100])).toBe(0);
  });
});

describe("limitsFromEnv", () => {
  it("uses the defaults when nothing is configured", () => {
    expect(limitsFromEnv({})).toEqual(DEFAULT_LIMITS);
  });

  it("reads the two tunable limits from strings or numbers, `0` included", () => {
    expect(limitsFromEnv({ MAX_NEW_DEVICES_PER_DAY: "7", MAX_GLOBAL_DAILY_BYTES: 5_000 })).toEqual({
      ...DEFAULT_LIMITS,
      newDevicesPerDay: 7,
      globalDailyBytes: 5_000,
    });
    expect(limitsFromEnv({ MAX_NEW_DEVICES_PER_DAY: "0" }).newDevicesPerDay).toBe(0);
    expect(limitsFromEnv({ MAX_NEW_DEVICES_PER_DAY: " 12 " }).newDevicesPerDay).toBe(12);
  });

  it("falls back to the default for anything that is not a non-negative integer", () => {
    for (const bad of ["", "abc", "-1", "1.5", "1e3", "0x10", "99999999999999999", null, undefined, true, -5, 2.5, Number.NaN]) {
      expect(limitsFromEnv({ MAX_NEW_DEVICES_PER_DAY: bad }).newDevicesPerDay, String(bad)).toBe(
        DEFAULT_LIMITS.newDevicesPerDay,
      );
    }
  });
});

describe("evaluateQuota", () => {
  it("allows up to exactly the per-clip limit and refuses one byte more", () => {
    expect(evaluateQuota(0, 0, MAX_CLIP_BYTES)).toBe("ok");
    expect(evaluateQuota(0, 0, MAX_CLIP_BYTES + 1)).toBe("clip_limit");
    expect(evaluateQuota(MAX_CLIP_BYTES - 5, 0, 5)).toBe("ok");
    expect(evaluateQuota(MAX_CLIP_BYTES - 5, 0, 6)).toBe("clip_limit");
  });

  it("allows up to exactly the daily limit and refuses one byte more", () => {
    expect(evaluateQuota(0, MAX_DAILY_BYTES - 100, 100)).toBe("ok");
    expect(evaluateQuota(0, MAX_DAILY_BYTES - 100, 101)).toBe("day_limit");
    expect(evaluateQuota(0, MAX_DAILY_BYTES, 1)).toBe("day_limit");
  });

  it("reports the clip limit first when both are exceeded", () => {
    expect(evaluateQuota(MAX_CLIP_BYTES, MAX_DAILY_BYTES, 1)).toBe("clip_limit");
  });

  it("is ok for zero additional bytes", () => {
    expect(evaluateQuota(MAX_CLIP_BYTES, MAX_DAILY_BYTES, 0)).toBe("ok");
  });
});
