import { describe, it, expect, beforeEach, afterEach } from 'vitest';
import { createTempStore } from '../helpers.js';

describe('conditional headers (preconditions)', () => {
  let store;
  let etag;
  let lastModified;

  beforeEach(() => {
    store = createTempStore();
    store.s3.createBucket('bkt');
    etag = store.s3.putObject('bkt', 'k', Buffer.from('hello'));
    lastModified = store.s3.headObject('bkt', 'k').lastModified;
  });
  afterEach(() => { store.cleanup(); });

  // ── getObject ────────────────────────────────────────────────────────────

  it('getObject ifMatch with current etag succeeds', () => {
    const out = store.s3.getObject('bkt', 'k', null, null, { ifMatch: etag });
    expect(out.body.toString()).toBe('hello');
  });

  it('getObject ifMatch with wildcard succeeds', () => {
    expect(() => store.s3.getObject('bkt', 'k', null, null, { ifMatch: '*' })).not.toThrow();
  });

  it('getObject ifMatch with stale etag throws PreconditionFailed', () => {
    let err;
    try {
      store.s3.getObject('bkt', 'k', null, null, { ifMatch: '"deadbeef"' });
    } catch (e) { err = e; }
    expect(err.code).toBe('PreconditionFailed');
  });

  it('getObject ifNoneMatch with current etag throws NotModified', () => {
    let err;
    try {
      store.s3.getObject('bkt', 'k', null, null, { ifNoneMatch: etag });
    } catch (e) { err = e; }
    expect(err.code).toBe('NotModified');
  });

  it('getObject ifNoneMatch with different etag returns body', () => {
    const out = store.s3.getObject('bkt', 'k', null, null, { ifNoneMatch: '"deadbeef"' });
    expect(out.body.toString()).toBe('hello');
  });

  it('getObject ifModifiedSince at lastModified throws NotModified', () => {
    let err;
    try {
      store.s3.getObject('bkt', 'k', null, null, { ifModifiedSince: lastModified });
    } catch (e) { err = e; }
    expect(err.code).toBe('NotModified');
  });

  it('getObject ifModifiedSince before lastModified returns body', () => {
    const out = store.s3.getObject('bkt', 'k', null, null, { ifModifiedSince: lastModified - 1 });
    expect(out.body.toString()).toBe('hello');
  });

  it('getObject ifUnmodifiedSince before lastModified throws PreconditionFailed', () => {
    let err;
    try {
      store.s3.getObject('bkt', 'k', null, null, { ifUnmodifiedSince: lastModified - 1 });
    } catch (e) { err = e; }
    expect(err.code).toBe('PreconditionFailed');
  });

  it('getObject ifMatch wins over ifNoneMatch (412 priority)', () => {
    let err;
    try {
      store.s3.getObject('bkt', 'k', null, null, {
        ifMatch: '"deadbeef"',
        ifNoneMatch: etag,
      });
    } catch (e) { err = e; }
    expect(err.code).toBe('PreconditionFailed');
  });

  // ── headObject ───────────────────────────────────────────────────────────

  it('headObject honours ifMatch and ifNoneMatch', () => {
    expect(store.s3.headObject('bkt', 'k', { ifMatch: etag }).etag).toBe(etag);
    let err;
    try { store.s3.headObject('bkt', 'k', { ifNoneMatch: etag }); }
    catch (e) { err = e; }
    expect(err.code).toBe('NotModified');
  });

  // ── putObject ────────────────────────────────────────────────────────────

  it('putObject ifNoneMatch=* succeeds when key absent (atomic create)', () => {
    expect(() =>
      store.s3.putObject('bkt', 'fresh', Buffer.from('x'), null, null, null, {
        ifNoneMatch: '*',
      }),
    ).not.toThrow();
  });

  it('putObject ifNoneMatch=* throws PreconditionFailed when key exists', () => {
    let err;
    try {
      store.s3.putObject('bkt', 'k', Buffer.from('x'), null, null, null, {
        ifNoneMatch: '*',
      });
    } catch (e) { err = e; }
    expect(err.code).toBe('PreconditionFailed');
  });

  it('putObject ifMatch with current etag overwrites (optimistic concurrency)', () => {
    const newEtag = store.s3.putObject(
      'bkt', 'k', Buffer.from('replaced'),
      null, null, null,
      { ifMatch: etag },
    );
    expect(newEtag).not.toBe(etag);
    expect(store.s3.getObject('bkt', 'k').body.toString()).toBe('replaced');
  });

  it('putObject ifMatch with stale etag throws PreconditionFailed', () => {
    let err;
    try {
      store.s3.putObject(
        'bkt', 'k', Buffer.from('x'),
        null, null, null,
        { ifMatch: '"deadbeef"' },
      );
    } catch (e) { err = e; }
    expect(err.code).toBe('PreconditionFailed');
    // Original body untouched.
    expect(store.s3.getObject('bkt', 'k').body.toString()).toBe('hello');
  });

  it('putObject ifMatch on absent key throws PreconditionFailed', () => {
    let err;
    try {
      store.s3.putObject(
        'bkt', 'never', Buffer.from('x'),
        null, null, null,
        { ifMatch: '"any"' },
      );
    } catch (e) { err = e; }
    expect(err.code).toBe('PreconditionFailed');
  });

  // ── copyObject ───────────────────────────────────────────────────────────

  it('copyObject sourceConditions ifMatch on stale etag throws PreconditionFailed', () => {
    let err;
    try {
      store.s3.copyObject(
        'bkt', 'k', 'bkt', 'dst',
        null, null,
        { ifMatch: '"deadbeef"' },
      );
    } catch (e) { err = e; }
    expect(err.code).toBe('PreconditionFailed');
  });

  it('copyObject sourceConditions ifMatch with current etag succeeds', () => {
    expect(() =>
      store.s3.copyObject(
        'bkt', 'k', 'bkt', 'dst',
        null, null,
        { ifMatch: etag },
      ),
    ).not.toThrow();
    expect(store.s3.getObject('bkt', 'dst').body.toString()).toBe('hello');
  });

  // ── etag normalization ───────────────────────────────────────────────────

  it('ifMatch accepts etag without surrounding quotes', () => {
    const unquoted = etag.replace(/"/g, '');
    expect(() => store.s3.getObject('bkt', 'k', null, null, { ifMatch: unquoted })).not.toThrow();
  });
});
