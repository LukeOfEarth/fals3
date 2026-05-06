# fals3 — Project specification

## 1. Summary

**fals3** is a Rust library that simulates Amazon S3 object behavior against a **local directory**, exposed to JavaScript/TypeScript as an **NPM native addon** (via [NAPI-RS](https://napi.rs/) / `napi` crate). The primary consumer is **automated test suites** that want S3-like semantics (buckets, keys, uploads, downloads, errors) without calling AWS or running LocalStack/MinIO.

**Positioning:** behavioral simulator for the *object model* and common SDK operations—not a full HTTP S3 endpoint emulator (unless added later as a stretch goal).

---

## 2. Goals

| Goal | Detail |
|------|--------|
| **Fidelity** | Match S3’s *observable* behavior for supported operations: naming rules, overwrite semantics, conditional requests where implemented, error shapes that map to AWS error codes/messages closely enough for assertions. |
| **Speed & isolation** | In-process, file-backed; no network required for default usage. |
| **NPM ergonomics** | Prebuilds for common platforms (`darwin-arm64`, `linux-x64`, `win32-x64`), TypeScript types shipped with the package, minimal setup in Jest/Vitest. |
| Single **root data directory** | All “buckets” and “keys” materialize under a configurable base path on disk. |

## 3. Non-goals (v1)

- Full S3 REST API compatibility or signature v4 validation.
- All S3 features (replication, intelligent-tiering, Object Lambda, etc.).
- Bit-for-bit compatibility with every edge case in AWS’s S3 implementation unless tests explicitly require it.
- Production use as a real object store.

---

## 4. Architecture

```
┌─────────────────────────────────────────────────────────┐
│  JS/TS tests (Vitest / Jest)                             │
│  import { Fals3, ... } from 'fals3'                      │
└────────────────────────┬────────────────────────────────┘
                         │ NAPI (async where I/O-bound)
┌────────────────────────▼────────────────────────────────┐
│  fals3-node (crate: thin NAPI layer)                     │
│  - Class factories, serde of options, error → JS         │
└────────────────────────┬────────────────────────────────┘
                         │
┌────────────────────────▼────────────────────────────────┐
│  fals3-core (crate: pure Rust “S3 over fs”)             │
│  - Bucket/key validation, path mapping, fs operations    │
│  - In-memory metadata index (ETags, timestamps, …)      │
│  - Optional: persistence of metadata JSON alongside objs  │
└─────────────────────────────────────────────────────────┘
```

**Crates:**

- **`fals3-core`** — No NAPI dependency. Unit-tested in Rust. Defines operations, error types, and filesystem layout.
- **`fals3-node`** — Depends on `fals3-core` + `napi` / `napi-derive`. Builds the `.node` binary.

**Repository layout (suggested):**

```
fals3/
  crates/
    fals3-core/
    fals3-node/
  npm/
    package.json          # name: fals3, types, optionalDependencies prebuilds
  docs/
```

---

## 5. Filesystem model

**Base directory** (e.g. `./tmp/s3-data`):

- **Bucket** → subdirectory: `{base}/{bucket}/` (bucket name rules aligned with S3: DNS-compliant subset for v1).
- **Object key** → relative path under bucket, with `/` preserved (normalize `.` and `..` reject or define explicitly; prefer **rejecting** unsafe segments for parity with S3 expectations).
- **Object body** → file at `{base}/{bucket}/{key}` (create parent dirs on `PutObject`).
- **Metadata** (v1 recommendation):
  - **Sidecar file**: `{path}.fals3-meta.json` *or* a single `.fals3-metadata.json` per bucket using a map of key → metadata (trade-off: many small files vs. one growing JSON). Prefer **sidecar** for simpler concurrent tests and easier debugging.
  - Store: `etag`, `last_modified`, `content_type`, `user_metadata` map, `content_encoding` if needed, `storage_class` stub.

**ETag:** Strong ETag for single-part uploads: hash of body (e.g. SHA-256 hex, or MD5 in base64 to mimic classic S3—document choice; MD5-in-base64 matches older SDK expectations for simple objects).

**Versioning:** v1 can treat buckets as **non-versioning** only; `Delete` removes object. Optional v2: `.fals3-versions/` directory tree.

---

## 6. Operation surface (prioritized)

### 6.1 Tier A — ship first

| Operation | Notes |
|-----------|--------|
| `CreateBucket` / `DeleteBucket` / `HeadBucket` | Empty bucket delete rules like S3 (must be empty or explicit force—match AWS). |
| `PutObject` | Stream or buffer from JS; set metadata headers. |
| `GetObject` | Return bytes + metadata; support `Range` if feasible. |
| `HeadObject` | Metadata only. |
| `DeleteObject` | Idempotent delete (204 vs. 404 policy—match AWS). |
| `ListObjectsV2` | Prefix, delimiter, `maxKeys`, `continuationToken` (encode offset in token). |
| `CopyObject` | Same-bucket and cross-bucket within same base directory. |

### 6.2 Tier B — compatibility wins

- `AbortMultipartUpload` / `CreateMultipartUpload` / `CompleteMultipartUpload` / `UploadPart` — implement enough for AWS SDK multipart flows used in tests.
- Conditional headers: `If-Match`, `If-None-Match`, `If-Modified-Since` on Get/Head/Put where applicable.

### 6.3 Tier C — later

- Presigned URLs (could stub as `file://` or reject).
- ACL / public access block (stub errors or no-ops with warnings in docs).

Each operation returns errors modeled after **AWS error codes** (e.g. `NoSuchBucket`, `NoSuchKey`, `PreconditionFailed`) so tests can `expect(err.name).toBe('NoSuchBucket')` or similar.

---

## 7. JavaScript / TypeScript API

**Principles:** small surface area, mirror familiar patterns (optional AWS SDK v3–like command pattern *or* a single `Fals3` class with methods—pick one and document).

**Example shape (illustrative):**

```ts
import { Fals3 } from 'fals3';

const s3 = await Fals3.open({ baseDir: '/tmp/fals3-test', strictS3Naming: true });

await s3.createBucket({ bucket: 'my-app-uploads' });
await s3.putObject({
  bucket: 'my-app-uploads',
  key: 'users/1/avatar.png',
  body: Buffer.from('...'),
  contentType: 'image/png',
  metadata: { uid: '1' },
});

const obj = await s3.getObject({ bucket: 'my-app-uploads', key: 'users/1/avatar.png' });
// obj.body, obj.etag, obj.lastModified, obj.metadata
```

**Test helpers:**

- `reset()` — wipe base dir or recreate client with fresh temp dir.
- `snapshot()` / `listAll()` — debug helpers (non-AWS, clearly named).

**Integration with AWS SDK v3 in tests:**

Two patterns to document:

1. **Direct fals3 API** — apps/tests call fals3 instead of S3 when `NODE_ENV=test`.
2. **Custom endpoint (stretch)** — run a minimal HTTP server in tests that implements S3’s REST subset so `S3Client({ endpoint: ..., forcePathStyle: true })` works; higher effort.

v1 should prioritize (1).

---

## 8. Concurrency and correctness

- **Per-bucket mutex** (Rust `tokio::sync::Mutex` or `std::sync::Mutex` in sync NAPI path) for list + put + delete consistency, or rely on FS + sidecar atomic writes (`write temp → rename`).
- Document that extremely parallel tests on the same key need the same expectations as against real S3 (last writer wins).

---

## 9. Packaging & build

- **NAPI-RS** project scaffolding: `napi build`, GitHub Actions matrix for prebuilds.
- **package.json**: `main`/`exports` for `.node`, `types` for `.d.ts`.
- **Engines:** Node LTS (e.g. ≥18).
- License, README with install + minimal example.

---

## 10. Testing strategy

| Layer | Tests |
|-------|--------|
| **Rust** | Unit tests for key validation, list pagination, etag, error mapping. |
| **JS** | Integration tests: run key scenarios (put/get/list/delete/multipart). |
| **Optional** | Golden tests comparing error messages/codes to LocalStack for a small subset. |

---

## 11. Security notes

- Paths must stay under `baseDir` (canonicalize and **prefix check** after resolving symlinks where possible).
- Not intended for hostile input; still reject `..` in keys and odd NUL bytes.

---

## 12. Milestones

1. **M0** — Repo, `fals3-core` path mapping + Put/Get/Delete/Head + `CreateBucket`.
2. **M1** — NAPI bindings + NPM package + ListObjectsV2 + TS types.
3. **M2** — Multipart + CopyObject + conditional headers.
4. **M3** — Polish, prebuilds, CI, docs for test integration patterns.

---

## 13. Open decisions (resolve early)

- Sidecar metadata vs. single index file.
- ETag algorithm (MD5 base64 vs. SHA-256 hex) vs. configurability.
- Async-only NAPI (`#[napi]` async) vs. `BlockingThread` pool for heavy I/O.
- Whether to support **virtual** host-style bucket names in API only (no HTTP).

---

## 14. Success criteria

- A TS test can replace S3 calls with fals3 and pass the same assertions for supported operations.
- `npm install fals3` works on macOS ARM and Linux x64 without a local Rust install (prebuilds).
- README “5 minute” quickstart runs Greenfield.
