# fals3 — Core Design Decisions

Resolves spec §13 open questions. These choices are binding for v1.

---

## 1. Metadata storage: sidecar files

**Decision:** sidecar file per object — `{path}.fals3-meta.json`.

**Rationale:**
- Simple concurrent access: each object's metadata lives next to its body, no single file becomes a write bottleneck during parallel test runs.
- Easier to debug: `ls` the bucket directory and each `.fals3-meta.json` is visible alongside its object.
- Atomic update pattern: write to a temp file, then `rename()` — no partial reads.
- A single index JSON would require a per-bucket lock for every metadata mutation; sidecar sidesteps that.

---

## 2. ETag algorithm: MD5 hex (matches AWS SDK expectations)

**Decision:** MD5 of the object body, formatted as a lowercase hex string, wrapped in double-quotes (e.g. `"d41d8cd98f00b204e9800998ecf8427e"`).

**Rationale:**
- AWS S3 returns `"<md5-hex>"` for single-part uploads. Tests that assert `expect(etag).toBe(awsEtag)` pass without any special-casing.
- MD5 base64 (as the spec mentioned as an alternative) is non-standard; real S3 uses hex.
- SHA-256 would improve collision resistance but would break parity with AWS SDK v3 matchers that compare ETag values directly.
- Configurability is deferred to v2; document that ETags are MD5-hex so consumers know.

---

## 3. NAPI async model: synchronous in v1, async deferred

**Decision (v1):** All NAPI methods are synchronous. The Rust core uses
`std::fs` and the binding returns plain values, not Promises.

**Decision (deferred):** Async (tokio + `#[napi]` async fn or `napi::Task`)
will be revisited in v2 if profiling on a real test suite shows the
synchronous path is a bottleneck.

**Rationale:**
- The primary consumer is **test code**. Tests expect deterministic, eager
  side effects: `s3.putObject(...)` should observably write a file by the
  time the next assertion runs. Sync semantics deliver that without forcing
  every test to `await`.
- Object sizes in tests are small (KB–low MB). The blocking cost of
  `std::fs::write` is well under a millisecond and dwarfed by NAPI call
  overhead either way.
- A sync surface eliminates the cost of routing every call through the
  tokio reactor and the Node thread-pool for trivially fast filesystem
  operations.
- Real AWS SDK v3 calls return Promises, but consumers of fals3 in tests
  typically wrap the client; the wrapper can present an async surface over
  the sync core if a particular test or shim needs one.
- Mixing async tokio fs with the per-bucket `parking_lot::RwLock` would
  require switching to an async-aware lock or holding sync locks across
  await points. The added complexity is not worth the v1 benefit.

**Reconsider when:** any of the following becomes true.
- A consumer hits measurable thread-pool starvation on the Node main loop.
- We add multipart uploads with large parts (≥10 MB) where blocking writes
  start to dominate.
- We add an HTTP endpoint (currently out of scope), which would require
  async I/O for connection handling.

---

## 4. Virtual host-style bucket names: API-surface only, no HTTP

**Decision:** v1 supports only path-style bucket addressing in the Rust/JS API (`Fals3::createBucket({ bucket: "name" })`). No virtual-host DNS or HTTP server in v1.

**Rationale:**
- fals3 is an in-process simulator, not an HTTP server. Virtual host-style is an HTTP/DNS concept irrelevant to the NAPI surface.
- Bucket names are validated against DNS-compatible naming rules (lowercase alphanumeric + hyphens, 3–63 chars, no leading/trailing hyphen) so they *would* be valid virtual-host names if an HTTP layer is added later.
- Stretch goal: HTTP endpoint in v2 would support both path-style and virtual-host.
