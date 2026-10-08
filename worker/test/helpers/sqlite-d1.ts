/**
 * A D1 stand-in backed by Node's built-in SQLite (`node:sqlite`), loaded with the real
 * migrations. D1 *is* SQLite, so the SQL in `src/db.ts` runs here exactly as it does in
 * production, with no network and no wrangler.
 */

import { readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { DatabaseSync } from "node:sqlite";
import type { SQLInputValue } from "node:sqlite";
import type { D1Like, D1StatementLike } from "../../src/db.js";

const MIGRATIONS_DIR = join(import.meta.dirname, "../../migrations");

/** Applies every migration, in file name order, like `wrangler d1 migrations apply`. */
function applyMigrations(database: DatabaseSync): void {
  const files = readdirSync(MIGRATIONS_DIR)
    .filter((name) => name.endsWith(".sql"))
    .sort();
  for (const file of files) {
    database.exec(readFileSync(join(MIGRATIONS_DIR, file), "utf8"));
  }
}

class SqliteStatement implements D1StatementLike {
  constructor(
    private readonly database: DatabaseSync,
    private readonly sql: string,
    private readonly params: SQLInputValue[] = [],
  ) {}

  bind(...values: unknown[]): D1StatementLike {
    return new SqliteStatement(this.database, this.sql, values as SQLInputValue[]);
  }

  first<T = unknown>(): Promise<T | null> {
    const row = this.database.prepare(this.sql).get(...this.params);
    return Promise.resolve(row === undefined ? null : ({ ...row } as T));
  }

  all<T = unknown>(): Promise<{ results: T[] }> {
    const rows = this.database.prepare(this.sql).all(...this.params);
    return Promise.resolve({ results: rows.map((row) => ({ ...row }) as T) });
  }

  run(): Promise<{ meta: { changes: number } }> {
    const result = this.database.prepare(this.sql).run(...this.params);
    return Promise.resolve({ meta: { changes: Number(result.changes) } });
  }
}

/** A D1-compatible database over an in-memory SQLite, with the schema already applied. */
export class SqliteD1 implements D1Like {
  readonly database = new DatabaseSync(":memory:");

  constructor() {
    applyMigrations(this.database);
  }

  prepare(sql: string): D1StatementLike {
    return new SqliteStatement(this.database, sql);
  }

  /** Runs the statements in one transaction, like D1's `batch()`. */
  async batch(statements: D1StatementLike[]): Promise<Array<{ meta: { changes?: number } }>> {
    this.database.exec("BEGIN");
    try {
      const results: Array<{ meta: { changes?: number } }> = [];
      for (const statement of statements) {
        results.push(await statement.run());
      }
      this.database.exec("COMMIT");
      return results;
    } catch (error) {
      this.database.exec("ROLLBACK");
      throw error;
    }
  }

  /** Test convenience: run a read query directly. */
  query<T = Record<string, unknown>>(sql: string, ...params: SQLInputValue[]): T[] {
    return this.database.prepare(sql).all(...params).map((row) => ({ ...row }) as T);
  }
}
