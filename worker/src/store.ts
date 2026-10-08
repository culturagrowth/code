/**
 * Object storage access (R2 binding) behind a small interface, plus the clip deletion routine
 * shared by `DELETE /v1/clips/:clip` and the hourly sweep.
 */

import { clipPrefix } from "./keys.js";

/** Largest number of keys R2 deletes in one call. */
export const R2_DELETE_BATCH = 1000;

/**
 * Default cap on R2 calls (list + delete) for one clip. A clip never holds more than 6144 objects
 * (see `MIN_CHARGED_BYTES`), which takes at most 14 calls, so this is only a safety valve and
 * keeps a request inside the Workers subrequest limit.
 */
export const DEFAULT_MAX_STORE_CALLS = 30;

/** One page of keys under a prefix. */
export interface KeyPage {
  keys: string[];
  /** `true` when more keys exist beyond this page. */
  truncated: boolean;
}

/** The object-store operations the Worker needs. */
export interface ObjectStore {
  /** Lists up to `R2_DELETE_BATCH` keys under `prefix`, in key order. */
  list(prefix: string): Promise<KeyPage>;
  /** Deletes the given keys (at most `R2_DELETE_BATCH`). Missing keys are not an error. */
  delete(keys: string[]): Promise<void>;
}

/** The subset of an R2 bucket binding used here (a real `R2Bucket` fits). */
export interface R2BucketLike {
  list(options: { prefix: string; limit: number }): Promise<{
    objects: Array<{ key: string }>;
    truncated: boolean;
  }>;
  delete(keys: string[]): Promise<void>;
}

/** Adapts an R2 bucket binding to {@link ObjectStore}. */
export function createR2Store(bucket: R2BucketLike): ObjectStore {
  return {
    async list(prefix) {
      const page = await bucket.list({ prefix, limit: R2_DELETE_BATCH });
      return { keys: page.objects.map((object) => object.key), truncated: page.truncated };
    },
    delete: (keys) => bucket.delete(keys),
  };
}

/** Outcome of {@link deleteClipObjects}. */
export interface DeleteResult {
  /** Number of objects deleted. */
  deleted: number;
  /** `true` when the prefix was listed to its end, i.e. nothing was left behind. */
  complete: boolean;
}

/** Options of {@link deleteClipObjects}. */
export interface DeleteOptions {
  /** Most store calls (list or delete) to make; a deletion cut short reports `complete: false`. */
  maxCalls?: number;
}

/**
 * Deletes every object under `clips/{crew}/{clip}/`.
 *
 * Each round lists the first page of what is left and deletes it; when the listing was truncated
 * the next round simply lists again from the start (the deleted keys are gone), so no cursor
 * handling is needed. The loop is bounded by `maxCalls`; running out reports `complete: false`
 * so callers keep the clip and try again instead of assuming it is gone.
 */
export async function deleteClipObjects(
  store: ObjectStore,
  crewId: string,
  clipId: string,
  options: DeleteOptions = {},
): Promise<DeleteResult> {
  const maxCalls = options.maxCalls ?? DEFAULT_MAX_STORE_CALLS;
  const prefix = clipPrefix(crewId, clipId);
  let deleted = 0;
  let calls = 0;
  while (calls < maxCalls) {
    calls += 1;
    const { keys, truncated } = await store.list(prefix);
    if (keys.length === 0) {
      // An empty page that still claims to be truncated is not trusted as "done".
      return { deleted, complete: !truncated };
    }
    if (calls >= maxCalls) {
      return { deleted, complete: false }; // no call left to delete what was listed
    }
    calls += 1;
    await store.delete(keys);
    deleted += keys.length;
    if (!truncated) {
      return { deleted, complete: true };
    }
  }
  return { deleted, complete: false };
}
