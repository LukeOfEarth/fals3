import { describe, it, expect, beforeEach, afterEach } from 'vitest';
import { createTempStore } from '../helpers.js';

describe('copyObject', () => {
  let store;
  beforeEach(() => {
    store = createTempStore();
    store.s3.createBucket('bkt');
  });
  afterEach(() => { store.cleanup(); });

  it('copies within same bucket and preserves body', () => {
    store.s3.putObject('bkt', 'src.txt', Buffer.from('hello'));
    const out = store.s3.copyObject('bkt', 'src.txt', 'bkt', 'dst.txt');
    expect(out.etag).toMatch(/^"[0-9a-f]{32}"$/);
    expect(out.lastModified).toBeGreaterThan(0);

    expect(store.s3.getObject('bkt', 'dst.txt').body.toString()).toBe('hello');
    // Source still present.
    expect(store.s3.getObject('bkt', 'src.txt').body.toString()).toBe('hello');
  });

  it('copies across buckets', () => {
    store.s3.createBucket('dst-bkt');
    store.s3.putObject('bkt', 'file.txt', Buffer.from('data'));
    store.s3.copyObject('bkt', 'file.txt', 'dst-bkt', 'copy.txt');
    expect(store.s3.getObject('dst-bkt', 'copy.txt').body.toString()).toBe('data');
  });

  it('default directive preserves source contentType and userMetadata', () => {
    store.s3.putObject('bkt', 'src', Buffer.from('hi'), 'text/plain', { owner: 'alice' });
    store.s3.copyObject('bkt', 'src', 'bkt', 'dst');
    const meta = store.s3.headObject('bkt', 'dst');
    expect(meta.contentType).toBe('text/plain');
    expect(meta.userMetadata).toEqual({ owner: 'alice' });
  });

  it('replaceContentType + replaceMetadata override source metadata', () => {
    store.s3.putObject('bkt', 'src', Buffer.from('hi'), 'text/plain', { owner: 'alice' });
    store.s3.copyObject(
      'bkt',
      'src',
      'bkt',
      'dst',
      'application/octet-stream',
      { owner: 'bob' },
    );
    const meta = store.s3.headObject('bkt', 'dst');
    expect(meta.contentType).toBe('application/octet-stream');
    expect(meta.userMetadata).toEqual({ owner: 'bob' });
  });

  it('missing source key throws NoSuchKey', () => {
    expect(() =>
      store.s3.copyObject('bkt', 'ghost', 'bkt', 'dst'),
    ).toThrowError(/^\[NoSuchKey\]/);
  });

  it('missing source bucket throws NoSuchBucket', () => {
    expect(() =>
      store.s3.copyObject('no-such', 'k', 'bkt', 'dst'),
    ).toThrowError(/^\[NoSuchBucket\]/);
  });

  it('missing destination bucket throws NoSuchBucket', () => {
    store.s3.putObject('bkt', 'src', Buffer.from('x'));
    expect(() =>
      store.s3.copyObject('bkt', 'src', 'no-dst', 'dst'),
    ).toThrowError(/^\[NoSuchBucket\]/);
  });
});
