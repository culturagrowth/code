import { readFileSync, readdirSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, it } from "vitest";
import { SqliteD1 } from "./helpers/sqlite-d1.js";

const root = join(import.meta.dirname, "..");
const read = (file: string) => readFileSync(join(root, file), "utf8");

describe("project configuration (SPEC layout)", () => {
  it("wrangler.toml declares the entry point, bindings and the hourly cron", () => {
    const toml = read("wrangler.toml");
    expect(toml).toMatch(/^name = "duoclip-worker"$/m);
    expect(toml).toMatch(/^main = "src\/index\.ts"$/m);
    expect(toml).toMatch(/^compatibility_date = "\d{4}-\d{2}-\d{2}"$/m);
    expect(toml).toMatch(/\[\[r2_buckets\]\]\s+binding = "CLIPS"/);
    expect(toml).toMatch(/\[\[d1_databases\]\]\s+binding = "DB"/);
    expect(toml).toMatch(/crons = \["0 \* \* \* \*"\]/);
    expect(toml).toMatch(/^ACCOUNT_ID = /m);
    expect(toml).toMatch(/^BUCKET_NAME = /m);
    // Secrets must never be committed in the config file.
    expect(toml).not.toMatch(/^R2_(ACCESS_KEY_ID|SECRET_ACCESS_KEY)\s*=/m);
  });

  it("package.json exposes the required scripts and exact dependency versions", () => {
    const pkg = JSON.parse(read("package.json")) as {
      scripts: Record<string, string>;
      dependencies: Record<string, string>;
      devDependencies: Record<string, string>;
    };
    expect(pkg.scripts["typecheck"]).toBe("tsc --noEmit");
    expect(pkg.scripts["test"]).toBe("vitest run");
    expect(pkg.scripts["dev"]).toBe("wrangler dev");
    expect(Object.keys(pkg.dependencies)).toEqual(["aws4fetch"]);
    for (const version of [...Object.values(pkg.dependencies), ...Object.values(pkg.devDependencies)]) {
      expect(version).toMatch(/^\d+\.\d+\.\d+$/);
    }
  });

  it("the migration creates every table of the data model", () => {
    const d1 = new SqliteD1();
    const tables = d1
      .query<{ name: string }>("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
      .map((row) => row.name);
    for (const table of ["devices", "crews", "crew_members", "invites", "clips", "usage", "seen_signatures", "counters"]) {
      expect(tables).toContain(table);
    }
    const columns = (table: string) =>
      d1.query<{ name: string }>(`SELECT name FROM pragma_table_info('${table}')`).map((row) => row.name);
    expect(columns("devices")).toEqual(["device_id", "public_key_b64", "display_name", "created_at"]);
    expect(columns("crews")).toEqual(["crew_id", "name", "created_by", "created_at"]);
    expect(columns("crew_members")).toEqual(["crew_id", "device_id", "joined_at"]);
    expect(columns("invites")).toEqual(["code", "crew_id", "created_by", "expires_at", "uses_left"]);
    expect(columns("clips")).toEqual(expect.arrayContaining(["clip_id", "crew_id", "owner", "created_at", "expires_at", "deleted_at"]));
    expect(columns("usage")).toEqual(["device_id", "day", "bytes"]);
    expect(columns("seen_signatures")).toEqual(["sig", "seen_at"]);
    expect(columns("counters")).toEqual(["scope", "day", "n"]);
  });

  it("migrations are numbered, append-only SQL files applied in order", () => {
    const files = readdirSync(join(root, "migrations")).sort();
    expect(files).toEqual(["0001_init.sql", "0002_abuse_limits.sql"]);
  });

  it("wrangler.toml documents the optional limit variables (commented out: the defaults apply)", () => {
    const toml = read("wrangler.toml");
    expect(toml).toMatch(/^# MAX_NEW_DEVICES_PER_DAY = "50"$/m);
    expect(toml).toMatch(/^# MAX_GLOBAL_DAILY_BYTES = "214748364800"/m);
    expect(214_748_364_800).toBe(200 * 1024 ** 3);
    expect(toml).not.toMatch(/^(MAX_NEW_DEVICES_PER_DAY|MAX_GLOBAL_DAILY_BYTES)\s*=/m);
  });
});
