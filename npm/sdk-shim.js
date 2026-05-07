'use strict';

/**
 * fals3/sdk-shim — drop-in replacement for `@aws-sdk/client-s3`'s `S3Client`,
 * backed by a local `Fals3` instance.
 *
 * Lets test code that already uses the AWS SDK v3 command pattern
 *
 *     await s3.send(new GetObjectCommand({ Bucket, Key }))
 *
 * run against fals3 unchanged: only the client construction line differs.
 *
 * Dispatch is by `command.constructor.name` so we never take a runtime
 * dependency on `@aws-sdk/client-s3`.  Anything that exposes
 * `{ constructor: { name }, input }` works — including hand-built test
 * doubles.
 */

const { Readable } = require('node:stream');

// ─── Error mapping ───────────────────────────────────────────────────────────

const CODE_TO_HTTP_STATUS = {
  NoSuchBucket: 404,
  BucketAlreadyExists: 409,
  BucketNotEmpty: 409,
  InvalidBucketName: 400,
  NoSuchKey: 404,
  InvalidObjectKey: 400,
  PreconditionFailed: 412,
  NotModified: 304,
  NoSuchUpload: 404,
  InvalidPart: 400,
  InvalidPartOrder: 400,
  PathEscape: 400,
  InternalError: 500,
};

/**
 * Re-shape a fals3 error so it matches what an AWS SDK v3 consumer expects:
 * `err.name` carries the AWS-style code, `err.$metadata.httpStatusCode` is
 * populated, and the `$fault` field is set.
 */
function rewrapAsAwsError(err) {
  if (!err || typeof err !== 'object' || typeof err.code !== 'string') {
    return err;
  }
  const code = err.code;
  err.name = code;
  err.Code = code;
  err.$fault = 'client';
  err.$metadata = err.$metadata || {};
  err.$metadata.httpStatusCode = CODE_TO_HTTP_STATUS[code] ?? 500;
  err.$metadata.requestId = err.$metadata.requestId || 'fals3-shim';
  return err;
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

function normalizeBody(body) {
  if (body == null) return Buffer.alloc(0);
  if (Buffer.isBuffer(body)) return body;
  if (body instanceof Uint8Array) return Buffer.from(body);
  if (typeof body === 'string') return Buffer.from(body);
  throw new TypeError(
    'fals3y/sdk-shim: Body must be a Buffer, Uint8Array, or string. ' +
    'Streams must be resolved by the caller before calling send().',
  );
}

function bodyResponse(buffer) {
  // Mimic AWS SDK v3's `SdkStream<Readable>` shape: a Readable with the
  // `transformToString` / `transformToByteArray` / `transformToWebStream`
  // helpers attached.
  const stream = Readable.from(buffer);
  stream.transformToString = async (encoding = 'utf8') => buffer.toString(encoding);
  stream.transformToByteArray = async () => new Uint8Array(buffer);
  stream.transformToWebStream = () => Readable.toWeb(Readable.from(buffer));
  return stream;
}

function metaToSdkOutput(meta) {
  return {
    ETag: meta.etag,
    LastModified: new Date(meta.lastModified * 1000),
    ContentType: meta.contentType,
    Metadata: meta.userMetadata,
    ContentEncoding: meta.contentEncoding,
    StorageClass: meta.storageClass,
    ContentLength: meta.size,
  };
}

function parseRangeHeader(range) {
  if (!range) return { rangeStart: null, rangeEnd: null };
  const m = /^bytes=(\d+)-(\d+)$/.exec(range);
  if (!m) {
    throw Object.assign(new Error(`Unsupported Range value: ${range}`), {
      name: 'InvalidArgument',
    });
  }
  return { rangeStart: Number(m[1]), rangeEnd: Number(m[2]) };
}

function parseCopySource(source) {
  // AWS accepts "bucket/key" or "/bucket/key", URL-encoded.
  let s = String(source);
  if (s.startsWith('/')) s = s.slice(1);
  try { s = decodeURIComponent(s); } catch { /* leave as-is */ }
  const slash = s.indexOf('/');
  if (slash < 0) {
    throw Object.assign(new Error(`Invalid CopySource: ${source}`), {
      name: 'InvalidArgument',
    });
  }
  return { srcBucket: s.slice(0, slash), srcKey: s.slice(slash + 1) };
}

function dateToUnixSeconds(d) {
  if (d == null) return undefined;
  const ms = d instanceof Date ? d.getTime() : new Date(d).getTime();
  if (Number.isNaN(ms)) return undefined;
  return Math.floor(ms / 1000);
}

function pickConditions(input) {
  const c = {};
  if (input.IfMatch != null) c.ifMatch = input.IfMatch;
  if (input.IfNoneMatch != null) c.ifNoneMatch = input.IfNoneMatch;
  const ims = dateToUnixSeconds(input.IfModifiedSince);
  if (ims != null) c.ifModifiedSince = ims;
  const ius = dateToUnixSeconds(input.IfUnmodifiedSince);
  if (ius != null) c.ifUnmodifiedSince = ius;
  return Object.keys(c).length ? c : null;
}

function pickSourceConditions(input) {
  const c = {};
  if (input.CopySourceIfMatch != null) c.ifMatch = input.CopySourceIfMatch;
  if (input.CopySourceIfNoneMatch != null) c.ifNoneMatch = input.CopySourceIfNoneMatch;
  const ims = dateToUnixSeconds(input.CopySourceIfModifiedSince);
  if (ims != null) c.ifModifiedSince = ims;
  const ius = dateToUnixSeconds(input.CopySourceIfUnmodifiedSince);
  if (ius != null) c.ifUnmodifiedSince = ius;
  return Object.keys(c).length ? c : null;
}

// ─── Command handlers ────────────────────────────────────────────────────────
//
// Each handler runs synchronously (the underlying fals3 calls are sync).
// `send()` wraps the result in a Promise so the SDK consumer sees a thenable.

const HANDLERS = {
  CreateBucketCommand(s3, input) {
    s3.createBucket(input.Bucket);
    return { Location: `/${input.Bucket}` };
  },
  HeadBucketCommand(s3, input) {
    s3.headBucket(input.Bucket);
    return {};
  },
  DeleteBucketCommand(s3, input) {
    s3.deleteBucket(input.Bucket);
    return {};
  },

  PutObjectCommand(s3, input) {
    const body = normalizeBody(input.Body);
    const etag = s3.putObject(
      input.Bucket,
      input.Key,
      body,
      input.ContentType ?? null,
      input.Metadata ?? null,
      input.ContentEncoding ?? null,
      pickConditions(input),
    );
    return { ETag: etag };
  },

  GetObjectCommand(s3, input) {
    const { rangeStart, rangeEnd } = parseRangeHeader(input.Range);
    const out = s3.getObject(
      input.Bucket,
      input.Key,
      rangeStart,
      rangeEnd,
      pickConditions(input),
    );
    return {
      ...metaToSdkOutput(out.meta),
      Body: bodyResponse(out.body),
      ContentLength: out.body.length,
    };
  },

  HeadObjectCommand(s3, input) {
    const meta = s3.headObject(input.Bucket, input.Key, pickConditions(input));
    return metaToSdkOutput(meta);
  },

  DeleteObjectCommand(s3, input) {
    s3.deleteObject(input.Bucket, input.Key);
    return {};
  },

  ListObjectsV2Command(s3, input) {
    const out = s3.listObjectsV2(
      input.Bucket,
      input.Prefix ?? null,
      input.Delimiter ?? null,
      input.MaxKeys ?? null,
      input.ContinuationToken ?? null,
    );
    return {
      Name: input.Bucket,
      Prefix: input.Prefix,
      Delimiter: input.Delimiter,
      MaxKeys: input.MaxKeys,
      Contents: out.contents.map((e) => ({
        Key: e.key,
        ETag: e.etag,
        Size: e.size,
        LastModified: new Date(e.lastModified * 1000),
        StorageClass: e.storageClass,
      })),
      CommonPrefixes: out.commonPrefixes.map((p) => ({ Prefix: p })),
      IsTruncated: out.isTruncated,
      NextContinuationToken: out.nextContinuationToken,
      ContinuationToken: input.ContinuationToken,
      KeyCount: out.keyCount,
    };
  },

  CopyObjectCommand(s3, input) {
    const { srcBucket, srcKey } = parseCopySource(input.CopySource);
    const replace = input.MetadataDirective === 'REPLACE';
    const out = s3.copyObject(
      srcBucket,
      srcKey,
      input.Bucket,
      input.Key,
      replace ? (input.ContentType ?? null) : null,
      replace ? (input.Metadata ?? null) : null,
      pickSourceConditions(input),
    );
    return {
      CopyObjectResult: {
        ETag: out.etag,
        LastModified: new Date(out.lastModified * 1000),
      },
    };
  },

  CreateMultipartUploadCommand(s3, input) {
    const out = s3.createMultipartUpload(
      input.Bucket,
      input.Key,
      input.ContentType ?? null,
      input.Metadata ?? null,
      input.ContentEncoding ?? null,
    );
    return { Bucket: out.bucket, Key: out.key, UploadId: out.uploadId };
  },

  UploadPartCommand(s3, input) {
    const body = normalizeBody(input.Body);
    const out = s3.uploadPart(input.UploadId, input.PartNumber, body);
    return { ETag: out.etag };
  },

  CompleteMultipartUploadCommand(s3, input) {
    const parts = (input.MultipartUpload?.Parts ?? []).map((p) => ({
      partNumber: p.PartNumber,
      etag: p.ETag,
    }));
    const out = s3.completeMultipartUpload(input.UploadId, parts);
    return { Bucket: out.bucket, Key: out.key, ETag: out.etag };
  },

  AbortMultipartUploadCommand(s3, input) {
    s3.abortMultipartUpload(input.UploadId);
    return {};
  },

  ListPartsCommand(s3, input) {
    const out = s3.listParts(input.UploadId);
    return {
      Bucket: out.bucket,
      Key: out.key,
      Parts: out.parts.map((p) => ({
        PartNumber: p.partNumber,
        ETag: p.etag,
        Size: p.size,
        LastModified: new Date(p.lastModified * 1000),
      })),
    };
  },
};

// ─── Public class ────────────────────────────────────────────────────────────

/**
 * Drop-in replacement for `@aws-sdk/client-s3`'s `S3Client`, backed by a
 * local `Fals3` instance.  Implements `send(command)` and the minimum
 * surface (`config`, `destroy`, `middlewareStack`) required for typical
 * SDK consumers.
 */
class Fals3S3Client {
  constructor(fals3, options = {}) {
    if (!fals3 || typeof fals3.putObject !== 'function') {
      throw new TypeError('Fals3S3Client requires a Fals3 instance');
    }
    this._fals3 = fals3;
    this.config = {
      region: options.region ?? 'fals3y',
      endpoint: 'fals3://local',
      requestHandler: { destroy: () => {} },
      ...options,
    };
    // Minimum middleware-stack stub so SDK utilities that mutate it do not crash.
    this.middlewareStack = {
      add: () => {},
      remove: () => {},
      use: () => {},
      clone: () => this.middlewareStack,
    };
  }

  /**
   * Register or replace a handler for a command name.  Useful if a test
   * needs to stub a command we don't yet support, or override behaviour.
   */
  static registerCommand(name, handler) {
    if (typeof name !== 'string' || typeof handler !== 'function') {
      throw new TypeError('registerCommand(name, handler) — bad arguments');
    }
    HANDLERS[name] = handler;
  }

  async send(command) {
    if (!command || typeof command !== 'object' || command.input == null) {
      throw new TypeError('Fals3S3Client.send: expected an SDK command object');
    }
    const name = command.constructor && command.constructor.name;
    const handler = HANDLERS[name];
    if (!handler) {
      const err = new Error(
        `Fals3S3Client: unsupported command "${name ?? 'unknown'}". ` +
        `Use Fals3S3Client.registerCommand(name, handler) to add support.`,
      );
      err.name = 'UnsupportedCommand';
      throw err;
    }
    try {
      const result = handler(this._fals3, command.input);
      return {
        ...result,
        $metadata: { httpStatusCode: 200, requestId: 'fals3-shim' },
      };
    } catch (err) {
      throw rewrapAsAwsError(err);
    }
  }

  destroy() { /* no-op for SDK parity */ }
}

module.exports = { Fals3S3Client };
