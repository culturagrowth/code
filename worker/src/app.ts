/** The dependencies a request handler needs, injected so tests can substitute fakes. */

import type { Db } from "./db.js";
import type { RandomBytes } from "./invite.js";
import type { Presigner } from "./presign.js";
import type { Limits } from "./quota.js";
import type { ObjectStore } from "./store.js";

/** Everything the router and the sweep use. */
export interface App {
  db: Db;
  store: ObjectStore;
  presigner: Presigner;
  /** Abuse limits (see `quota.ts`). */
  limits: Limits;
  /** Current unix time in milliseconds. */
  now(): number;
  /** Cryptographically secure random bytes. */
  randomBytes: RandomBytes;
  /** A fresh random UUID v4 (lowercase, hyphenated). */
  randomUuid(): string;
}
