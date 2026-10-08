/**
 * Hourly sweep (Cron Trigger `0 * * * *`).
 *
 * - Deletes the R2 objects of clips whose `expires_at` has passed, then their rows (only for
 *   clips whose objects were verifiably all removed).
 * - Purges replay-protection signatures, expired invites and old usage counters.
 *
 * Presigned URLs stay valid for up to 15 minutes after they are issued, so an upload that was
 * authorised just before a clip expired (or was deleted) may still land after the objects were
 * removed. To close that window a clip row is kept for one more sweep after its objects were
 * deleted: the next run lists the prefix again and removes any late arrival, then drops the row.
 * The R2 lifecycle rule documented in README.md remains the final backstop.
 */

import type { App } from "./app.js";
import { PRESIGN_EXPIRES_S } from "./presign.js";
import { PRESENCE_TTL_MS } from "./presence.js";
import { utcDay } from "./quota.js";
import { deleteClipObjects } from "./store.js";
import type { ObjectStore } from "./store.js";

/**
 * Most clips looked at per run. Anything left over is picked up by the next hourly run.
 */
export const SWEEP_MAX_CLIPS = 20;

/**
 * Most R2 calls (list + delete) one run makes. The free Workers plan allows 50 subrequests per
 * invocation, D1 queries included; the sweep itself needs at most 8 D1 calls, so 40 keeps a safe
 * margin. A clip normally costs 2 calls. When the budget runs out the remaining clips (or the
 * rest of a very large one) wait for the next run, with their rows still in place. On a paid
 * plan this can be raised.
 */
export const SWEEP_STORE_CALL_BUDGET = 40;

/** Replay-protection signatures are kept this long (the replay window is 10 minutes). */
export const SIGNATURE_RETENTION_MS = 15 * 60 * 1000;

/** Daily usage and abuse counters older than this many days are dropped. */
export const USAGE_RETENTION_DAYS = 7;

/** What one sweep did. */
export interface SweepStats {
  /** Expired clips whose objects were processed. */
  clips_processed: number;
  /** R2 objects deleted. */
  objects_deleted: number;
  /** Clip rows removed. */
  clip_rows_deleted: number;
  /** Clips whose deletion failed and will be retried next run. */
  clip_failures: number;
  /** Clips only partly emptied because the R2 call budget ran out; they are kept and retried. */
  clips_incomplete: number;
  /** Clips not touched at all this run because the R2 call budget was already used up. */
  clips_deferred: number;
  signatures_purged: number;
  invites_purged: number;
  usage_rows_purged: number;
  counter_rows_purged: number;
  presence_rows_purged: number;
}

/** Runs one sweep and logs a one-line JSON summary. */
export async function runSweep(app: Pick<App, "db" | "store" | "now">): Promise<SweepStats> {
  const now = app.now();
  const stats: SweepStats = {
    clips_processed: 0,
    objects_deleted: 0,
    clip_rows_deleted: 0,
    clip_failures: 0,
    clips_incomplete: 0,
    clips_deferred: 0,
    signatures_purged: 0,
    invites_purged: 0,
    usage_rows_purged: 0,
    counter_rows_purged: 0,
    presence_rows_purged: 0,
  };

  // Counts every R2 call, also the ones that throw, so the budget cannot be overrun.
  let storeCalls = 0;
  const store: ObjectStore = {
    list: (prefix) => {
      storeCalls += 1;
      return app.store.list(prefix);
    },
    delete: (keys) => {
      storeCalls += 1;
      return app.store.delete(keys);
    },
  };

  const expired = await app.db.listExpiredClips(now, SWEEP_MAX_CLIPS);
  const rowsToDrop: string[] = [];
  const rowGraceCutoff = now - PRESIGN_EXPIRES_S * 1000;
  for (const clip of expired) {
    const callsLeft = SWEEP_STORE_CALL_BUDGET - storeCalls;
    if (callsLeft < 2) {
      stats.clips_deferred += 1;
      continue;
    }
    try {
      const result = await deleteClipObjects(store, clip.crew_id, clip.clip_id, {
        maxCalls: callsLeft,
      });
      stats.objects_deleted += result.deleted;
      if (!result.complete) {
        // Never drop the row while objects may remain: the next run finishes the job.
        stats.clips_incomplete += 1;
        continue;
      }
      stats.clips_processed += 1;
      if (clip.expires_at < rowGraceCutoff) {
        rowsToDrop.push(clip.clip_id);
      }
    } catch (error) {
      stats.clip_failures += 1;
      console.error(
        JSON.stringify({
          event: "sweep_clip_failed",
          clip_id: clip.clip_id,
          message: error instanceof Error ? error.message : "unknown error",
        }),
      );
    }
  }
  stats.clip_rows_deleted = await app.db.deleteClips(rowsToDrop);

  stats.signatures_purged = await app.db.purgeSignatures(now - SIGNATURE_RETENTION_MS);
  stats.invites_purged = await app.db.purgeInvites(now);
  const retentionDay = utcDay(now - USAGE_RETENTION_DAYS * 24 * 60 * 60 * 1000);
  stats.usage_rows_purged = await app.db.purgeUsage(retentionDay);
  stats.counter_rows_purged = await app.db.purgeCounters(retentionDay);
  stats.presence_rows_purged = await app.db.purgePresence(now - PRESENCE_TTL_MS);

  console.log(JSON.stringify({ event: "sweep", ...stats }));
  return stats;
}
