import type { Fals3 } from './index';

export interface TempStore {
  s3: Fals3;
  baseDir: string;
  cleanup(): void;
}

export interface SnapshotEntry {
  size: number;
  etag: string;
  contentType: string | undefined;
}

/**
 * Create a Fals3 instance backed by a fresh OS temp directory.
 * Call `cleanup()` in `afterEach` / `afterAll` to remove the directory.
 */
export function createTempStore(): TempStore;

/**
 * Wipe all buckets/objects under `baseDir` and recreate the directory.
 * The same `Fals3` instance can be reused after this call.
 */
export function reset(s3: Fals3, baseDir: string): void;

/**
 * List every object across all buckets, sorted by bucket then key.
 * Non-AWS debug helper — do not call in production paths.
 */
export function listAll(
  s3: Fals3,
  baseDir: string,
): Array<{ bucket: string; key: string; size: number; etag: string }>;

/**
 * Snapshot the entire store as a plain object keyed by `"bucket/key"`.
 * Useful with `expect(snapshot(s3, baseDir)).toMatchSnapshot()`.
 */
export function snapshot(
  s3: Fals3,
  baseDir: string,
): Record<string, SnapshotEntry>;
