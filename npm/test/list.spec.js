import { describe, it, expect, beforeEach, afterEach } from 'vitest';
import { createTempStore } from '../helpers.js';

function put(s3, bucket, key, body) {
  s3.putObject(bucket, key, Buffer.from(body));
}

describe('listObjectsV2', () => {
  let store;
  beforeEach(() => {
    store = createTempStore();
    store.s3.createBucket('bkt');
  });
  afterEach(() => { store.cleanup(); });

  it('lists all keys sorted alphabetically', () => {
    put(store.s3, 'bkt', 'c', 'c');
    put(store.s3, 'bkt', 'a', 'a');
    put(store.s3, 'bkt', 'b', 'b');

    const out = store.s3.listObjectsV2('bkt');
    expect(out.contents.map((e) => e.key)).toEqual(['a', 'b', 'c']);
    expect(out.keyCount).toBe(3);
    expect(out.isTruncated).toBe(false);
    expect(out.nextContinuationToken).toBeUndefined();
    expect(out.commonPrefixes).toEqual([]);
  });

  it('each entry includes etag, size, lastModified, storageClass', () => {
    put(store.s3, 'bkt', 'hi', 'hello');
    const out = store.s3.listObjectsV2('bkt');
    const e = out.contents[0];
    expect(e.key).toBe('hi');
    expect(e.size).toBe(5);
    expect(e.etag).toMatch(/^"[0-9a-f]{32}"$/);
    expect(e.storageClass).toBe('STANDARD');
    expect(e.lastModified).toBeGreaterThan(0);
  });

  it('prefix filter only returns matching keys', () => {
    put(store.s3, 'bkt', 'imgs/a.png', 'a');
    put(store.s3, 'bkt', 'imgs/b.png', 'b');
    put(store.s3, 'bkt', 'docs/c.txt', 'c');

    const out = store.s3.listObjectsV2('bkt', 'imgs/');
    expect(out.contents.map((e) => e.key)).toEqual(['imgs/a.png', 'imgs/b.png']);
    expect(out.keyCount).toBe(2);
  });

  it('delimiter collapses keys into commonPrefixes', () => {
    put(store.s3, 'bkt', 'a/x.txt', 'x');
    put(store.s3, 'bkt', 'a/y.txt', 'y');
    put(store.s3, 'bkt', 'b/z.txt', 'z');
    put(store.s3, 'bkt', 'root.txt', 'r');

    const out = store.s3.listObjectsV2('bkt', null, '/');
    expect(out.contents.map((e) => e.key)).toEqual(['root.txt']);
    expect(out.commonPrefixes.sort()).toEqual(['a/', 'b/']);
  });

  it('paginates with maxKeys + continuationToken across all keys exactly once', () => {
    for (let i = 0; i < 5; i++) {
      put(store.s3, 'bkt', `obj${i}`, String(i));
    }

    const seen = new Set();
    let token = null;
    let pages = 0;
    while (true) {
      const out = store.s3.listObjectsV2('bkt', null, null, 2, token);
      pages++;
      for (const e of out.contents) {
        expect(seen.has(e.key)).toBe(false);
        seen.add(e.key);
      }
      if (!out.isTruncated) {
        expect(out.nextContinuationToken).toBeUndefined();
        break;
      }
      expect(out.nextContinuationToken).not.toBeUndefined();
      token = out.nextContinuationToken;
    }
    expect(pages).toBe(3);
    expect(seen.size).toBe(5);
  });

  it('list on empty bucket returns empty contents', () => {
    store.s3.createBucket('empty');
    const out = store.s3.listObjectsV2('empty');
    expect(out.contents).toEqual([]);
    expect(out.commonPrefixes).toEqual([]);
    expect(out.keyCount).toBe(0);
    expect(out.isTruncated).toBe(false);
  });

  it('list on missing bucket throws NoSuchBucket', () => {
    expect(() => store.s3.listObjectsV2('ghost')).toThrowError(/^\[NoSuchBucket\]/);
  });
});
