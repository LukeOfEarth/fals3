import { describe, it, expect, beforeEach, afterEach } from 'vitest';
import { createTempStore } from '../helpers.js';

describe('multipart upload', () => {
  let store;
  beforeEach(() => {
    store = createTempStore();
    store.s3.createBucket('bkt');
  });
  afterEach(() => { store.cleanup(); });

  it('full roundtrip: create, upload parts, complete, get', () => {
    const created = store.s3.createMultipartUpload('bkt', 'big.bin', 'application/octet-stream', {
      author: 'alice',
    });
    expect(created.bucket).toBe('bkt');
    expect(created.key).toBe('big.bin');
    expect(typeof created.uploadId).toBe('string');
    expect(created.uploadId.length).toBeGreaterThan(0);

    const p1 = store.s3.uploadPart(created.uploadId, 1, Buffer.from('hello '));
    const p2 = store.s3.uploadPart(created.uploadId, 2, Buffer.from('world'));
    expect(p1.partNumber).toBe(1);
    expect(p2.partNumber).toBe(2);
    expect(p1.etag).toMatch(/^"[0-9a-f]{32}"$/);

    const done = store.s3.completeMultipartUpload(created.uploadId, [
      { partNumber: 1, etag: p1.etag },
      { partNumber: 2, etag: p2.etag },
    ]);
    expect(done.bucket).toBe('bkt');
    expect(done.key).toBe('big.bin');
    // Multipart ETag format: "<32-hex>-<part-count>"
    expect(done.etag).toMatch(/^"[0-9a-f]{32}-2"$/);

    const got = store.s3.getObject('bkt', 'big.bin');
    expect(got.body.toString()).toBe('hello world');
    expect(got.meta.etag).toBe(done.etag);
    expect(got.meta.contentType).toBe('application/octet-stream');
    expect(got.meta.userMetadata).toEqual({ author: 'alice' });
    expect(got.meta.size).toBe(11);
  });

  it('completed object survives reset of upload state directory', () => {
    const created = store.s3.createMultipartUpload('bkt', 'k');
    const p1 = store.s3.uploadPart(created.uploadId, 1, Buffer.from('x'));
    store.s3.completeMultipartUpload(created.uploadId, [{ partNumber: 1, etag: p1.etag }]);

    // Re-completing should fail — upload state was cleaned up.
    let err;
    try {
      store.s3.completeMultipartUpload(created.uploadId, [{ partNumber: 1, etag: p1.etag }]);
    } catch (e) { err = e; }
    expect(err.code).toBe('NoSuchUpload');
  });

  it('listParts returns uploaded parts in part-number order', () => {
    const created = store.s3.createMultipartUpload('bkt', 'k');
    const p3 = store.s3.uploadPart(created.uploadId, 3, Buffer.from('ccc'));
    const p1 = store.s3.uploadPart(created.uploadId, 1, Buffer.from('a'));
    const p2 = store.s3.uploadPart(created.uploadId, 2, Buffer.from('bb'));

    const listed = store.s3.listParts(created.uploadId);
    expect(listed.bucket).toBe('bkt');
    expect(listed.key).toBe('k');
    expect(listed.parts.map((p) => p.partNumber)).toEqual([1, 2, 3]);
    expect(listed.parts.map((p) => p.size)).toEqual([1, 2, 3]);
    expect(listed.parts[0].etag).toBe(p1.etag);
    expect(listed.parts[1].etag).toBe(p2.etag);
    expect(listed.parts[2].etag).toBe(p3.etag);
  });

  it('abortMultipartUpload removes the in-flight state', () => {
    const created = store.s3.createMultipartUpload('bkt', 'k');
    store.s3.uploadPart(created.uploadId, 1, Buffer.from('x'));
    store.s3.abortMultipartUpload(created.uploadId);

    let err;
    try { store.s3.listParts(created.uploadId); } catch (e) { err = e; }
    expect(err.code).toBe('NoSuchUpload');
  });

  it('abortMultipartUpload on unknown uploadId is idempotent (no throw)', () => {
    expect(() => store.s3.abortMultipartUpload('does-not-exist')).not.toThrow();
  });

  it('uploadPart on unknown uploadId throws NoSuchUpload', () => {
    let err;
    try {
      store.s3.uploadPart('ghost', 1, Buffer.from('x'));
    } catch (e) { err = e; }
    expect(err.code).toBe('NoSuchUpload');
  });

  it('uploadPart with partNumber=0 throws InvalidPart', () => {
    const created = store.s3.createMultipartUpload('bkt', 'k');
    let err;
    try {
      store.s3.uploadPart(created.uploadId, 0, Buffer.from('x'));
    } catch (e) { err = e; }
    expect(err.code).toBe('InvalidPart');
  });

  it('uploadPart with partNumber > 10000 throws InvalidPart', () => {
    const created = store.s3.createMultipartUpload('bkt', 'k');
    let err;
    try {
      store.s3.uploadPart(created.uploadId, 10_001, Buffer.from('x'));
    } catch (e) { err = e; }
    expect(err.code).toBe('InvalidPart');
  });

  it('completeMultipartUpload with empty parts throws InvalidPart', () => {
    const created = store.s3.createMultipartUpload('bkt', 'k');
    let err;
    try {
      store.s3.completeMultipartUpload(created.uploadId, []);
    } catch (e) { err = e; }
    expect(err.code).toBe('InvalidPart');
  });

  it('completeMultipartUpload with descending parts throws InvalidPartOrder', () => {
    const created = store.s3.createMultipartUpload('bkt', 'k');
    const p1 = store.s3.uploadPart(created.uploadId, 1, Buffer.from('a'));
    const p2 = store.s3.uploadPart(created.uploadId, 2, Buffer.from('b'));
    let err;
    try {
      store.s3.completeMultipartUpload(created.uploadId, [
        { partNumber: 2, etag: p2.etag },
        { partNumber: 1, etag: p1.etag },
      ]);
    } catch (e) { err = e; }
    expect(err.code).toBe('InvalidPartOrder');
  });

  it('completeMultipartUpload with wrong etag throws InvalidPart', () => {
    const created = store.s3.createMultipartUpload('bkt', 'k');
    store.s3.uploadPart(created.uploadId, 1, Buffer.from('a'));
    let err;
    try {
      store.s3.completeMultipartUpload(created.uploadId, [
        { partNumber: 1, etag: '"deadbeef"' },
      ]);
    } catch (e) { err = e; }
    expect(err.code).toBe('InvalidPart');
  });

  it('createMultipartUpload on missing bucket throws NoSuchBucket', () => {
    let err;
    try { store.s3.createMultipartUpload('ghost', 'k'); } catch (e) { err = e; }
    expect(err.code).toBe('NoSuchBucket');
  });

  it('re-uploading the same partNumber overwrites and Complete uses latest etag', () => {
    const created = store.s3.createMultipartUpload('bkt', 'k');
    const first = store.s3.uploadPart(created.uploadId, 1, Buffer.from('first'));
    const second = store.s3.uploadPart(created.uploadId, 1, Buffer.from('second-version'));
    expect(first.etag).not.toBe(second.etag);

    store.s3.completeMultipartUpload(created.uploadId, [
      { partNumber: 1, etag: second.etag },
    ]);
    expect(store.s3.getObject('bkt', 'k').body.toString()).toBe('second-version');
  });

  it('multipart object can be GET with byte range like any other object', () => {
    const created = store.s3.createMultipartUpload('bkt', 'k');
    const p1 = store.s3.uploadPart(created.uploadId, 1, Buffer.from('hello '));
    const p2 = store.s3.uploadPart(created.uploadId, 2, Buffer.from('world'));
    store.s3.completeMultipartUpload(created.uploadId, [
      { partNumber: 1, etag: p1.etag },
      { partNumber: 2, etag: p2.etag },
    ]);
    const slice = store.s3.getObject('bkt', 'k', 6, 10);
    expect(slice.body.toString()).toBe('world');
  });
});
