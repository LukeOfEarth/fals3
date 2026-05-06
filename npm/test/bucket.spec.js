import { describe, it, expect, beforeEach, afterEach } from 'vitest';
import { Fals3 } from '../index.js';
import { createTempStore } from '../helpers.js';

describe('bucket ops', () => {
  let store;
  beforeEach(() => { store = createTempStore(); });
  afterEach(() => { store.cleanup(); });

  it('createBucket then headBucket succeeds', () => {
    store.s3.createBucket('my-bucket');
    expect(() => store.s3.headBucket('my-bucket')).not.toThrow();
  });

  it('createBucket twice throws BucketAlreadyExists', () => {
    store.s3.createBucket('dup');
    expect(() => store.s3.createBucket('dup')).toThrowError(/^\[BucketAlreadyExists\]/);
  });

  it('headBucket on missing throws NoSuchBucket', () => {
    expect(() => store.s3.headBucket('ghost')).toThrowError(/^\[NoSuchBucket\]/);
  });

  it('createBucket rejects invalid name', () => {
    expect(() => store.s3.createBucket('BadName')).toThrowError(/^\[InvalidBucketName\]/);
    expect(() => store.s3.createBucket('ab')).toThrowError(/^\[InvalidBucketName\]/);
    expect(() => store.s3.createBucket('-bad')).toThrowError(/^\[InvalidBucketName\]/);
  });

  it('deleteBucket on empty bucket succeeds and clears it', () => {
    store.s3.createBucket('todel');
    store.s3.deleteBucket('todel');
    expect(() => store.s3.headBucket('todel')).toThrowError(/^\[NoSuchBucket\]/);
  });

  it('deleteBucket non-empty without force throws BucketNotEmpty', () => {
    store.s3.createBucket('full');
    store.s3.putObject('full', 'k', Buffer.from('x'));
    expect(() => store.s3.deleteBucket('full')).toThrowError(/^\[BucketNotEmpty\]/);
  });

  it('deleteBucket non-empty with force succeeds', () => {
    store.s3.createBucket('full');
    store.s3.putObject('full', 'k', Buffer.from('x'));
    store.s3.deleteBucket('full', true);
    expect(() => store.s3.headBucket('full')).toThrowError(/^\[NoSuchBucket\]/);
  });

  it('deleteBucket on missing bucket throws NoSuchBucket', () => {
    expect(() => store.s3.deleteBucket('ghost')).toThrowError(/^\[NoSuchBucket\]/);
  });

  it('Fals3.version returns a non-empty string', () => {
    expect(typeof Fals3.version()).toBe('string');
    expect(Fals3.version().length).toBeGreaterThan(0);
  });
});
