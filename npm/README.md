# fals3

**S3 behavioral simulator for local testing.** Pure-Rust core compiled to a Node.js native addon. Runs in-process, no network, no Docker.

`fals3` materialises buckets and objects on the local filesystem and gives you the same API surface and error shapes you'd assert against real S3 — so your tests don't have to care whether they're hitting AWS, LocalStack, or this.

## Why

| Compared to | What `fals3` gives you |
|-------------|------------------------|
| **LocalStack / MinIO** | No container, no port, no boot time. In-process. |
| **`aws-sdk-client-mock`** | Real put/get/list/copy semantics with actual bytes on disk — not stubbed return values. |
| **A custom in-memory mock** | S3-compatible error codes, ETags (MD5-hex), key validation, ListV2 pagination + delimiters, sidecar metadata. |

## Install

```sh
npm install --save-dev fals3
```

Prebuilt binaries for `darwin-arm64`, `linux-x64-gnu`, and `win32-x64-msvc` install via `optionalDependencies`. No Rust toolchain required to consume the package.

## Quickstart

```ts
import { Fals3 } from 'fals3';
import { createTempStore } from 'fals3/helpers';

const { s3, cleanup } = createTempStore();

s3.createBucket('my-uploads');
s3.putObject('my-uploads', 'users/1/avatar.png', Buffer.from('...'), 'image/png', {
  uploadedBy: 'alice',
});

const { body, meta } = s3.getObject('my-uploads', 'users/1/avatar.png');
console.log(body.length, meta.etag, meta.userMetadata.uploadedBy);

cleanup(); // remove the temp directory
```

## API

All methods are **synchronous**. They throw on error; see [Error handling](#error-handling).

### Open a store

```ts
import { Fals3 } from 'fals3';

const s3 = Fals3.open({ baseDir: '/tmp/fals3-test' });
```

The base directory is created if missing and canonicalised. Every bucket lives at `{baseDir}/{bucket}/`, every object at `{baseDir}/{bucket}/{key}`, with a `{path}.fals3-meta.json` sidecar holding metadata.

### Buckets

| Method | Throws |
|--------|--------|
| `s3.createBucket(name)` | `BucketAlreadyExists`, `InvalidBucketName` |
| `s3.headBucket(name)` | `NoSuchBucket` |
| `s3.deleteBucket(name, force?)` | `NoSuchBucket`, `BucketNotEmpty` (when `force !== true`) |

Bucket names follow S3 DNS rules (3–63 chars, lowercase alphanumeric + hyphen, no leading/trailing hyphen).

### Objects

```ts
const etag = s3.putObject(
  bucket,
  key,
  body,                  // Buffer | Uint8Array
  contentType?,          // string | null
  userMetadata?,         // Record<string,string> | null  (x-amz-meta-* equivalent)
  contentEncoding?,      // string | null
);

const { body, meta } = s3.getObject(bucket, key, rangeStart?, rangeEnd?);
const meta            = s3.headObject(bucket, key);
                        s3.deleteObject(bucket, key);   // idempotent
```

`rangeStart` / `rangeEnd` are inclusive byte offsets, S3-style. `rangeEnd` is clamped to the body length, so `(0, 9999)` on a 10-byte object returns all 10 bytes.

`putObject` returns the ETag — a strong MD5-hex wrapped in double-quotes, e.g. `"5d41402abc4b2a76b9719d911017c592"`. Identical to what real S3 returns for single-part uploads.

### ListObjectsV2

```ts
const out = s3.listObjectsV2(bucket, prefix?, delimiter?, maxKeys?, continuationToken?);

out.contents             // ObjectEntry[]
out.commonPrefixes       // string[]    (when delimiter is set)
out.isTruncated          // boolean
out.nextContinuationToken // string | undefined
out.keyCount             // number
```

Page through everything:

```ts
let token: string | undefined;
do {
  const page = s3.listObjectsV2(bucket, undefined, undefined, 1000, token);
  for (const entry of page.contents) {
    // ...
  }
  token = page.nextContinuationToken;
} while (token);
```

### CopyObject

```ts
// Preserve source metadata (COPY directive).
s3.copyObject(srcBucket, srcKey, dstBucket, dstKey);

// Replace metadata (REPLACE directive). Pass either argument to switch.
s3.copyObject(srcBucket, srcKey, dstBucket, dstKey,
  'application/octet-stream',
  { replacedBy: 'bob' },
);
```

Same-bucket and cross-bucket copies are supported as long as both buckets live under the same `baseDir`.

### Multipart upload

S3 multipart is supported via five methods that mirror the AWS API:

```ts
const created = s3.createMultipartUpload('bkt', 'big.bin', 'application/octet-stream');

const p1 = s3.uploadPart(created.uploadId, 1, chunk1); // → { partNumber, etag }
const p2 = s3.uploadPart(created.uploadId, 2, chunk2);

const done = s3.completeMultipartUpload(created.uploadId, [
  { partNumber: 1, etag: p1.etag },
  { partNumber: 2, etag: p2.etag },
]);
// done.etag is the AWS multipart ETag: "<md5-of-md5s>-<part-count>"

// Or cancel and discard everything uploaded so far:
s3.abortMultipartUpload(created.uploadId);

// Inspect what's been uploaded mid-flight:
const inflight = s3.listParts(created.uploadId);
```

**Semantics:**
- `partNumber` must be in `1..=10000`. Re-uploading the same number replaces the previous body (last writer wins, matching S3).
- `completeMultipartUpload` requires `parts` in strictly ascending `partNumber` order. Out-of-order or duplicate part numbers throw `err.code === 'InvalidPartOrder'`.
- Each `(partNumber, etag)` in the completion list must match a part that was actually uploaded; mismatches throw `err.code === 'InvalidPart'`.
- `abortMultipartUpload` is idempotent — calling it on an unknown `uploadId` succeeds silently, matching AWS behaviour.
- The completed object's ETag is the multipart ETag (`"<32-hex>-<N>"`), distinct from the single-part ETag (`"<32-hex>"`).
- In-flight upload state lives under `{baseDir}/.fals3-uploads/{uploadId}/` and is cleaned up by `completeMultipartUpload` / `abortMultipartUpload`. The leading `.` ensures it can never collide with a real bucket name.

Unlike real S3, fals3 does **not** enforce the 5 MiB minimum part size for non-final parts — tests with small parts work as expected.

### Conditional headers (preconditions)

`getObject`, `headObject`, `putObject`, and `copyObject` accept an optional `IfConditions` object that mirrors the S3 HTTP precondition headers:

```ts
interface IfConditions {
  ifMatch?: string;            // succeed only if current ETag matches (or '*')
  ifNoneMatch?: string;        // read: 304 if matches; write: '*' = must not exist
  ifModifiedSince?: number;    // unix seconds — read: 304 if not modified after
  ifUnmodifiedSince?: number;  // unix seconds — read/write: 412 if newer
}
```

**Atomic create** (S3 `If-None-Match: *` on `Put`):

```ts
try {
  s3.putObject('bkt', 'k', body, null, null, null, { ifNoneMatch: '*' });
} catch (err) {
  if (err.code === 'PreconditionFailed') {
    // The key already exists — abort or retry under different name.
  }
}
```

**Optimistic concurrency** (read-modify-write with ETag check):

```ts
const { body, meta } = s3.getObject('bkt', 'k');
const next = mutate(body);
try {
  s3.putObject('bkt', 'k', next, null, null, null, { ifMatch: meta.etag });
} catch (err) {
  if (err.code === 'PreconditionFailed') {
    // Someone else updated 'k' since we read it; retry.
  }
}
```

**Conditional GET** (avoid re-downloading unchanged objects):

```ts
try {
  const { body } = s3.getObject('bkt', 'k', null, null, { ifNoneMatch: cachedEtag });
  // body is fresh — replace cache.
} catch (err) {
  if (err.code === 'NotModified') {
    // Cache still good.
  }
}
```

ETag values may be supplied with or without surrounding double quotes. The wildcard `'*'` matches any existing object. Evaluation order matches S3: `ifMatch` / `ifUnmodifiedSince` (412 PreconditionFailed) take priority over `ifNoneMatch` / `ifModifiedSince` (304 NotModified) when both fire.

## Test helpers

Non-S3 utilities for tests, exported from `fals3/helpers`:

```ts
import { createTempStore, reset, listAll, snapshot } from 'fals3/helpers';

const { s3, baseDir, cleanup } = createTempStore();
// ... run tests ...
cleanup();

// Wipe between tests but reuse the instance.
reset(s3, baseDir);

// Debug-only: enumerate every object in every bucket.
const all = listAll(s3, baseDir);

// Snapshot for `expect(...).toMatchSnapshot()`.
expect(snapshot(s3, baseDir)).toMatchSnapshot();
```

### Vitest pattern

```ts
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { createTempStore } from 'fals3/helpers';

describe('upload pipeline', () => {
  let store: ReturnType<typeof createTempStore>;

  beforeEach(() => { store = createTempStore(); });
  afterEach(() => { store.cleanup(); });

  it('uploads avatar', () => {
    store.s3.createBucket('avatars');
    yourCode(store.s3); // pass the fals3 instance into the code under test
    expect(store.s3.headObject('avatars', 'expected-key').size).toBeGreaterThan(0);
  });
});
```

### Jest pattern

Same as above with `import { afterEach, beforeEach, describe, expect, it, jest } from '@jest/globals';`.

## Error handling

Every method throws on failure. The thrown value is a real `Error` instance with two stable fields:

- **`err.code`** — AWS-style error code, e.g. `"NoSuchBucket"`, `"NoSuchKey"`, `"BucketAlreadyExists"`, `"BucketNotEmpty"`, `"InvalidBucketName"`, `"InvalidObjectKey"`, `"PathEscape"`, `"InternalError"`.
- **`err.message`** — `"[<code>] <human description>"`. The code is mirrored into the message prefix so it shows up in stack traces and log lines.

```ts
try {
  s3.headBucket('ghost');
} catch (err) {
  err.code;     // "NoSuchBucket"
  err.message;  // "[NoSuchBucket] The specified bucket does not exist"
}
```

Assert in tests:

```ts
expect(() => s3.headBucket('ghost')).toThrow(
  expect.objectContaining({ code: 'NoSuchBucket' }),
);
```

Or against the message:

```ts
expect(() => s3.headBucket('ghost')).toThrowError(/^\[NoSuchBucket\]/);
```

The full code list is exported as the `Fals3ErrorCode` type from `fals3`.

## Integrating with the AWS SDK v3

`fals3/sdk-shim` ships a `Fals3S3Client` that's a drop-in replacement for `@aws-sdk/client-s3`'s `S3Client`. Test code that already uses the SDK command pattern works unchanged — only the client construction line differs.

```ts
import { Fals3 } from 'fals3';
import { Fals3S3Client } from 'fals3/sdk-shim';
import {
  GetObjectCommand,
  PutObjectCommand,
  ListObjectsV2Command,
} from '@aws-sdk/client-s3';

const fals3 = Fals3.open({ baseDir: '/tmp/fals3-test' });
const s3 = new Fals3S3Client(fals3);

await s3.send(new PutObjectCommand({
  Bucket: 'uploads', Key: 'k', Body: 'hello',
}));

const out = await s3.send(new GetObjectCommand({ Bucket: 'uploads', Key: 'k' }));
console.log(await out.Body.transformToString()); // → "hello"
```

`Body` is returned as a `SdkStream`-like object with the standard `transformToString()` / `transformToByteArray()` / `transformToWebStream()` helpers — the same surface real S3 returns.

**Supported commands:** `CreateBucketCommand`, `HeadBucketCommand`, `DeleteBucketCommand`, `PutObjectCommand`, `GetObjectCommand`, `HeadObjectCommand`, `DeleteObjectCommand`, `ListObjectsV2Command`, `CopyObjectCommand`, `CreateMultipartUploadCommand`, `UploadPartCommand`, `CompleteMultipartUploadCommand`, `AbortMultipartUploadCommand`, `ListPartsCommand`. Anything else throws `err.name === 'UnsupportedCommand'`.

**Errors** are rewrapped to match AWS SDK v3 shape:

```ts
try {
  await s3.send(new GetObjectCommand({ Bucket: 'b', Key: 'missing' }));
} catch (err) {
  err.name;                          // "NoSuchKey"
  err.$metadata.httpStatusCode;      // 404
  err.$fault;                        // "client"
}
```

**No runtime dependency** on `@aws-sdk/client-s3`: the shim dispatches by `command.constructor.name`, so any object that exposes `{ constructor: { name }, input }` works (real SDK classes, hand-built test doubles, anything else).

Need a command we don't yet handle? Register a one-off handler:

```ts
import { Fals3S3Client } from 'fals3/sdk-shim';

Fals3S3Client.registerCommand('SelectObjectContentCommand', (s3, input) => {
  // Custom implementation against the underlying Fals3 instance.
  return { /* SDK-shaped output */ };
});
```

## What's supported (v1)

- Buckets: `CreateBucket`, `HeadBucket`, `DeleteBucket` (with `force`)
- Objects: `PutObject`, `GetObject` (with byte-range), `HeadObject`, `DeleteObject` (idempotent)
- Listing: `ListObjectsV2` with `prefix`, `delimiter`, `maxKeys`, `continuationToken`
- Copy: `CopyObject` same- and cross-bucket, COPY and REPLACE metadata directives
- Multipart: `CreateMultipartUpload`, `UploadPart`, `CompleteMultipartUpload`, `AbortMultipartUpload`, `ListParts`
- Conditional headers: `If-Match`, `If-None-Match`, `If-Modified-Since`, `If-Unmodified-Since` on read/write/copy-source
- ETag: MD5-hex strong ETag for single-part; `"<md5-of-md5s>-<N>"` for multipart — both AWS-compatible
- Concurrency: per-bucket reader/writer lock, parallel reads, serialised writes

## Status

| Area | State |
|------|-------|
| Tier A operations (above) | Shipped |
| `err.code` as a JS error property | Shipped |
| Conditional headers (`If-Match`, `If-None-Match`, `If-Modified-Since`, `If-Unmodified-Since`) | Shipped |
| Multipart upload (`Create`/`Upload`/`Complete`/`Abort`/`ListParts`) | Shipped |
| First-party AWS SDK v3 shim (`fals3/sdk-shim`) | Shipped |
| HTTP endpoint | Out of scope for v1 |

See [`docs/PROJECT_SPEC.md`](../docs/PROJECT_SPEC.md) and [`docs/DECISIONS.md`](../docs/DECISIONS.md) for design rationale.

## License

MIT
