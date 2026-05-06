'use strict';

/**
 * fals3 test helpers — non-AWS utilities for use in test suites.
 *
 * These are NOT part of the S3 API surface. They are debug/test conveniences,
 * clearly named to avoid confusion with real S3 operations.
 */

const os = require('os');
const fs = require('fs');
const path = require('path');
const { Fals3 } = require('./index.js');

/**
 * Create a Fals3 instance backed by a fresh temporary directory.
 *
 * The returned object includes a `baseDir` property and a `cleanup()` method
 * that removes the directory when the test is done.
 *
 * @returns {{ s3: import('./index').Fals3, baseDir: string, cleanup: () => void }}
 */
function createTempStore() {
  const baseDir = fs.mkdtempSync(path.join(os.tmpdir(), 'fals3-'));
  const s3 = Fals3.open({ baseDir });
  return {
    s3,
    baseDir,
    cleanup() {
      fs.rmSync(baseDir, { recursive: true, force: true });
    },
  };
}

/**
 * Reset a Fals3 store: wipe all buckets/objects under `baseDir` and recreate
 * the directory so the same `Fals3` instance can be reused.
 *
 * @param {import('./index').Fals3} _s3  Unused — kept for API symmetry.
 * @param {string} baseDir
 */
function reset(_s3, baseDir) {
  fs.rmSync(baseDir, { recursive: true, force: true });
  fs.mkdirSync(baseDir, { recursive: true });
}

/**
 * List every object in every bucket under `baseDir`.
 * Returns a flat array of `{ bucket, key, size, etag }` sorted by bucket then key.
 *
 * @param {import('./index').Fals3} s3
 * @returns {Array<{ bucket: string, key: string, size: number, etag: string }>}
 */
function listAll(s3, baseDir) {
  const buckets = fs
    .readdirSync(baseDir, { withFileTypes: true })
    .filter((d) => d.isDirectory())
    .map((d) => d.name)
    .sort();

  const results = [];
  for (const bucket of buckets) {
    const out = s3.listObjectsV2(bucket);
    for (const entry of out.contents) {
      results.push({ bucket, key: entry.key, size: entry.size, etag: entry.etag });
    }
  }
  return results;
}

/**
 * Snapshot the entire store: returns a plain object mapping
 * `"bucket/key"` → `{ size, etag, contentType }` for every object.
 * Useful for `expect(snapshot(s3, baseDir)).toMatchSnapshot()`.
 *
 * @param {import('./index').Fals3} s3
 * @param {string} baseDir
 * @returns {Record<string, { size: number, etag: string, contentType: string | null }>}
 */
function snapshot(s3, baseDir) {
  const entries = listAll(s3, baseDir);
  const result = {};
  for (const { bucket, key } of entries) {
    const meta = s3.headObject(bucket, key);
    result[`${bucket}/${key}`] = {
      size: meta.size,
      etag: meta.etag,
      contentType: meta.contentType,
    };
  }
  return result;
}

module.exports = { createTempStore, reset, listAll, snapshot };
