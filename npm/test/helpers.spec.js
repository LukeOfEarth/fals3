import fs from 'node:fs';
import { describe, it, expect } from 'vitest';
import { createTempStore, reset, listAll, snapshot } from '../helpers.js';

describe('test helpers', () => {
  it('createTempStore returns a usable Fals3 + cleanup removes the dir', () => {
    const { s3, baseDir, cleanup } = createTempStore();
    expect(fs.existsSync(baseDir)).toBe(true);
    s3.createBucket('bkt');
    s3.putObject('bkt', 'k', Buffer.from('x'));
    cleanup();
    expect(fs.existsSync(baseDir)).toBe(false);
  });

  it('reset wipes contents but keeps store usable', () => {
    const { s3, baseDir, cleanup } = createTempStore();
    try {
      s3.createBucket('bkt');
      s3.putObject('bkt', 'k', Buffer.from('x'));
      reset(s3, baseDir);
      // Bucket directory was wiped — listObjectsV2 should now report NoSuchBucket.
      expect(() => s3.listObjectsV2('bkt')).toThrowError(/^\[NoSuchBucket\]/);
      // And the same instance can be reused after a fresh createBucket.
      s3.createBucket('bkt');
      const out = s3.listObjectsV2('bkt');
      expect(out.keyCount).toBe(0);
    } finally { cleanup(); }
  });

  it('listAll enumerates objects across all buckets sorted by bucket then key', () => {
    const { s3, baseDir, cleanup } = createTempStore();
    try {
      s3.createBucket('bkt1');
      s3.createBucket('bkt2');
      s3.putObject('bkt1', 'a', Buffer.from('1'));
      s3.putObject('bkt1', 'b', Buffer.from('22'));
      s3.putObject('bkt2', 'c', Buffer.from('333'));

      const all = listAll(s3, baseDir);
      expect(all).toEqual([
        { bucket: 'bkt1', key: 'a', size: 1, etag: expect.stringMatching(/^"[0-9a-f]{32}"$/) },
        { bucket: 'bkt1', key: 'b', size: 2, etag: expect.stringMatching(/^"[0-9a-f]{32}"$/) },
        { bucket: 'bkt2', key: 'c', size: 3, etag: expect.stringMatching(/^"[0-9a-f]{32}"$/) },
      ]);
    } finally { cleanup(); }
  });

  it('snapshot returns a stable map keyed by "bucket/key"', () => {
    const { s3, baseDir, cleanup } = createTempStore();
    try {
      s3.createBucket('bkt');
      s3.putObject('bkt', 'one', Buffer.from('hi'), 'text/plain');
      s3.putObject('bkt', 'two', Buffer.from('there'));

      const snap = snapshot(s3, baseDir);
      expect(Object.keys(snap).sort()).toEqual(['bkt/one', 'bkt/two']);
      expect(snap['bkt/one']).toEqual({
        size: 2,
        etag: expect.stringMatching(/^"[0-9a-f]{32}"$/),
        contentType: 'text/plain',
      });
      expect(snap['bkt/two'].contentType).toBeUndefined();
    } finally { cleanup(); }
  });
});
