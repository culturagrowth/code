import type { KeyPage, ObjectStore } from "../../src/store.js";

/** In-memory {@link ObjectStore} with a configurable page size to exercise pagination. */
export class MemoryObjectStore implements ObjectStore {
  readonly keys = new Set<string>();
  listCalls = 0;
  deleteCalls = 0;
  /** When set, deleting a key matching this predicate's prefix throws. */
  failDeleteWhen: ((keys: string[]) => boolean) | null = null;

  constructor(private readonly pageSize = 1000) {}

  put(...keys: string[]): void {
    for (const key of keys) {
      this.keys.add(key);
    }
  }

  list(prefix: string): Promise<KeyPage> {
    this.listCalls += 1;
    const matching = [...this.keys].filter((key) => key.startsWith(prefix)).sort();
    return Promise.resolve({
      keys: matching.slice(0, this.pageSize),
      truncated: matching.length > this.pageSize,
    });
  }

  delete(keys: string[]): Promise<void> {
    this.deleteCalls += 1;
    if (keys.length > 1000) {
      return Promise.reject(new Error("R2 deletes at most 1000 keys per call"));
    }
    if (this.failDeleteWhen?.(keys)) {
      return Promise.reject(new Error("simulated R2 failure"));
    }
    for (const key of keys) {
      this.keys.delete(key);
    }
    return Promise.resolve();
  }
}
