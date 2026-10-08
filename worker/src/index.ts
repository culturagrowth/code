/**
 * DuoClip Worker entry point.
 *
 * `fetch` serves the JSON API (see `routes.ts`); `scheduled` runs the hourly sweep (`sweep.ts`).
 * The Worker never sees clip content in clear: clips are end-to-end encrypted by the apps.
 */

import type { App } from "./app.js";
import type { Env } from "./env.js";
import { createD1Db } from "./db.js";
import { createPresigner, presignConfigFromEnv } from "./presign.js";
import type { Presigner } from "./presign.js";
import { limitsFromEnv } from "./quota.js";
import { handleRequest } from "./routes.js";
import { createR2Store } from "./store.js";
import { runSweep } from "./sweep.js";

/** Wires the real Cloudflare bindings into the dependencies the handlers use. */
export function createApp(env: Env): App {
  const now = () => Date.now();
  // Created on first use so a missing secret only fails routes that presign, with a 500.
  let presigner: Presigner | undefined;
  return {
    db: createD1Db(env.DB),
    store: createR2Store(env.CLIPS),
    presigner: {
      async presign(method, key, options) {
        presigner ??= createPresigner(presignConfigFromEnv(env), now);
        return presigner.presign(method, key, options);
      },
    },
    limits: limitsFromEnv(env),
    now,
    randomBytes: (length) => crypto.getRandomValues(new Uint8Array(length)),
    randomUuid: () => crypto.randomUUID(),
  };
}

export default {
  async fetch(request, env): Promise<Response> {
    return handleRequest(request, createApp(env));
  },

  async scheduled(_controller, env): Promise<void> {
    // Awaited (not waitUntil) so a failing sweep is reported as a failed cron invocation.
    await runSweep(createApp(env));
  },
} satisfies ExportedHandler<Env>;
