import { describe, it, expect, beforeEach, afterEach } from 'vitest';
import { createTempStore } from '../helpers.js';

describe('object ops', () => {
  let store;
  beforeEach(() => {
    store = createTempStore();
    store.s3.createBucket('bkt');
  });
  afterEach(() => { store.cleanup(); });

  it('put then get round-trips body and metadata', () => {
    const etag = store.s3.putObject(
      'bkt',
      'hello.txt',
      Buffer.from('hello world'),
      'text/plain',
      { author: 'test' },
    );
    expect(etag).toMatch(/^"[0-9a-f]{32}"$/);

    const got = store.s3.getObject('bkt', 'hello.txt');
    expect(Buffer.isBuffer(got.body)).toBe(true);
    expect(got.body.toString()).toBe('hello world');
    expect(got.meta.etag).toBe(etag);
    expect(got.meta.contentType).toBe('text/plain');
    expect(got.meta.size).toBe(11);
    expect(got.meta.userMetadata).toEqual({ author: 'test' });
    expect(got.meta.storageClass).toBe('STANDARD');
    expect(got.meta.lastModified).toBeGreaterThan(0);
  });

  it('etag is MD5 hex of body, matching AWS shape', () => {
    // Empty body has the well-known MD5 of zero bytes.
    const etag = store.s3.putObject('bkt', 'empty', Buffer.alloc(0));
    expect(etag).toBe('"d41d8cd98f00b204e9800998ecf8427e"');
  });

  it('put with nested key creates parent dirs', () => {
    store.s3.putObject('bkt', 'users/1/avatar.png', Buffer.from([0x89, 0x50, 0x4e, 0x47]));
    const got = store.s3.getObject('bkt', 'users/1/avatar.png');
    expect(got.body[0]).toBe(0x89);
    expect(got.body[1]).toBe(0x50);
  });

  it('put overwrites existing object and updates etag', () => {
    const e1 = store.s3.putObject('bkt', 'k', Buffer.from('one'));
    const e2 = store.s3.putObject('bkt', 'k', Buffer.from('two'));
    expect(e1).not.toBe(e2);
    const got = store.s3.getObject('bkt', 'k');
    expect(got.body.toString()).toBe('two');
  });

  it('headObject returns metadata without body', () => {
    store.s3.putObject('bkt', 'f', Buffer.from('hi'), 'text/plain', { x: 'y' });
    const meta = store.s3.headObject('bkt', 'f');
    expect(meta.size).toBe(2);
    expect(meta.contentType).toBe('text/plain');
    expect(meta.userMetadata).toEqual({ x: 'y' });
  });

  it('getObject with rangeStart/rangeEnd returns inclusive byte slice', () => {
    store.s3.putObject('bkt', 'data.bin', Buffer.from('0123456789'));
    const got = store.s3.getObject('bkt', 'data.bin', 2, 5);
    expect(got.body.toString()).toBe('2345');
    expect(got.meta.size).toBe(10);
  });

  it('getObject with range past end clamps to body length', () => {
    store.s3.putObject('bkt', 'short', Buffer.from('abc'));
    const got = store.s3.getObject('bkt', 'short', 1, 999);
    expect(got.body.toString()).toBe('bc');
  });

  it('getObject on missing key throws NoSuchKey', () => {
    expect(() => store.s3.getObject('bkt', 'ghost')).toThrowError(/^\[NoSuchKey\]/);
  });

  it('headObject on missing key throws NoSuchKey', () => {
    expect(() => store.s3.headObject('bkt', 'ghost')).toThrowError(/^\[NoSuchKey\]/);
  });

  it('deleteObject is idempotent on missing key', () => {
    expect(() => store.s3.deleteObject('bkt', 'never-existed')).not.toThrow();
  });

  it('deleteObject removes both body and metadata', () => {
    store.s3.putObject('bkt', 'bye.txt', Buffer.from('bye'));
    store.s3.deleteObject('bkt', 'bye.txt');
    expect(() => store.s3.getObject('bkt', 'bye.txt')).toThrowError(/^\[NoSuchKey\]/);
    expect(() => store.s3.headObject('bkt', 'bye.txt')).toThrowError(/^\[NoSuchKey\]/);
  });

  it('binary data round-trips byte-for-byte', () => {
    const bytes = Buffer.from([0, 1, 2, 0xff, 0xfe, 0x80, 0x7f]);
    store.s3.putObject('bkt', 'bin', bytes);
    const got = store.s3.getObject('bkt', 'bin');
    expect(Buffer.compare(got.body, bytes)).toBe(0);
  });

  it('contentEncoding round-trips', () => {
    store.s3.putObject('bkt', 'gz', Buffer.from('payload'), 'application/json', null, 'gzip');
    const meta = store.s3.headObject('bkt', 'gz');
    expect(meta.contentEncoding).toBe('gzip');
  });

  it('put on missing bucket throws NoSuchBucket', () => {
    expect(() => store.s3.putObject('no-such', 'k', Buffer.from('x'))).toThrowError(
      /^\[NoSuchBucket\]/,
    );
  });

  it('put with invalid key throws InvalidObjectKey', () => {
    expect(() => store.s3.putObject('bkt', '', Buffer.from('x'))).toThrowError(
      /^\[InvalidObjectKey\]/,
    );
    expect(() => store.s3.putObject('bkt', '../escape', Buffer.from('x'))).toThrowError(
      /^\[InvalidObjectKey\]/,
    );
  });
});
