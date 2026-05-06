import { describe, it, expect } from 'vitest';
import { createTempStore } from '../helpers.js';

/**
 * Every fals3 error is a real JS Error with:
 *   err.code     — AWS-style error code (e.g. "NoSuchBucket")
 *   err.message  — "[<code>] <human description>"
 *
 * The tests below assert on `err.code` (the canonical contract) and also
 * check the message-prefix mirror for tooling that inspects messages.
 */

function catchError(fn) {
  try { fn(); } catch (e) { return e; }
  return undefined;
}

describe('error.code surface', () => {
  it('NoSuchBucket on headBucket', () => {
    const { s3, cleanup } = createTempStore();
    try {
      const err = catchError(() => s3.headBucket('ghost'));
      expect(err).toBeInstanceOf(Error);
      expect(err.code).toBe('NoSuchBucket');
      expect(err.message).toMatch(/^\[NoSuchBucket\]/);
    } finally { cleanup(); }
  });

  it('BucketAlreadyExists on duplicate createBucket', () => {
    const { s3, cleanup } = createTempStore();
    try {
      s3.createBucket('dup');
      const err = catchError(() => s3.createBucket('dup'));
      expect(err.code).toBe('BucketAlreadyExists');
    } finally { cleanup(); }
  });

  it('BucketNotEmpty on deleteBucket without force', () => {
    const { s3, cleanup } = createTempStore();
    try {
      s3.createBucket('full');
      s3.putObject('full', 'k', Buffer.from('x'));
      const err = catchError(() => s3.deleteBucket('full'));
      expect(err.code).toBe('BucketNotEmpty');
    } finally { cleanup(); }
  });

  it('InvalidBucketName on bad bucket name', () => {
    const { s3, cleanup } = createTempStore();
    try {
      const err = catchError(() => s3.createBucket('Bad_Name'));
      expect(err.code).toBe('InvalidBucketName');
    } finally { cleanup(); }
  });

  it('NoSuchKey on getObject of missing key', () => {
    const { s3, cleanup } = createTempStore();
    try {
      s3.createBucket('bkt');
      const err = catchError(() => s3.getObject('bkt', 'ghost'));
      expect(err.code).toBe('NoSuchKey');
    } finally { cleanup(); }
  });

  it('InvalidObjectKey on empty key', () => {
    const { s3, cleanup } = createTempStore();
    try {
      s3.createBucket('bkt');
      const err = catchError(() => s3.putObject('bkt', '', Buffer.from('x')));
      expect(err.code).toBe('InvalidObjectKey');
    } finally { cleanup(); }
  });

  it('InvalidObjectKey on traversal segment', () => {
    const { s3, cleanup } = createTempStore();
    try {
      s3.createBucket('bkt');
      const err = catchError(() => s3.putObject('bkt', '../escape', Buffer.from('x')));
      expect(err.code).toBe('InvalidObjectKey');
    } finally { cleanup(); }
  });

  it('NoSuchBucket on copyObject with missing destination bucket', () => {
    const { s3, cleanup } = createTempStore();
    try {
      s3.createBucket('src');
      s3.putObject('src', 'k', Buffer.from('x'));
      const err = catchError(() => s3.copyObject('src', 'k', 'no-dst', 'k2'));
      expect(err.code).toBe('NoSuchBucket');
    } finally { cleanup(); }
  });

  it('errors thrown are real Error instances (instanceof check)', () => {
    const { s3, cleanup } = createTempStore();
    try {
      const err = catchError(() => s3.headBucket('ghost'));
      expect(err).toBeInstanceOf(Error);
      expect(typeof err.stack).toBe('string');
      expect(err.name).toBe('Error');
    } finally { cleanup(); }
  });
});
