import { describe, it, expect, beforeEach, afterEach } from 'vitest';
import { createTempStore } from '../helpers.js';
import { Fals3S3Client } from '../sdk-shim.js';

/**
 * The shim dispatches by `command.constructor.name` and reads `command.input`,
 * so we can build duck-typed commands here without depending on
 * `@aws-sdk/client-s3` at test time. The `cmd()` helper produces objects
 * indistinguishable from the real SDK command classes from the shim's
 * point of view.
 */
function cmd(name, input) {
  function Command() {}
  Object.defineProperty(Command, 'name', { value: name });
  const c = new Command();
  c.input = input;
  return c;
}

describe('Fals3S3Client (AWS SDK v3 shim)', () => {
  let store;
  let client;

  beforeEach(() => {
    store = createTempStore();
    client = new Fals3S3Client(store.s3);
  });
  afterEach(() => { store.cleanup(); });

  // ── Construction ─────────────────────────────────────────────────────────

  it('rejects construction without a Fals3 instance', () => {
    expect(() => new Fals3S3Client(null)).toThrow(TypeError);
    expect(() => new Fals3S3Client({})).toThrow(TypeError);
  });

  it('exposes config + middlewareStack stubs', () => {
    expect(client.config.region).toBe('fals3y');
    expect(typeof client.middlewareStack.add).toBe('function');
    expect(() => client.destroy()).not.toThrow();
  });

  // ── Bucket commands ──────────────────────────────────────────────────────

  it('CreateBucket / HeadBucket / DeleteBucket roundtrip', async () => {
    const created = await client.send(cmd('CreateBucketCommand', { Bucket: 'bkt' }));
    expect(created.Location).toBe('/bkt');
    expect(created.$metadata.httpStatusCode).toBe(200);

    await client.send(cmd('HeadBucketCommand', { Bucket: 'bkt' }));
    await client.send(cmd('DeleteBucketCommand', { Bucket: 'bkt' }));

    await expect(
      client.send(cmd('HeadBucketCommand', { Bucket: 'bkt' })),
    ).rejects.toMatchObject({ name: 'NoSuchBucket' });
  });

  // ── PutObject / GetObject / HeadObject / DeleteObject ────────────────────

  it('PutObject returns ETag in AWS shape', async () => {
    await client.send(cmd('CreateBucketCommand', { Bucket: 'bkt' }));
    const out = await client.send(cmd('PutObjectCommand', {
      Bucket: 'bkt',
      Key: 'hi.txt',
      Body: Buffer.from('hello'),
      ContentType: 'text/plain',
      Metadata: { author: 'alice' },
    }));
    expect(out.ETag).toMatch(/^"[0-9a-f]{32}"$/);
  });

  it('GetObject returns Body with transformToString / transformToByteArray', async () => {
    await client.send(cmd('CreateBucketCommand', { Bucket: 'bkt' }));
    await client.send(cmd('PutObjectCommand', {
      Bucket: 'bkt', Key: 'k', Body: 'hello world', ContentType: 'text/plain',
    }));

    const out = await client.send(cmd('GetObjectCommand', { Bucket: 'bkt', Key: 'k' }));
    expect(await out.Body.transformToString()).toBe('hello world');
    const bytes = await out.Body.transformToByteArray();
    expect(bytes).toBeInstanceOf(Uint8Array);
    expect(out.ContentType).toBe('text/plain');
    expect(out.ContentLength).toBe(11);
    expect(out.LastModified).toBeInstanceOf(Date);
    expect(out.ETag).toMatch(/^"[0-9a-f]{32}"$/);
    expect(out.StorageClass).toBe('STANDARD');
  });

  it('GetObject Range header parses to inclusive byte slice', async () => {
    await client.send(cmd('CreateBucketCommand', { Bucket: 'bkt' }));
    await client.send(cmd('PutObjectCommand', {
      Bucket: 'bkt', Key: 'k', Body: '0123456789',
    }));
    const out = await client.send(cmd('GetObjectCommand', {
      Bucket: 'bkt', Key: 'k', Range: 'bytes=2-5',
    }));
    expect(await out.Body.transformToString()).toBe('2345');
  });

  it('HeadObject returns metadata-only AWS shape', async () => {
    await client.send(cmd('CreateBucketCommand', { Bucket: 'bkt' }));
    await client.send(cmd('PutObjectCommand', {
      Bucket: 'bkt', Key: 'k', Body: 'hi', Metadata: { x: 'y' },
    }));
    const out = await client.send(cmd('HeadObjectCommand', { Bucket: 'bkt', Key: 'k' }));
    expect(out.ContentLength).toBe(2);
    expect(out.Metadata).toEqual({ x: 'y' });
    expect(out.LastModified).toBeInstanceOf(Date);
    expect(out.Body).toBeUndefined();
  });

  it('DeleteObject is idempotent', async () => {
    await client.send(cmd('CreateBucketCommand', { Bucket: 'bkt' }));
    await expect(
      client.send(cmd('DeleteObjectCommand', { Bucket: 'bkt', Key: 'never' })),
    ).resolves.toMatchObject({ $metadata: { httpStatusCode: 200 } });
  });

  // ── Conditional headers via SDK shape ────────────────────────────────────

  it('PutObject IfNoneMatch=* on existing key throws PreconditionFailed', async () => {
    await client.send(cmd('CreateBucketCommand', { Bucket: 'bkt' }));
    await client.send(cmd('PutObjectCommand', { Bucket: 'bkt', Key: 'k', Body: 'a' }));
    await expect(
      client.send(cmd('PutObjectCommand', {
        Bucket: 'bkt', Key: 'k', Body: 'b', IfNoneMatch: '*',
      })),
    ).rejects.toMatchObject({
      name: 'PreconditionFailed',
      $metadata: { httpStatusCode: 412 },
    });
  });

  it('GetObject IfNoneMatch=current-etag throws NotModified', async () => {
    await client.send(cmd('CreateBucketCommand', { Bucket: 'bkt' }));
    const put = await client.send(cmd('PutObjectCommand', {
      Bucket: 'bkt', Key: 'k', Body: 'a',
    }));
    await expect(
      client.send(cmd('GetObjectCommand', {
        Bucket: 'bkt', Key: 'k', IfNoneMatch: put.ETag,
      })),
    ).rejects.toMatchObject({
      name: 'NotModified',
      $metadata: { httpStatusCode: 304 },
    });
  });

  it('GetObject IfModifiedSince accepts a Date object', async () => {
    await client.send(cmd('CreateBucketCommand', { Bucket: 'bkt' }));
    await client.send(cmd('PutObjectCommand', { Bucket: 'bkt', Key: 'k', Body: 'a' }));
    const meta = await client.send(cmd('HeadObjectCommand', { Bucket: 'bkt', Key: 'k' }));
    await expect(
      client.send(cmd('GetObjectCommand', {
        Bucket: 'bkt', Key: 'k', IfModifiedSince: meta.LastModified,
      })),
    ).rejects.toMatchObject({ name: 'NotModified' });
  });

  // ── ListObjectsV2 ────────────────────────────────────────────────────────

  it('ListObjectsV2 returns SDK-shaped Contents + CommonPrefixes', async () => {
    await client.send(cmd('CreateBucketCommand', { Bucket: 'bkt' }));
    await client.send(cmd('PutObjectCommand', { Bucket: 'bkt', Key: 'a/x', Body: 'x' }));
    await client.send(cmd('PutObjectCommand', { Bucket: 'bkt', Key: 'a/y', Body: 'y' }));
    await client.send(cmd('PutObjectCommand', { Bucket: 'bkt', Key: 'root', Body: 'r' }));

    const out = await client.send(cmd('ListObjectsV2Command', {
      Bucket: 'bkt', Delimiter: '/',
    }));
    expect(out.Name).toBe('bkt');
    expect(out.Contents.map((e) => e.Key)).toEqual(['root']);
    expect(out.CommonPrefixes).toEqual([{ Prefix: 'a/' }]);
    expect(out.IsTruncated).toBe(false);
    expect(out.Contents[0].LastModified).toBeInstanceOf(Date);
    expect(out.Contents[0].ETag).toMatch(/^"[0-9a-f]{32}"$/);
  });

  // ── CopyObject ───────────────────────────────────────────────────────────

  it('CopyObject parses CopySource "bucket/key" form', async () => {
    await client.send(cmd('CreateBucketCommand', { Bucket: 'bkt' }));
    await client.send(cmd('PutObjectCommand', {
      Bucket: 'bkt', Key: 'src', Body: 'hello', ContentType: 'text/plain',
    }));
    const out = await client.send(cmd('CopyObjectCommand', {
      Bucket: 'bkt', Key: 'dst', CopySource: 'bkt/src',
    }));
    expect(out.CopyObjectResult.ETag).toMatch(/^"[0-9a-f]{32}"$/);
    expect(out.CopyObjectResult.LastModified).toBeInstanceOf(Date);
    const got = await client.send(cmd('GetObjectCommand', { Bucket: 'bkt', Key: 'dst' }));
    expect(await got.Body.transformToString()).toBe('hello');
  });

  it('CopyObject accepts URL-encoded leading-slash CopySource', async () => {
    await client.send(cmd('CreateBucketCommand', { Bucket: 'bkt' }));
    await client.send(cmd('PutObjectCommand', {
      Bucket: 'bkt', Key: 'a b/c.txt', Body: 'x',
    }));
    await client.send(cmd('CopyObjectCommand', {
      Bucket: 'bkt', Key: 'd', CopySource: '/bkt/a%20b/c.txt',
    }));
    const got = await client.send(cmd('GetObjectCommand', { Bucket: 'bkt', Key: 'd' }));
    expect(await got.Body.transformToString()).toBe('x');
  });

  it('CopyObject MetadataDirective=REPLACE swaps metadata', async () => {
    await client.send(cmd('CreateBucketCommand', { Bucket: 'bkt' }));
    await client.send(cmd('PutObjectCommand', {
      Bucket: 'bkt', Key: 's', Body: 'x',
      ContentType: 'text/plain', Metadata: { owner: 'alice' },
    }));
    await client.send(cmd('CopyObjectCommand', {
      Bucket: 'bkt', Key: 'd', CopySource: 'bkt/s',
      MetadataDirective: 'REPLACE',
      ContentType: 'application/octet-stream',
      Metadata: { owner: 'bob' },
    }));
    const meta = await client.send(cmd('HeadObjectCommand', { Bucket: 'bkt', Key: 'd' }));
    expect(meta.ContentType).toBe('application/octet-stream');
    expect(meta.Metadata).toEqual({ owner: 'bob' });
  });

  // ── Multipart ────────────────────────────────────────────────────────────

  it('Multipart roundtrip via SDK command shape', async () => {
    await client.send(cmd('CreateBucketCommand', { Bucket: 'bkt' }));

    const created = await client.send(cmd('CreateMultipartUploadCommand', {
      Bucket: 'bkt', Key: 'big.bin', ContentType: 'application/octet-stream',
    }));
    expect(created.UploadId).toBeTruthy();

    const p1 = await client.send(cmd('UploadPartCommand', {
      Bucket: 'bkt', Key: 'big.bin', UploadId: created.UploadId,
      PartNumber: 1, Body: Buffer.from('hello '),
    }));
    const p2 = await client.send(cmd('UploadPartCommand', {
      Bucket: 'bkt', Key: 'big.bin', UploadId: created.UploadId,
      PartNumber: 2, Body: Buffer.from('world'),
    }));

    const listed = await client.send(cmd('ListPartsCommand', {
      Bucket: 'bkt', Key: 'big.bin', UploadId: created.UploadId,
    }));
    expect(listed.Parts.map((p) => p.PartNumber)).toEqual([1, 2]);

    const done = await client.send(cmd('CompleteMultipartUploadCommand', {
      Bucket: 'bkt', Key: 'big.bin', UploadId: created.UploadId,
      MultipartUpload: { Parts: [
        { PartNumber: 1, ETag: p1.ETag },
        { PartNumber: 2, ETag: p2.ETag },
      ] },
    }));
    expect(done.ETag).toMatch(/^"[0-9a-f]{32}-2"$/);

    const got = await client.send(cmd('GetObjectCommand', { Bucket: 'bkt', Key: 'big.bin' }));
    expect(await got.Body.transformToString()).toBe('hello world');
  });

  it('AbortMultipartUpload removes in-flight state', async () => {
    await client.send(cmd('CreateBucketCommand', { Bucket: 'bkt' }));
    const created = await client.send(cmd('CreateMultipartUploadCommand', {
      Bucket: 'bkt', Key: 'k',
    }));
    await client.send(cmd('UploadPartCommand', {
      Bucket: 'bkt', Key: 'k', UploadId: created.UploadId,
      PartNumber: 1, Body: Buffer.from('x'),
    }));
    await client.send(cmd('AbortMultipartUploadCommand', {
      Bucket: 'bkt', Key: 'k', UploadId: created.UploadId,
    }));
    await expect(
      client.send(cmd('ListPartsCommand', {
        Bucket: 'bkt', Key: 'k', UploadId: created.UploadId,
      })),
    ).rejects.toMatchObject({ name: 'NoSuchUpload' });
  });

  // ── Error mapping ────────────────────────────────────────────────────────

  it('rejects with AWS-shaped error: name, $fault, $metadata.httpStatusCode', async () => {
    await client.send(cmd('CreateBucketCommand', { Bucket: 'bkt' }));
    let caught;
    try {
      await client.send(cmd('GetObjectCommand', { Bucket: 'bkt', Key: 'ghost' }));
    } catch (e) { caught = e; }
    expect(caught).toBeInstanceOf(Error);
    expect(caught.name).toBe('NoSuchKey');
    expect(caught.Code).toBe('NoSuchKey');
    expect(caught.$fault).toBe('client');
    expect(caught.$metadata.httpStatusCode).toBe(404);
  });

  // ── Unsupported / extension surface ──────────────────────────────────────

  it('throws UnsupportedCommand for unknown command names', async () => {
    await expect(
      client.send(cmd('SelectObjectContentCommand', { Bucket: 'b' })),
    ).rejects.toMatchObject({ name: 'UnsupportedCommand' });
  });

  it('registerCommand allows extending the dispatcher', async () => {
    Fals3S3Client.registerCommand('PingCommand', (_s3, input) => ({ Pong: input.Value }));
    const out = await client.send(cmd('PingCommand', { Value: 42 }));
    expect(out.Pong).toBe(42);
    expect(out.$metadata.httpStatusCode).toBe(200);
  });
});
