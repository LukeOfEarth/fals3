/**
 * fals3 — S3 behavioral simulator for local testing.
 *
 * All methods are synchronous (the Rust layer is sync-over-fs).
 *
 * Every thrown error is a real `Error` instance with:
 * - `err.code` — AWS-style error code (e.g. `"NoSuchBucket"`, `"NoSuchKey"`,
 *   `"BucketAlreadyExists"`, `"BucketNotEmpty"`, `"InvalidBucketName"`,
 *   `"InvalidObjectKey"`, `"PathEscape"`, `"InternalError"`).
 * - `err.message` — `"[<code>] <human description>"` (the code is mirrored
 *   into the message prefix so it appears in stack traces and logs).
 *
 * ```ts
 * try {
 *   s3.headBucket('missing');
 * } catch (err) {
 *   if (err.code === 'NoSuchBucket') { ... }
 * }
 * ```
 */

/** Union of AWS-style error codes that fals3 can throw on `err.code`. */
export type Fals3ErrorCode =
  | 'NoSuchBucket'
  | 'BucketAlreadyExists'
  | 'BucketNotEmpty'
  | 'InvalidBucketName'
  | 'NoSuchKey'
  | 'InvalidObjectKey'
  | 'PreconditionFailed'
  | 'NotModified'
  | 'NoSuchUpload'
  | 'InvalidPart'
  | 'InvalidPartOrder'
  | 'PathEscape'
  | 'InternalError';

/**
 * HTTP-style precondition headers.  Accepted by `getObject`, `headObject`,
 * `putObject` (write-side), and `copyObject` (source-side).
 *
 * On reads (`get`, `head`, copy source):
 * - `ifMatch` / `ifUnmodifiedSince` failure → `err.code === 'PreconditionFailed'`
 * - `ifNoneMatch` / `ifModifiedSince` failure → `err.code === 'NotModified'`
 *
 * On writes (`put`):
 * - `ifNoneMatch: '*'` → object must not exist (atomic create)
 * - `ifMatch: <etag>` → current object must have this ETag (optimistic concurrency)
 * - Failure → `err.code === 'PreconditionFailed'`
 *
 * ETag values are matched case-sensitively after stripping surrounding double
 * quotes, so `'"abc"'`, `'abc'`, and `'  "abc"  '` are equivalent. The
 * wildcard `'*'` matches any existing object.
 */
export interface IfConditions {
  ifMatch?: string;
  ifNoneMatch?: string;
  /** Unix timestamp in seconds. */
  ifModifiedSince?: number;
  /** Unix timestamp in seconds. */
  ifUnmodifiedSince?: number;
}

/** Options passed to {@link Fals3.open}. */
export interface OpenOptions {
  /** Root directory for all buckets and objects. Created if it does not exist. */
  baseDir: string;
}

/** Metadata stored alongside every object. */
export interface ObjectMeta {
  /** Strong ETag: MD5 hex wrapped in double-quotes, e.g. `"d41d8cd98f00b204e9800998ecf8427e"`. */
  etag: string;
  /** Unix timestamp (seconds) when the object was last written. */
  lastModified: number;
  /** Absent (`undefined`) when the object was put without a Content-Type. */
  contentType: string | undefined;
  userMetadata: Record<string, string>;
  contentEncoding: string | undefined;
  /** Always `"STANDARD"` in v1. */
  storageClass: string;
  /** Object size in bytes. */
  size: number;
}

/** A single object entry returned by {@link Fals3.listObjectsV2}. */
export interface ObjectEntry {
  key: string;
  etag: string;
  size: number;
  lastModified: number;
  storageClass: string;
}

/** Response from {@link Fals3.listObjectsV2}. */
export interface ListObjectsV2Output {
  contents: ObjectEntry[];
  /** Virtual-directory prefixes collapsed by the delimiter. */
  commonPrefixes: string[];
  isTruncated: boolean;
  /** Present when `isTruncated` is true; pass as `continuationToken` to get the next page. */
  nextContinuationToken: string | undefined;
  keyCount: number;
}

/** Response from {@link Fals3.getObject}. */
export interface GetObjectOutput {
  body: Buffer;
  meta: ObjectMeta;
}

/** Response from {@link Fals3.copyObject}. */
export interface CopyObjectOutput {
  etag: string;
  lastModified: number;
}

/** Response from {@link Fals3.createMultipartUpload}. */
export interface CreateMultipartUploadOutput {
  bucket: string;
  key: string;
  /** Opaque identifier — pass to subsequent multipart calls. */
  uploadId: string;
}

/** Response from {@link Fals3.uploadPart}. */
export interface UploadPartOutput {
  partNumber: number;
  /** MD5 ETag of the part body — pass back in `completeMultipartUpload`. */
  etag: string;
}

/** Element of the `parts` array passed to {@link Fals3.completeMultipartUpload}. */
export interface CompletedPart {
  partNumber: number;
  etag: string;
}

/** Response from {@link Fals3.completeMultipartUpload}. */
export interface CompleteMultipartUploadOutput {
  bucket: string;
  key: string;
  /** AWS-style multipart ETag: `"<md5-of-md5s>-<part-count>"`. */
  etag: string;
}

/** A single part returned by {@link Fals3.listParts}. */
export interface PartEntry {
  partNumber: number;
  etag: string;
  size: number;
  lastModified: number;
}

/** Response from {@link Fals3.listParts}. */
export interface ListPartsOutput {
  bucket: string;
  key: string;
  parts: PartEntry[];
}

/**
 * S3 simulator class.
 *
 * @example
 * ```ts
 * import { Fals3 } from 'fals3';
 *
 * const s3 = Fals3.open({ baseDir: '/tmp/fals3-test' });
 * s3.createBucket('my-bucket');
 * s3.putObject('my-bucket', 'hello.txt', Buffer.from('hello'), 'text/plain');
 * const { body } = s3.getObject('my-bucket', 'hello.txt');
 * console.log(body.toString()); // "hello"
 * ```
 */
export declare class Fals3 {
  /**
   * Open (or create) a Fals3 instance rooted at `options.baseDir`.
   * The directory is created if it does not exist.
   */
  static open(options: OpenOptions): Fals3;

  // ── Bucket operations ──────────────────────────────────────────────────

  /** Create a bucket. Throws `BucketAlreadyExists` if it already exists. */
  createBucket(bucket: string): void;

  /**
   * Delete a bucket.
   * Throws `BucketNotEmpty` if the bucket contains objects and `force` is false.
   */
  deleteBucket(bucket: string, force?: boolean): void;

  /** Check that a bucket exists. Throws `NoSuchBucket` if it doesn't. */
  headBucket(bucket: string): void;

  // ── Object operations ──────────────────────────────────────────────────

  /**
   * Write an object. Returns the ETag.
   *
   * @param metadata    User-defined key/value pairs (`x-amz-meta-*` equivalent).
   * @param conditions  Optional preconditions. Use `{ ifNoneMatch: '*' }` for
   *                    atomic create, or `{ ifMatch: <etag> }` for optimistic
   *                    concurrency. Failed preconditions throw with
   *                    `err.code === 'PreconditionFailed'`.
   */
  putObject(
    bucket: string,
    key: string,
    body: Buffer | Uint8Array,
    contentType?: string | null,
    metadata?: Record<string, string> | null,
    contentEncoding?: string | null,
    conditions?: IfConditions | null,
  ): string;

  /**
   * Read an object body and metadata.
   *
   * @param rangeStart  Inclusive byte start (requires `rangeEnd`).
   * @param rangeEnd    Inclusive byte end.
   * @param conditions  Optional preconditions. See {@link IfConditions}.
   */
  getObject(
    bucket: string,
    key: string,
    rangeStart?: number | null,
    rangeEnd?: number | null,
    conditions?: IfConditions | null,
  ): GetObjectOutput;

  /** Return metadata only (no body). */
  headObject(
    bucket: string,
    key: string,
    conditions?: IfConditions | null,
  ): ObjectMeta;

  /**
   * Delete an object. Idempotent — deleting a non-existent key is a no-op
   * (matches AWS `DeleteObject` returning 204 even when the key was absent).
   */
  deleteObject(bucket: string, key: string): void;

  /**
   * List objects in a bucket (S3 ListObjectsV2 semantics).
   *
   * @param prefix             Only return keys beginning with this string.
   * @param delimiter          Collapse keys containing this delimiter after the
   *                           prefix into `commonPrefixes`.
   * @param maxKeys            Page size (1–1000, default 1000).
   * @param continuationToken  Resume from a previous truncated response.
   */
  listObjectsV2(
    bucket: string,
    prefix?: string | null,
    delimiter?: string | null,
    maxKeys?: number | null,
    continuationToken?: string | null,
  ): ListObjectsV2Output;

  /**
   * Copy an object within or across buckets.
   *
   * By default, source metadata is preserved (COPY directive).
   * Pass `replaceContentType` or `replaceMetadata` to use the REPLACE directive.
   *
   * `sourceConditions` (mirrors S3 `x-amz-copy-source-if-*` headers) is
   * evaluated against the *source* object before copying.
   */
  copyObject(
    srcBucket: string,
    srcKey: string,
    dstBucket: string,
    dstKey: string,
    replaceContentType?: string | null,
    replaceMetadata?: Record<string, string> | null,
    sourceConditions?: IfConditions | null,
  ): CopyObjectOutput;

  // ── Multipart upload ──────────────────────────────────────────────────

  /**
   * Begin a multipart upload.  Returns an `uploadId` to pass to subsequent
   * `uploadPart`, `listParts`, `completeMultipartUpload`, and
   * `abortMultipartUpload` calls.
   *
   * Nothing is written into the destination key until
   * `completeMultipartUpload` succeeds.
   */
  createMultipartUpload(
    bucket: string,
    key: string,
    contentType?: string | null,
    metadata?: Record<string, string> | null,
    contentEncoding?: string | null,
  ): CreateMultipartUploadOutput;

  /**
   * Upload one part of an in-flight multipart upload.
   *
   * `partNumber` must be in `1..=10000`.  Re-uploading the same part number
   * replaces the previous body (last-writer-wins, matching S3).
   */
  uploadPart(
    uploadId: string,
    partNumber: number,
    body: Buffer | Uint8Array,
  ): UploadPartOutput;

  /**
   * Finalise a multipart upload.  `parts` must be in strictly ascending
   * `partNumber` order; each `(partNumber, etag)` must match a previously
   * uploaded part.
   *
   * Throws `err.code === 'NoSuchUpload'`, `'InvalidPartOrder'`, or
   * `'InvalidPart'` on validation failure.
   */
  completeMultipartUpload(
    uploadId: string,
    parts: CompletedPart[],
  ): CompleteMultipartUploadOutput;

  /**
   * Cancel an in-flight multipart upload and discard all uploaded parts.
   * Idempotent — aborting an unknown `uploadId` succeeds silently.
   */
  abortMultipartUpload(uploadId: string): void;

  /** List the parts already uploaded for an in-flight multipart upload. */
  listParts(uploadId: string): ListPartsOutput;

  /** Return the fals3-node package version. */
  static version(): string;
}
