// The storage the mailbox logic needs. Structurally identical to the Durable Object `ctx.storage.kv`
// (SyncKvStorage on SQLite-backed objects), so the DO passes that in and tests pass MemoryKv.

export interface KvStore {
  get<T = unknown>(key: string): T | undefined;
  put<T>(key: string, value: T): void;
  delete(key: string): boolean;
  list<T = unknown>(options?: { prefix?: string; limit?: number }): Iterable<[string, T]>;
}

/** In-memory KvStore for tests and local tools. Values are structured-cloned like the real store. */
export class MemoryKv implements KvStore {
  private readonly map = new Map<string, unknown>();
  /** Number of put/delete calls (tests use it to check that unchanged config is not rewritten). */
  writes = 0;

  get<T = unknown>(key: string): T | undefined {
    const v = this.map.get(key);
    return v === undefined ? undefined : (structuredClone(v) as T);
  }

  put<T>(key: string, value: T): void {
    this.writes++;
    this.map.set(key, structuredClone(value));
  }

  delete(key: string): boolean {
    this.writes++;
    return this.map.delete(key);
  }

  *list<T = unknown>(options: { prefix?: string; limit?: number } = {}): Iterable<[string, T]> {
    const keys = [...this.map.keys()].filter((k) => !options.prefix || k.startsWith(options.prefix)).sort();
    const limit = options.limit ?? Infinity;
    let n = 0;
    for (const k of keys) {
      if (n++ >= limit) return;
      yield [k, structuredClone(this.map.get(k)) as T];
    }
  }

  get size(): number {
    return this.map.size;
  }

  keys(): string[] {
    return [...this.map.keys()].sort();
  }
}
