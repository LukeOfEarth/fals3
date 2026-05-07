use std::{
    collections::{BTreeSet, HashMap},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::{SystemTime, UNIX_EPOCH},
};

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

use crate::{
    error::{Fals3Error, Result},
    meta::{compute_md5_bytes, compute_multipart_etag, ObjectMeta},
    paths::{
        assert_within_base, bucket_path, meta_path, object_path, validate_bucket_name, validate_key,
    },
};

/// Reserved directory under `base_dir` that holds in-flight multipart upload
/// state.  The leading `.` ensures it can never collide with a real bucket
/// name (`validate_bucket_name` rejects names that contain `.`).
const MULTIPART_DIR: &str = ".fals3-uploads";

/// Per-bucket reader-writer lock.
///
/// Write lock: `put_object`, `delete_object`, `copy_object` (destination bucket),
///             `create_bucket`, `delete_bucket`.
/// Read lock:  `get_object`, `head_object`, `list_objects_v2`.
///
/// Sidecar writes are already individually atomic (write-to-temp then rename),
/// so the lock guards the bucket as a unit — preventing a list from observing a
/// half-written key/sidecar pair during a concurrent put.
type BucketLock = Arc<RwLock<()>>;

/// The core S3 simulator.  All operations are synchronous filesystem calls.
///
/// `Store` is `Send + Sync` and can be shared across threads (e.g. inside an
/// `Arc<Store>`).  Concurrent operations on **different** buckets are fully
/// parallel.  Concurrent operations on the **same** bucket serialise writes and
/// allow parallel reads.
pub struct Store {
    base_dir: PathBuf,
    /// Guards access per bucket.  Keyed by bucket name.
    locks: RwLock<HashMap<String, BucketLock>>,
}

// ─── Bucket input ────────────────────────────────────────────────────────────

pub struct CreateBucketInput {
    pub bucket: String,
}

pub struct DeleteBucketInput {
    pub bucket: String,
    /// If true, delete the bucket even if it contains objects.
    pub force: bool,
}

pub struct HeadBucketInput {
    pub bucket: String,
}

// ─── Conditional headers ─────────────────────────────────────────────────────

/// HTTP-style precondition headers used by S3 for conditional reads, writes,
/// and copy-source checks.
///
/// Semantics match S3:
/// - On reads (`get`, `head`, copy source): `if_match` / `if_unmodified_since`
///   failures produce `PreconditionFailed`; `if_none_match` /
///   `if_modified_since` failures produce `NotModified`.
/// - On writes (`put`): `if_none_match: "*"` means "object must not exist"
///   (write-once semantics, used for atomic create); `if_match: <etag>` means
///   "current object must have this ETag" (optimistic concurrency).
///
/// ETag values may be supplied with or without surrounding double quotes;
/// comparison strips quotes on both sides.  The wildcard `"*"` matches any
/// existing object.
#[derive(Debug, Clone, Default)]
pub struct IfConditions {
    pub if_match: Option<String>,
    pub if_none_match: Option<String>,
    /// Unix timestamp (seconds).
    pub if_modified_since: Option<u64>,
    /// Unix timestamp (seconds).
    pub if_unmodified_since: Option<u64>,
}

impl IfConditions {
    fn is_empty(&self) -> bool {
        self.if_match.is_none()
            && self.if_none_match.is_none()
            && self.if_modified_since.is_none()
            && self.if_unmodified_since.is_none()
    }
}

fn strip_etag_quotes(s: &str) -> &str {
    s.trim().trim_matches('"')
}

fn etag_matches(provided: &str, actual_quoted: &str) -> bool {
    let p = strip_etag_quotes(provided);
    let a = strip_etag_quotes(actual_quoted);
    p == "*" || p == a
}

/// Evaluate preconditions against a known-existing object.
///
/// Order of evaluation matches AWS S3:
/// 1. `If-Match`            → `PreconditionFailed`
/// 2. `If-Unmodified-Since` → `PreconditionFailed`
/// 3. `If-None-Match`       → `NotModified`
/// 4. `If-Modified-Since`   → `NotModified`
fn check_read_preconditions(meta: &ObjectMeta, c: &IfConditions) -> Result<()> {
    if let Some(ref im) = c.if_match {
        if !etag_matches(im, &meta.etag) {
            return Err(Fals3Error::PreconditionFailed);
        }
    }
    if let Some(ts) = c.if_unmodified_since {
        if meta.last_modified > ts {
            return Err(Fals3Error::PreconditionFailed);
        }
    }
    if let Some(ref inm) = c.if_none_match {
        if etag_matches(inm, &meta.etag) {
            return Err(Fals3Error::NotModified);
        }
    }
    if let Some(ts) = c.if_modified_since {
        if meta.last_modified <= ts {
            return Err(Fals3Error::NotModified);
        }
    }
    Ok(())
}

/// Evaluate write preconditions for `PutObject`.
///
/// - `if_none_match: "*"` — object must not exist (`PreconditionFailed` if it does).
/// - `if_none_match: <etag>` — current object, if any, must not have this ETag.
/// - `if_match: <etag>`   — object must exist with this ETag (`PreconditionFailed` otherwise).
/// - `if_unmodified_since`— object, if it exists, must not be newer than the timestamp.
/// - `if_modified_since`  — ignored on writes (S3 does not honour it for `Put`).
fn check_write_preconditions(existing: Option<&ObjectMeta>, c: &IfConditions) -> Result<()> {
    if let (Some(ref inm), Some(meta)) = (&c.if_none_match, existing) {
        if etag_matches(inm, &meta.etag) {
            return Err(Fals3Error::PreconditionFailed);
        }
    }
    if let Some(ref im) = c.if_match {
        match existing {
            None => return Err(Fals3Error::PreconditionFailed),
            Some(meta) if !etag_matches(im, &meta.etag) => {
                return Err(Fals3Error::PreconditionFailed)
            }
            Some(_) => {}
        }
    }
    if let (Some(ts), Some(meta)) = (c.if_unmodified_since, existing) {
        if meta.last_modified > ts {
            return Err(Fals3Error::PreconditionFailed);
        }
    }
    Ok(())
}

// ─── Object input / output ───────────────────────────────────────────────────

pub struct PutObjectInput {
    pub bucket: String,
    pub key: String,
    pub body: Vec<u8>,
    pub content_type: Option<String>,
    pub metadata: HashMap<String, String>,
    pub content_encoding: Option<String>,
    pub conditions: IfConditions,
}

#[derive(Debug)]
pub struct PutObjectOutput {
    pub etag: String,
}

pub struct GetObjectInput {
    pub bucket: String,
    pub key: String,
    /// Byte range: `(start, end_inclusive)`.  `None` = full object.
    pub range: Option<(u64, u64)>,
    pub conditions: IfConditions,
}

#[derive(Debug)]
pub struct GetObjectOutput {
    pub body: Vec<u8>,
    pub meta: ObjectMeta,
}

pub struct HeadObjectInput {
    pub bucket: String,
    pub key: String,
    pub conditions: IfConditions,
}

pub struct DeleteObjectInput {
    pub bucket: String,
    pub key: String,
}

pub struct ListObjectsV2Input {
    pub bucket: String,
    /// Only return keys that begin with this string.
    pub prefix: Option<String>,
    /// Group keys that share a common prefix up to this delimiter into `common_prefixes`.
    pub delimiter: Option<String>,
    /// Maximum number of keys to return (default / max: 1000).
    pub max_keys: Option<u32>,
    /// Resume from a previous truncated response.
    pub continuation_token: Option<String>,
}

/// A single object entry in a `ListObjectsV2` response.
#[derive(Debug, Clone)]
pub struct ObjectEntry {
    pub key: String,
    pub etag: String,
    pub size: u64,
    pub last_modified: u64,
    pub storage_class: String,
}

#[derive(Debug)]
pub struct ListObjectsV2Output {
    pub contents: Vec<ObjectEntry>,
    /// Key prefixes collapsed by the delimiter (like S3 `CommonPrefixes`).
    pub common_prefixes: Vec<String>,
    pub is_truncated: bool,
    /// Present when `is_truncated` is true; pass as `continuation_token` to get the next page.
    pub next_continuation_token: Option<String>,
    pub key_count: u32,
}

pub struct CopyObjectInput {
    pub src_bucket: String,
    pub src_key: String,
    pub dst_bucket: String,
    pub dst_key: String,
    /// If `Some`, replace metadata on the copy; if `None`, preserve source metadata.
    pub metadata_directive: Option<MetadataDirective>,
    /// Preconditions evaluated against the **source** object (S3
    /// `x-amz-copy-source-if-*` headers).
    pub source_conditions: IfConditions,
}

pub enum MetadataDirective {
    Replace {
        content_type: Option<String>,
        metadata: HashMap<String, String>,
    },
}

#[derive(Debug)]
pub struct CopyObjectOutput {
    pub etag: String,
    pub last_modified: u64,
}

// ─── Multipart ───────────────────────────────────────────────────────────────

/// Maximum part number S3 allows (10 000).
pub const MAX_PART_NUMBER: u32 = 10_000;

#[derive(Debug, Serialize, Deserialize)]
struct UploadMeta {
    bucket: String,
    key: String,
    content_type: Option<String>,
    #[serde(default)]
    user_metadata: HashMap<String, String>,
    content_encoding: Option<String>,
    started_at: u64,
}

pub struct CreateMultipartUploadInput {
    pub bucket: String,
    pub key: String,
    pub content_type: Option<String>,
    pub metadata: HashMap<String, String>,
    pub content_encoding: Option<String>,
}

#[derive(Debug)]
pub struct CreateMultipartUploadOutput {
    pub bucket: String,
    pub key: String,
    pub upload_id: String,
}

pub struct UploadPartInput {
    pub upload_id: String,
    pub part_number: u32,
    pub body: Vec<u8>,
}

#[derive(Debug)]
pub struct UploadPartOutput {
    pub part_number: u32,
    pub etag: String,
}

#[derive(Debug, Clone)]
pub struct CompletedPart {
    pub part_number: u32,
    pub etag: String,
}

pub struct CompleteMultipartUploadInput {
    pub upload_id: String,
    /// Parts in ascending `part_number` order.  Each `etag` must match the
    /// ETag returned by the corresponding `upload_part` call.
    pub parts: Vec<CompletedPart>,
}

#[derive(Debug)]
pub struct CompleteMultipartUploadOutput {
    pub bucket: String,
    pub key: String,
    /// AWS-style multipart ETag: `"<md5-of-md5s>-<part-count>"`.
    pub etag: String,
}

pub struct AbortMultipartUploadInput {
    pub upload_id: String,
}

pub struct ListPartsInput {
    pub upload_id: String,
}

#[derive(Debug, Clone)]
pub struct PartEntry {
    pub part_number: u32,
    pub etag: String,
    pub size: u64,
    pub last_modified: u64,
}

#[derive(Debug)]
pub struct ListPartsOutput {
    pub bucket: String,
    pub key: String,
    pub parts: Vec<PartEntry>,
}

// ─── Store implementation ────────────────────────────────────────────────────

impl Store {
    /// Create a new `Store` rooted at `base_dir`.
    ///
    /// `base_dir` is created if it does not exist.  The path is canonicalized
    /// (symlinks resolved) so that prefix checks work correctly on macOS where
    /// `/tmp` is a symlink to `/private/tmp`.
    pub fn open(base_dir: impl Into<PathBuf>) -> Result<Self> {
        let base_dir: PathBuf = base_dir.into();
        std::fs::create_dir_all(&base_dir)?;
        let base_dir = base_dir.canonicalize()?;
        Ok(Self {
            base_dir,
            locks: RwLock::new(HashMap::new()),
        })
    }

    pub fn base_dir(&self) -> &Path {
        &self.base_dir
    }

    // ── Bucket operations ─────────────────────────────────────────────────

    /// Create a bucket (mkdir).
    ///
    /// Errors: `InvalidBucketName`, `BucketAlreadyExists`.
    pub fn create_bucket(&self, input: CreateBucketInput) -> Result<()> {
        validate_bucket_name(&input.bucket)?;
        let lock = self.bucket_lock(&input.bucket);
        let _guard = lock.write();
        let path = bucket_path(&self.base_dir, &input.bucket);
        if path.exists() {
            return Err(Fals3Error::BucketAlreadyExists {
                bucket: input.bucket,
            });
        }
        std::fs::create_dir_all(&path)?;
        Ok(())
    }

    /// Delete a bucket.
    ///
    /// - If `force` is false, returns `BucketNotEmpty` when the bucket contains objects.
    /// - Idempotent when the bucket doesn't exist: returns `NoSuchBucket`.
    pub fn delete_bucket(&self, input: DeleteBucketInput) -> Result<()> {
        validate_bucket_name(&input.bucket)?;
        let lock = self.bucket_lock(&input.bucket);
        let _guard = lock.write();
        let path = bucket_path(&self.base_dir, &input.bucket);
        if !path.exists() {
            return Err(Fals3Error::NoSuchBucket {
                bucket: input.bucket,
            });
        }
        if !input.force && self.bucket_has_objects(&path)? {
            return Err(Fals3Error::BucketNotEmpty {
                bucket: input.bucket,
            });
        }
        std::fs::remove_dir_all(&path)?;
        Ok(())
    }

    /// Check that a bucket exists.
    ///
    /// Errors: `InvalidBucketName`, `NoSuchBucket`.
    pub fn head_bucket(&self, input: HeadBucketInput) -> Result<()> {
        validate_bucket_name(&input.bucket)?;
        let path = bucket_path(&self.base_dir, &input.bucket);
        if !path.is_dir() {
            return Err(Fals3Error::NoSuchBucket {
                bucket: input.bucket,
            });
        }
        Ok(())
    }

    // ── Object operations ─────────────────────────────────────────────────

    /// Write an object body + sidecar metadata.
    ///
    /// Creates parent directories as needed (mirrors S3 key namespacing).
    /// Errors: `NoSuchBucket`, `InvalidObjectKey`.
    pub fn put_object(&self, input: PutObjectInput) -> Result<PutObjectOutput> {
        validate_bucket_name(&input.bucket)?;
        validate_key(&input.key)?;
        self.require_bucket(&input.bucket)?;
        let lock = self.bucket_lock(&input.bucket);
        let _guard = lock.write();

        let body_path = object_path(&self.base_dir, &input.bucket, &input.key);
        assert_within_base(&self.base_dir, &body_path)?;

        // Evaluate preconditions before writing.  Existing meta is None when
        // the object doesn't exist yet.
        if !input.conditions.is_empty() {
            let sidecar = meta_path(&self.base_dir, &input.bucket, &input.key);
            let existing = if body_path.exists() {
                Some(ObjectMeta::read(&sidecar)?)
            } else {
                None
            };
            check_write_preconditions(existing.as_ref(), &input.conditions)?;
        }

        // Create parent dirs for nested keys (e.g. `users/1/avatar.png`).
        if let Some(parent) = body_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let meta = ObjectMeta::new(
            &input.body,
            input.content_type,
            input.metadata,
            input.content_encoding,
        );

        // Write body then sidecar — body first so a partial write leaves no
        // orphaned sidecar pointing at a missing file.
        std::fs::write(&body_path, &input.body)?;

        let sidecar = meta_path(&self.base_dir, &input.bucket, &input.key);
        meta.write(&sidecar)?;

        Ok(PutObjectOutput { etag: meta.etag })
    }

    /// Read an object body and metadata.
    ///
    /// Supports optional byte-range (inclusive).
    /// Errors: `NoSuchBucket`, `NoSuchKey`, `InvalidObjectKey`.
    pub fn get_object(&self, input: GetObjectInput) -> Result<GetObjectOutput> {
        validate_bucket_name(&input.bucket)?;
        validate_key(&input.key)?;
        self.require_bucket(&input.bucket)?;
        let lock = self.bucket_lock(&input.bucket);
        let _guard = lock.read();

        let body_path = object_path(&self.base_dir, &input.bucket, &input.key);
        assert_within_base(&self.base_dir, &body_path)?;

        if !body_path.exists() {
            return Err(Fals3Error::NoSuchKey {
                bucket: input.bucket,
                key: input.key,
            });
        }

        let sidecar = meta_path(&self.base_dir, &input.bucket, &input.key);
        let meta = ObjectMeta::read(&sidecar)?;
        check_read_preconditions(&meta, &input.conditions)?;

        let full_body = std::fs::read(&body_path)?;
        let body = match input.range {
            Some((start, end)) => {
                let end = end.min(full_body.len().saturating_sub(1) as u64);
                full_body[start as usize..=end as usize].to_vec()
            }
            None => full_body,
        };

        Ok(GetObjectOutput { body, meta })
    }

    /// Return metadata only (no body).
    ///
    /// Errors: `NoSuchBucket`, `NoSuchKey`, `InvalidObjectKey`.
    pub fn head_object(&self, input: HeadObjectInput) -> Result<ObjectMeta> {
        validate_bucket_name(&input.bucket)?;
        validate_key(&input.key)?;
        self.require_bucket(&input.bucket)?;
        let lock = self.bucket_lock(&input.bucket);
        let _guard = lock.read();

        let body_path = object_path(&self.base_dir, &input.bucket, &input.key);
        assert_within_base(&self.base_dir, &body_path)?;

        if !body_path.exists() {
            return Err(Fals3Error::NoSuchKey {
                bucket: input.bucket,
                key: input.key,
            });
        }

        let sidecar = meta_path(&self.base_dir, &input.bucket, &input.key);
        let meta = ObjectMeta::read(&sidecar)?;
        check_read_preconditions(&meta, &input.conditions)?;
        Ok(meta)
    }

    /// Delete an object (idempotent — deleting a non-existent key is a no-op,
    /// matching AWS `DeleteObject` returning 204 even if the key didn't exist).
    ///
    /// Errors: `NoSuchBucket`, `InvalidObjectKey`.
    pub fn delete_object(&self, input: DeleteObjectInput) -> Result<()> {
        validate_bucket_name(&input.bucket)?;
        validate_key(&input.key)?;
        self.require_bucket(&input.bucket)?;
        let lock = self.bucket_lock(&input.bucket);
        let _guard = lock.write();

        let body_path = object_path(&self.base_dir, &input.bucket, &input.key);
        assert_within_base(&self.base_dir, &body_path)?;

        // Idempotent: ignore NotFound.
        if body_path.exists() {
            std::fs::remove_file(&body_path)?;
        }

        let sidecar = meta_path(&self.base_dir, &input.bucket, &input.key);
        if sidecar.exists() {
            std::fs::remove_file(&sidecar)?;
        }

        Ok(())
    }

    // ── ListObjectsV2 ─────────────────────────────────────────────────────

    /// List objects in a bucket, S3 ListObjectsV2 semantics.
    ///
    /// - `prefix`: only keys starting with this string are returned.
    /// - `delimiter`: keys that contain the delimiter after the prefix are
    ///   collapsed into `common_prefixes` (like S3 "virtual directories").
    /// - `max_keys`: page size (1–1000, default 1000).
    /// - `continuation_token`: opaque token from a previous truncated response.
    ///
    /// Errors: `NoSuchBucket`, `InvalidBucketName`.
    pub fn list_objects_v2(&self, input: ListObjectsV2Input) -> Result<ListObjectsV2Output> {
        validate_bucket_name(&input.bucket)?;
        self.require_bucket(&input.bucket)?;
        let lock = self.bucket_lock(&input.bucket);
        let _guard = lock.read();

        let prefix = input.prefix.as_deref().unwrap_or("");
        let max_keys = input.max_keys.unwrap_or(1000).min(1000) as usize;

        // Collect all object keys in the bucket (excluding sidecar files), sorted.
        let mut all_keys: Vec<String> = self.collect_keys(&input.bucket)?;
        all_keys.sort();

        // Apply prefix filter.
        let mut keys: Vec<String> = all_keys
            .into_iter()
            .filter(|k| k.starts_with(prefix))
            .collect();

        // Apply continuation token (token is the exclusive start key).
        if let Some(ref token) = input.continuation_token {
            let start_key = Self::decode_token(token);
            keys.retain(|k| k.as_str() > start_key.as_str());
        }

        // Apply delimiter — split keys into contents vs. common_prefixes.
        let mut contents: Vec<ObjectEntry> = Vec::new();
        let mut common_prefix_set: BTreeSet<String> = BTreeSet::new();

        for key in &keys {
            if contents.len() + common_prefix_set.len() >= max_keys {
                break;
            }
            if let Some(ref delim) = input.delimiter {
                // Look for the delimiter *after* the prefix.
                let suffix = &key[prefix.len()..];
                if let Some(pos) = suffix.find(delim.as_str()) {
                    let cp = format!("{}{}{}", prefix, &suffix[..pos], delim);
                    common_prefix_set.insert(cp);
                    continue;
                }
            }
            // Regular content entry — load sidecar for metadata.
            let sidecar = meta_path(&self.base_dir, &input.bucket, key);
            let meta = ObjectMeta::read(&sidecar)?;
            contents.push(ObjectEntry {
                key: key.clone(),
                etag: meta.etag,
                size: meta.size,
                last_modified: meta.last_modified,
                storage_class: meta.storage_class,
            });
        }

        let total_returned = contents.len() + common_prefix_set.len();
        let is_truncated = total_returned >= max_keys && {
            // Check whether there are more keys beyond what we returned.
            let last_key = contents
                .last()
                .map(|e| e.key.as_str())
                .or_else(|| common_prefix_set.iter().last().map(String::as_str));
            if let Some(lk) = last_key {
                keys.iter().any(|k| k.as_str() > lk)
            } else {
                false
            }
        };

        let next_continuation_token = if is_truncated {
            let last_key = contents
                .last()
                .map(|e| e.key.clone())
                .or_else(|| common_prefix_set.iter().last().cloned());
            last_key.map(|k| Self::encode_token(&k))
        } else {
            None
        };

        let key_count = (contents.len() + common_prefix_set.len()) as u32;
        let common_prefixes: Vec<String> = common_prefix_set.into_iter().collect();

        Ok(ListObjectsV2Output {
            contents,
            common_prefixes,
            is_truncated,
            next_continuation_token,
            key_count,
        })
    }

    // ── CopyObject ────────────────────────────────────────────────────────

    /// Copy an object within or across buckets (both must be in this Store's base_dir).
    ///
    /// Errors: `NoSuchBucket`, `NoSuchKey`, `InvalidBucketName`, `InvalidObjectKey`.
    pub fn copy_object(&self, input: CopyObjectInput) -> Result<CopyObjectOutput> {
        validate_bucket_name(&input.src_bucket)?;
        validate_bucket_name(&input.dst_bucket)?;
        validate_key(&input.src_key)?;
        validate_key(&input.dst_key)?;
        self.require_bucket(&input.src_bucket)?;
        self.require_bucket(&input.dst_bucket)?;

        // Read source under a write lock (conservative but correct), then drop
        // the lock before writing the destination.  This is equivalent to S3's
        // "read the source at the time of the CopyObject call" semantics.
        let (body, src_meta) = {
            let src_lock = self.bucket_lock(&input.src_bucket);
            let _guard = src_lock.write();

            let src_body_path = object_path(&self.base_dir, &input.src_bucket, &input.src_key);
            assert_within_base(&self.base_dir, &src_body_path)?;
            if !src_body_path.exists() {
                return Err(Fals3Error::NoSuchKey {
                    bucket: input.src_bucket.clone(),
                    key: input.src_key.clone(),
                });
            }
            let src_sidecar = meta_path(&self.base_dir, &input.src_bucket, &input.src_key);
            let meta = ObjectMeta::read(&src_sidecar)?;
            check_read_preconditions(&meta, &input.source_conditions)?;
            let body = std::fs::read(&src_body_path)?;
            (body, meta)
            // _guard dropped here, releasing the src lock.
        };

        // Write destination under its write lock (no-op for same-bucket since
        // we already released the single write lock above — acceptable: last
        // writer wins, consistent with S3 semantics).
        let dst_meta = {
            let dst_lock = self.bucket_lock(&input.dst_bucket);
            let _guard = dst_lock.write();

            let dst_body_path = object_path(&self.base_dir, &input.dst_bucket, &input.dst_key);
            assert_within_base(&self.base_dir, &dst_body_path)?;
            if let Some(parent) = dst_body_path.parent() {
                std::fs::create_dir_all(parent)?;
            }

            let meta = match input.metadata_directive {
                Some(MetadataDirective::Replace {
                    content_type,
                    metadata,
                }) => ObjectMeta::new(&body, content_type, metadata, None),
                None => ObjectMeta::new(
                    &body,
                    src_meta.content_type,
                    src_meta.user_metadata,
                    src_meta.content_encoding,
                ),
            };

            std::fs::write(&dst_body_path, &body)?;
            let dst_sidecar = meta_path(&self.base_dir, &input.dst_bucket, &input.dst_key);
            meta.write(&dst_sidecar)?;
            meta
        };

        Ok(CopyObjectOutput {
            etag: dst_meta.etag,
            last_modified: dst_meta.last_modified,
        })
    }

    // ── Multipart upload ──────────────────────────────────────────────────

    /// Begin a multipart upload.  Returns a fresh `upload_id` that subsequent
    /// `upload_part` and `complete_multipart_upload` calls reference.
    ///
    /// State is recorded under `{base_dir}/.fals3-uploads/{upload_id}/` until
    /// the upload is completed or aborted; nothing is written into the
    /// destination bucket yet.
    pub fn create_multipart_upload(
        &self,
        input: CreateMultipartUploadInput,
    ) -> Result<CreateMultipartUploadOutput> {
        validate_bucket_name(&input.bucket)?;
        validate_key(&input.key)?;
        self.require_bucket(&input.bucket)?;

        let upload_id = new_upload_id();
        let dir = upload_dir(&self.base_dir, &upload_id);
        std::fs::create_dir_all(&dir)?;

        let started_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let upload_meta = UploadMeta {
            bucket: input.bucket.clone(),
            key: input.key.clone(),
            content_type: input.content_type,
            user_metadata: input.metadata,
            content_encoding: input.content_encoding,
            started_at,
        };
        write_upload_meta(&dir, &upload_meta)?;

        Ok(CreateMultipartUploadOutput {
            bucket: input.bucket,
            key: input.key,
            upload_id,
        })
    }

    /// Upload a single part of an in-flight multipart upload.
    ///
    /// `part_number` must be in `1..=MAX_PART_NUMBER`.  Re-uploading the same
    /// part number replaces the previous body (last writer wins, matches S3).
    ///
    /// Returns the part's MD5 ETag — callers must pass it back, paired with
    /// `part_number`, in `complete_multipart_upload`.
    pub fn upload_part(&self, input: UploadPartInput) -> Result<UploadPartOutput> {
        validate_part_number(input.part_number)?;
        let dir = upload_dir(&self.base_dir, &input.upload_id);
        if !dir.is_dir() {
            return Err(Fals3Error::NoSuchUpload {
                upload_id: input.upload_id,
            });
        }

        let body_path = part_body_path(&dir, input.part_number);
        let etag_path = part_etag_path(&dir, input.part_number);

        // Body first, then etag — order matters for crash recovery: a stray
        // body without an etag is interpreted as "not yet written" by Complete.
        std::fs::write(&body_path, &input.body)?;
        let etag = crate::meta::compute_etag(&input.body);
        std::fs::write(&etag_path, &etag)?;

        Ok(UploadPartOutput {
            part_number: input.part_number,
            etag,
        })
    }

    /// Finalise a multipart upload by concatenating its parts in order and
    /// writing the resulting object to the bucket.
    ///
    /// Validations (matching S3 error codes):
    /// - Upload exists                                     → otherwise `NoSuchUpload`
    /// - `parts` non-empty and strictly ascending          → otherwise `InvalidPartOrder`
    /// - Each `(part_number, etag)` matches a stored part  → otherwise `InvalidPart`
    pub fn complete_multipart_upload(
        &self,
        input: CompleteMultipartUploadInput,
    ) -> Result<CompleteMultipartUploadOutput> {
        let dir = upload_dir(&self.base_dir, &input.upload_id);
        if !dir.is_dir() {
            return Err(Fals3Error::NoSuchUpload {
                upload_id: input.upload_id,
            });
        }

        if input.parts.is_empty() {
            return Err(Fals3Error::InvalidPart);
        }
        for window in input.parts.windows(2) {
            if window[0].part_number >= window[1].part_number {
                return Err(Fals3Error::InvalidPartOrder);
            }
        }

        let upload_meta = read_upload_meta(&dir)?;

        // Verify each requested part exists with the expected etag, and
        // accumulate the body bytes + raw md5 digests for the multipart etag.
        let mut full_body: Vec<u8> = Vec::new();
        let mut part_md5s: Vec<[u8; 16]> = Vec::with_capacity(input.parts.len());
        for part in &input.parts {
            validate_part_number(part.part_number)?;
            let body_path = part_body_path(&dir, part.part_number);
            let etag_path = part_etag_path(&dir, part.part_number);
            if !body_path.is_file() || !etag_path.is_file() {
                return Err(Fals3Error::InvalidPart);
            }
            let stored_etag = std::fs::read_to_string(&etag_path)?;
            if !etags_equal(&part.etag, &stored_etag) {
                return Err(Fals3Error::InvalidPart);
            }
            let body = std::fs::read(&body_path)?;
            part_md5s.push(compute_md5_bytes(&body));
            full_body.extend_from_slice(&body);
        }

        // Now write the assembled object, holding the destination bucket lock.
        self.require_bucket(&upload_meta.bucket)?;
        let lock = self.bucket_lock(&upload_meta.bucket);
        let _guard = lock.write();

        let dst_body_path = object_path(&self.base_dir, &upload_meta.bucket, &upload_meta.key);
        assert_within_base(&self.base_dir, &dst_body_path)?;
        if let Some(parent) = dst_body_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&dst_body_path, &full_body)?;

        let etag = compute_multipart_etag(&part_md5s);
        let meta = ObjectMeta::with_explicit_etag(
            full_body.len() as u64,
            etag.clone(),
            upload_meta.content_type.clone(),
            upload_meta.user_metadata.clone(),
            upload_meta.content_encoding.clone(),
        );
        let sidecar = meta_path(&self.base_dir, &upload_meta.bucket, &upload_meta.key);
        meta.write(&sidecar)?;

        // Best-effort cleanup of the upload state directory.  If this fails,
        // the object is still complete; the orphan can be removed manually.
        let _ = std::fs::remove_dir_all(&dir);

        Ok(CompleteMultipartUploadOutput {
            bucket: upload_meta.bucket,
            key: upload_meta.key,
            etag,
        })
    }

    /// Cancel an in-flight multipart upload and discard all uploaded parts.
    ///
    /// Idempotent: aborting an unknown `upload_id` succeeds silently
    /// (matches S3's behaviour where `AbortMultipartUpload` is safe to retry).
    pub fn abort_multipart_upload(&self, input: AbortMultipartUploadInput) -> Result<()> {
        let dir = upload_dir(&self.base_dir, &input.upload_id);
        if dir.is_dir() {
            std::fs::remove_dir_all(&dir)?;
        }
        Ok(())
    }

    /// List the parts already uploaded for an in-flight multipart upload.
    pub fn list_parts(&self, input: ListPartsInput) -> Result<ListPartsOutput> {
        let dir = upload_dir(&self.base_dir, &input.upload_id);
        if !dir.is_dir() {
            return Err(Fals3Error::NoSuchUpload {
                upload_id: input.upload_id,
            });
        }
        let upload_meta = read_upload_meta(&dir)?;

        let mut parts: Vec<PartEntry> = Vec::new();
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if let Some(n) = parse_part_body_filename(&name_str) {
                let etag_path = part_etag_path(&dir, n);
                let etag = if etag_path.is_file() {
                    std::fs::read_to_string(&etag_path)?
                } else {
                    // Body present but no etag yet — treat as in-flight; skip.
                    continue;
                };
                let md = entry.metadata()?;
                let last_modified = md
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                    .map(|d| d.as_secs())
                    .unwrap_or(upload_meta.started_at);
                parts.push(PartEntry {
                    part_number: n,
                    etag,
                    size: md.len(),
                    last_modified,
                });
            }
        }
        parts.sort_by_key(|p| p.part_number);

        Ok(ListPartsOutput {
            bucket: upload_meta.bucket,
            key: upload_meta.key,
            parts,
        })
    }

    // ── Helpers ───────────────────────────────────────────────────────────

    /// Recursively collect all object keys (non-sidecar files) under a bucket, as relative paths.
    fn collect_keys(&self, bucket: &str) -> Result<Vec<String>> {
        let bucket_dir = bucket_path(&self.base_dir, bucket);
        let mut keys = Vec::new();
        self.collect_keys_recursive(&bucket_dir, &bucket_dir, &mut keys)?;
        Ok(keys)
    }

    fn collect_keys_recursive(
        &self,
        bucket_dir: &Path,
        dir: &Path,
        keys: &mut Vec<String>,
    ) -> Result<()> {
        // `&self` isn't strictly needed (this is a tree walk over `dir`), but
        // keeping the method shape lets us reuse `self.base_dir` etc. in the
        // future without touching call sites.  Suppress the "parameter only
        // used in recursion" lint.
        #[allow(clippy::only_used_in_recursion)]
        let _ = self;
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let ft = entry.file_type()?;
            let name = entry.file_name();
            let name_str = name.to_string_lossy();

            if ft.is_dir() {
                self.collect_keys_recursive(bucket_dir, &entry.path(), keys)?;
            } else if ft.is_file() && !name_str.ends_with(".fals3-meta.json") {
                // Build key as relative path from bucket root, using forward slashes.
                let rel = entry
                    .path()
                    .strip_prefix(bucket_dir)
                    .unwrap_or(&entry.path())
                    .to_string_lossy()
                    .replace('\\', "/");
                keys.push(rel);
            }
        }
        Ok(())
    }

    /// Encode a continuation token (just the last-seen key, base64-encoded).
    fn encode_token(last_key: &str) -> String {
        use std::fmt::Write as _;
        // Simple percent-style hex encoding to keep it opaque and ASCII-safe.
        let mut out = String::new();
        for b in last_key.bytes() {
            let _ = write!(out, "{b:02x}");
        }
        out
    }

    /// Decode a continuation token back to the exclusive-start key.
    fn decode_token(token: &str) -> String {
        (0..token.len())
            .step_by(2)
            .filter_map(|i| u8::from_str_radix(&token[i..i + 2], 16).ok())
            .map(|b| b as char)
            .collect()
    }

    fn require_bucket(&self, bucket: &str) -> Result<()> {
        let path = bucket_path(&self.base_dir, bucket);
        if !path.is_dir() {
            return Err(Fals3Error::NoSuchBucket {
                bucket: bucket.to_string(),
            });
        }
        Ok(())
    }

    /// Return (creating if absent) the `RwLock` for `bucket`.
    fn bucket_lock(&self, bucket: &str) -> BucketLock {
        // Fast path: lock already exists.
        {
            let map = self.locks.read();
            if let Some(lock) = map.get(bucket) {
                return Arc::clone(lock);
            }
        }
        // Slow path: insert under write lock.
        let mut map = self.locks.write();
        Arc::clone(
            map.entry(bucket.to_string())
                .or_insert_with(|| Arc::new(RwLock::new(()))),
        )
    }

    /// Returns `true` if the bucket directory contains at least one non-sidecar file.
    fn bucket_has_objects(&self, bucket_dir: &Path) -> Result<bool> {
        for entry in std::fs::read_dir(bucket_dir)? {
            let entry = entry?;
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            // Skip sidecar files — only count body files as "objects".
            if !name_str.ends_with(".fals3-meta.json") {
                let ft = entry.file_type()?;
                if ft.is_file() || ft.is_dir() {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }
}

// ─── Multipart helpers ───────────────────────────────────────────────────────

fn upload_dir(base: &Path, upload_id: &str) -> PathBuf {
    base.join(MULTIPART_DIR).join(upload_id)
}

fn upload_meta_path(dir: &Path) -> PathBuf {
    dir.join("meta.json")
}

fn part_body_path(dir: &Path, part_number: u32) -> PathBuf {
    dir.join(format!("part-{part_number:05}.bin"))
}

fn part_etag_path(dir: &Path, part_number: u32) -> PathBuf {
    dir.join(format!("part-{part_number:05}.etag"))
}

fn parse_part_body_filename(name: &str) -> Option<u32> {
    let stem = name.strip_suffix(".bin")?;
    let num = stem.strip_prefix("part-")?;
    num.parse().ok()
}

fn read_upload_meta(dir: &Path) -> Result<UploadMeta> {
    let path = upload_meta_path(dir);
    let bytes = std::fs::read(&path)?;
    Ok(serde_json::from_slice(&bytes)?)
}

fn write_upload_meta(dir: &Path, meta: &UploadMeta) -> Result<()> {
    let path = upload_meta_path(dir);
    let json = serde_json::to_vec_pretty(meta)?;
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, &json)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

fn validate_part_number(n: u32) -> Result<()> {
    if (1..=MAX_PART_NUMBER).contains(&n) {
        Ok(())
    } else {
        Err(Fals3Error::InvalidPart)
    }
}

fn etags_equal(a: &str, b: &str) -> bool {
    a.trim().trim_matches('"') == b.trim().trim_matches('"')
}

fn new_upload_id() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let t = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64;
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{t:016x}{n:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        (dir, store)
    }

    // ── CreateBucket ──────────────────────────────────────────────────────

    #[test]
    fn create_and_head_bucket() {
        let (_dir, store) = tmp_store();
        store
            .create_bucket(CreateBucketInput {
                bucket: "my-bucket".into(),
            })
            .unwrap();
        store
            .head_bucket(HeadBucketInput {
                bucket: "my-bucket".into(),
            })
            .unwrap();
    }

    #[test]
    fn create_bucket_already_exists() {
        let (_dir, store) = tmp_store();
        store
            .create_bucket(CreateBucketInput {
                bucket: "dup".into(),
            })
            .unwrap();
        let err = store
            .create_bucket(CreateBucketInput {
                bucket: "dup".into(),
            })
            .unwrap_err();
        assert_eq!(err.code(), "BucketAlreadyExists");
    }

    #[test]
    fn create_bucket_invalid_name() {
        let (_dir, store) = tmp_store();
        let err = store
            .create_bucket(CreateBucketInput {
                bucket: "BadName".into(),
            })
            .unwrap_err();
        assert_eq!(err.code(), "InvalidBucketName");
    }

    #[test]
    fn head_bucket_missing() {
        let (_dir, store) = tmp_store();
        let err = store
            .head_bucket(HeadBucketInput {
                bucket: "missing".into(),
            })
            .unwrap_err();
        assert_eq!(err.code(), "NoSuchBucket");
    }

    // ── DeleteBucket ──────────────────────────────────────────────────────

    #[test]
    fn delete_empty_bucket() {
        let (_dir, store) = tmp_store();
        store
            .create_bucket(CreateBucketInput {
                bucket: "todel".into(),
            })
            .unwrap();
        store
            .delete_bucket(DeleteBucketInput {
                bucket: "todel".into(),
                force: false,
            })
            .unwrap();
        let err = store
            .head_bucket(HeadBucketInput {
                bucket: "todel".into(),
            })
            .unwrap_err();
        assert_eq!(err.code(), "NoSuchBucket");
    }

    #[test]
    fn delete_nonempty_bucket_without_force_errors() {
        let (_dir, store) = tmp_store();
        store
            .create_bucket(CreateBucketInput {
                bucket: "full".into(),
            })
            .unwrap();
        store
            .put_object(PutObjectInput {
                bucket: "full".into(),
                key: "obj.txt".into(),
                body: b"data".to_vec(),
                content_type: None,
                metadata: HashMap::new(),
                content_encoding: None,
                conditions: IfConditions::default(),
            })
            .unwrap();
        let err = store
            .delete_bucket(DeleteBucketInput {
                bucket: "full".into(),
                force: false,
            })
            .unwrap_err();
        assert_eq!(err.code(), "BucketNotEmpty");
    }

    #[test]
    fn delete_nonempty_bucket_with_force() {
        let (_dir, store) = tmp_store();
        store
            .create_bucket(CreateBucketInput {
                bucket: "full".into(),
            })
            .unwrap();
        store
            .put_object(PutObjectInput {
                bucket: "full".into(),
                key: "obj.txt".into(),
                body: b"data".to_vec(),
                content_type: None,
                metadata: HashMap::new(),
                content_encoding: None,
                conditions: IfConditions::default(),
            })
            .unwrap();
        store
            .delete_bucket(DeleteBucketInput {
                bucket: "full".into(),
                force: true,
            })
            .unwrap();
    }

    #[test]
    fn delete_missing_bucket() {
        let (_dir, store) = tmp_store();
        let err = store
            .delete_bucket(DeleteBucketInput {
                bucket: "ghost".into(),
                force: false,
            })
            .unwrap_err();
        assert_eq!(err.code(), "NoSuchBucket");
    }

    // ── PutObject / GetObject / HeadObject ────────────────────────────────

    #[test]
    fn put_get_roundtrip() {
        let (_dir, store) = tmp_store();
        store
            .create_bucket(CreateBucketInput {
                bucket: "bkt".into(),
            })
            .unwrap();

        let out = store
            .put_object(PutObjectInput {
                bucket: "bkt".into(),
                key: "hello.txt".into(),
                body: b"hello world".to_vec(),
                content_type: Some("text/plain".into()),
                metadata: HashMap::from([("author".into(), "test".into())]),
                content_encoding: None,
                conditions: IfConditions::default(),
            })
            .unwrap();

        assert!(out.etag.starts_with('"'));

        let got = store
            .get_object(GetObjectInput {
                bucket: "bkt".into(),
                key: "hello.txt".into(),
                range: None,
                conditions: IfConditions::default(),
            })
            .unwrap();

        assert_eq!(got.body, b"hello world");
        assert_eq!(got.meta.etag, out.etag);
        assert_eq!(got.meta.content_type, Some("text/plain".into()));
        assert_eq!(
            got.meta.user_metadata.get("author").map(String::as_str),
            Some("test")
        );
    }

    #[test]
    fn put_nested_key() {
        let (_dir, store) = tmp_store();
        store
            .create_bucket(CreateBucketInput {
                bucket: "bkt".into(),
            })
            .unwrap();
        store
            .put_object(PutObjectInput {
                bucket: "bkt".into(),
                key: "users/1/avatar.png".into(),
                body: b"\x89PNG".to_vec(),
                content_type: Some("image/png".into()),
                metadata: HashMap::new(),
                content_encoding: None,
                conditions: IfConditions::default(),
            })
            .unwrap();

        let got = store
            .get_object(GetObjectInput {
                bucket: "bkt".into(),
                key: "users/1/avatar.png".into(),
                range: None,
                conditions: IfConditions::default(),
            })
            .unwrap();
        assert_eq!(&got.body[..4], b"\x89PNG");
    }

    #[test]
    fn get_range() {
        let (_dir, store) = tmp_store();
        store
            .create_bucket(CreateBucketInput {
                bucket: "bkt".into(),
            })
            .unwrap();
        store
            .put_object(PutObjectInput {
                bucket: "bkt".into(),
                key: "data.bin".into(),
                body: b"0123456789".to_vec(),
                content_type: None,
                metadata: HashMap::new(),
                content_encoding: None,
                conditions: IfConditions::default(),
            })
            .unwrap();

        let got = store
            .get_object(GetObjectInput {
                bucket: "bkt".into(),
                key: "data.bin".into(),
                range: Some((2, 5)),
                conditions: IfConditions::default(),
            })
            .unwrap();
        assert_eq!(got.body, b"2345");
    }

    #[test]
    fn get_missing_key() {
        let (_dir, store) = tmp_store();
        store
            .create_bucket(CreateBucketInput {
                bucket: "bkt".into(),
            })
            .unwrap();
        let err = store
            .get_object(GetObjectInput {
                bucket: "bkt".into(),
                key: "ghost.txt".into(),
                range: None,
                conditions: IfConditions::default(),
            })
            .unwrap_err();
        assert_eq!(err.code(), "NoSuchKey");
    }

    #[test]
    fn head_object() {
        let (_dir, store) = tmp_store();
        store
            .create_bucket(CreateBucketInput {
                bucket: "bkt".into(),
            })
            .unwrap();
        store
            .put_object(PutObjectInput {
                bucket: "bkt".into(),
                key: "f.txt".into(),
                body: b"hi".to_vec(),
                content_type: None,
                metadata: HashMap::new(),
                content_encoding: None,
                conditions: IfConditions::default(),
            })
            .unwrap();

        let meta = store
            .head_object(HeadObjectInput {
                bucket: "bkt".into(),
                key: "f.txt".into(),
                conditions: IfConditions::default(),
            })
            .unwrap();
        assert_eq!(meta.size, 2);
    }

    // ── DeleteObject ──────────────────────────────────────────────────────

    #[test]
    fn delete_object_idempotent() {
        let (_dir, store) = tmp_store();
        store
            .create_bucket(CreateBucketInput {
                bucket: "bkt".into(),
            })
            .unwrap();
        // Delete a key that was never created — should succeed (idempotent).
        store
            .delete_object(DeleteObjectInput {
                bucket: "bkt".into(),
                key: "ghost.txt".into(),
            })
            .unwrap();
    }

    #[test]
    fn delete_object_then_get_returns_no_such_key() {
        let (_dir, store) = tmp_store();
        store
            .create_bucket(CreateBucketInput {
                bucket: "bkt".into(),
            })
            .unwrap();
        store
            .put_object(PutObjectInput {
                bucket: "bkt".into(),
                key: "bye.txt".into(),
                body: b"bye".to_vec(),
                content_type: None,
                metadata: HashMap::new(),
                content_encoding: None,
                conditions: IfConditions::default(),
            })
            .unwrap();
        store
            .delete_object(DeleteObjectInput {
                bucket: "bkt".into(),
                key: "bye.txt".into(),
            })
            .unwrap();

        let err = store
            .get_object(GetObjectInput {
                bucket: "bkt".into(),
                key: "bye.txt".into(),
                range: None,
                conditions: IfConditions::default(),
            })
            .unwrap_err();
        assert_eq!(err.code(), "NoSuchKey");
    }

    #[test]
    fn put_on_missing_bucket_errors() {
        let (_dir, store) = tmp_store();
        let err = store
            .put_object(PutObjectInput {
                bucket: "no-such".into(),
                key: "f.txt".into(),
                body: vec![],
                content_type: None,
                metadata: HashMap::new(),
                content_encoding: None,
                conditions: IfConditions::default(),
            })
            .unwrap_err();
        assert_eq!(err.code(), "NoSuchBucket");
    }

    // ── ListObjectsV2 ─────────────────────────────────────────────────────

    fn put(store: &Store, bucket: &str, key: &str, body: &[u8]) {
        store
            .put_object(PutObjectInput {
                bucket: bucket.into(),
                key: key.into(),
                body: body.to_vec(),
                content_type: None,
                metadata: HashMap::new(),
                content_encoding: None,
                conditions: IfConditions::default(),
            })
            .unwrap();
    }

    #[test]
    fn list_all_keys() {
        let (_dir, store) = tmp_store();
        store
            .create_bucket(CreateBucketInput {
                bucket: "bkt".into(),
            })
            .unwrap();
        put(&store, "bkt", "a.txt", b"a");
        put(&store, "bkt", "b.txt", b"b");
        put(&store, "bkt", "c.txt", b"c");

        let out = store
            .list_objects_v2(ListObjectsV2Input {
                bucket: "bkt".into(),
                prefix: None,
                delimiter: None,
                max_keys: None,
                continuation_token: None,
            })
            .unwrap();

        assert_eq!(out.key_count, 3);
        assert!(!out.is_truncated);
        let keys: Vec<&str> = out.contents.iter().map(|e| e.key.as_str()).collect();
        assert_eq!(keys, vec!["a.txt", "b.txt", "c.txt"]);
    }

    #[test]
    fn list_with_prefix() {
        let (_dir, store) = tmp_store();
        store
            .create_bucket(CreateBucketInput {
                bucket: "bkt".into(),
            })
            .unwrap();
        put(&store, "bkt", "imgs/a.png", b"a");
        put(&store, "bkt", "imgs/b.png", b"b");
        put(&store, "bkt", "docs/c.txt", b"c");

        let out = store
            .list_objects_v2(ListObjectsV2Input {
                bucket: "bkt".into(),
                prefix: Some("imgs/".into()),
                delimiter: None,
                max_keys: None,
                continuation_token: None,
            })
            .unwrap();

        assert_eq!(out.key_count, 2);
        let keys: Vec<&str> = out.contents.iter().map(|e| e.key.as_str()).collect();
        assert_eq!(keys, vec!["imgs/a.png", "imgs/b.png"]);
    }

    #[test]
    fn list_with_delimiter_common_prefixes() {
        let (_dir, store) = tmp_store();
        store
            .create_bucket(CreateBucketInput {
                bucket: "bkt".into(),
            })
            .unwrap();
        put(&store, "bkt", "a/x.txt", b"x");
        put(&store, "bkt", "a/y.txt", b"y");
        put(&store, "bkt", "b/z.txt", b"z");
        put(&store, "bkt", "root.txt", b"r");

        let out = store
            .list_objects_v2(ListObjectsV2Input {
                bucket: "bkt".into(),
                prefix: None,
                delimiter: Some("/".into()),
                max_keys: None,
                continuation_token: None,
            })
            .unwrap();

        assert!(out.common_prefixes.contains(&"a/".to_string()));
        assert!(out.common_prefixes.contains(&"b/".to_string()));
        assert_eq!(out.contents.len(), 1);
        assert_eq!(out.contents[0].key, "root.txt");
    }

    #[test]
    fn list_pagination() {
        let (_dir, store) = tmp_store();
        store
            .create_bucket(CreateBucketInput {
                bucket: "bkt".into(),
            })
            .unwrap();
        for i in 0..5u8 {
            put(&store, "bkt", &format!("obj{i}.txt"), &[i]);
        }

        let page1 = store
            .list_objects_v2(ListObjectsV2Input {
                bucket: "bkt".into(),
                prefix: None,
                delimiter: None,
                max_keys: Some(2),
                continuation_token: None,
            })
            .unwrap();
        assert_eq!(page1.contents.len(), 2);
        assert!(page1.is_truncated);
        assert!(page1.next_continuation_token.is_some());

        let page2 = store
            .list_objects_v2(ListObjectsV2Input {
                bucket: "bkt".into(),
                prefix: None,
                delimiter: None,
                max_keys: Some(2),
                continuation_token: page1.next_continuation_token.clone(),
            })
            .unwrap();
        assert_eq!(page2.contents.len(), 2);

        let page3 = store
            .list_objects_v2(ListObjectsV2Input {
                bucket: "bkt".into(),
                prefix: None,
                delimiter: None,
                max_keys: Some(2),
                continuation_token: page2.next_continuation_token.clone(),
            })
            .unwrap();
        assert_eq!(page3.contents.len(), 1);
        assert!(!page3.is_truncated);

        // Ensure pages are non-overlapping and cover all 5 keys.
        let mut all: Vec<String> = page1
            .contents
            .iter()
            .chain(page2.contents.iter())
            .chain(page3.contents.iter())
            .map(|e| e.key.clone())
            .collect();
        all.sort();
        all.dedup();
        assert_eq!(all.len(), 5);
    }

    #[test]
    fn list_empty_bucket() {
        let (_dir, store) = tmp_store();
        store
            .create_bucket(CreateBucketInput {
                bucket: "empty".into(),
            })
            .unwrap();
        let out = store
            .list_objects_v2(ListObjectsV2Input {
                bucket: "empty".into(),
                prefix: None,
                delimiter: None,
                max_keys: None,
                continuation_token: None,
            })
            .unwrap();
        assert_eq!(out.key_count, 0);
        assert!(!out.is_truncated);
    }

    // ── CopyObject ────────────────────────────────────────────────────────

    #[test]
    fn copy_same_bucket() {
        let (_dir, store) = tmp_store();
        store
            .create_bucket(CreateBucketInput {
                bucket: "bkt".into(),
            })
            .unwrap();
        put(&store, "bkt", "src.txt", b"hello");

        let out = store
            .copy_object(CopyObjectInput {
                src_bucket: "bkt".into(),
                src_key: "src.txt".into(),
                dst_bucket: "bkt".into(),
                dst_key: "dst.txt".into(),
                metadata_directive: None,
                source_conditions: IfConditions::default(),
            })
            .unwrap();
        assert!(!out.etag.is_empty());

        let got = store
            .get_object(GetObjectInput {
                bucket: "bkt".into(),
                key: "dst.txt".into(),
                range: None,
                conditions: IfConditions::default(),
            })
            .unwrap();
        assert_eq!(got.body, b"hello");
    }

    #[test]
    fn copy_cross_bucket() {
        let (_dir, store) = tmp_store();
        store
            .create_bucket(CreateBucketInput {
                bucket: "src-bkt".into(),
            })
            .unwrap();
        store
            .create_bucket(CreateBucketInput {
                bucket: "dst-bkt".into(),
            })
            .unwrap();
        put(&store, "src-bkt", "file.txt", b"data");

        store
            .copy_object(CopyObjectInput {
                src_bucket: "src-bkt".into(),
                src_key: "file.txt".into(),
                dst_bucket: "dst-bkt".into(),
                dst_key: "copy.txt".into(),
                metadata_directive: None,
                source_conditions: IfConditions::default(),
            })
            .unwrap();

        let got = store
            .get_object(GetObjectInput {
                bucket: "dst-bkt".into(),
                key: "copy.txt".into(),
                range: None,
                conditions: IfConditions::default(),
            })
            .unwrap();
        assert_eq!(got.body, b"data");
    }

    #[test]
    fn copy_preserves_metadata_by_default() {
        let (_dir, store) = tmp_store();
        store
            .create_bucket(CreateBucketInput {
                bucket: "bkt".into(),
            })
            .unwrap();
        store
            .put_object(PutObjectInput {
                bucket: "bkt".into(),
                key: "src.txt".into(),
                body: b"hi".to_vec(),
                content_type: Some("text/plain".into()),
                metadata: HashMap::from([("owner".into(), "alice".into())]),
                content_encoding: None,
                conditions: IfConditions::default(),
            })
            .unwrap();

        store
            .copy_object(CopyObjectInput {
                src_bucket: "bkt".into(),
                src_key: "src.txt".into(),
                dst_bucket: "bkt".into(),
                dst_key: "dst.txt".into(),
                metadata_directive: None,
                source_conditions: IfConditions::default(),
            })
            .unwrap();

        let meta = store
            .head_object(HeadObjectInput {
                bucket: "bkt".into(),
                key: "dst.txt".into(),
                conditions: IfConditions::default(),
            })
            .unwrap();
        assert_eq!(meta.content_type, Some("text/plain".into()));
        assert_eq!(
            meta.user_metadata.get("owner").map(String::as_str),
            Some("alice")
        );
    }

    #[test]
    fn copy_replace_metadata() {
        let (_dir, store) = tmp_store();
        store
            .create_bucket(CreateBucketInput {
                bucket: "bkt".into(),
            })
            .unwrap();
        store
            .put_object(PutObjectInput {
                bucket: "bkt".into(),
                key: "src.txt".into(),
                body: b"hi".to_vec(),
                content_type: Some("text/plain".into()),
                metadata: HashMap::from([("owner".into(), "alice".into())]),
                content_encoding: None,
                conditions: IfConditions::default(),
            })
            .unwrap();

        store
            .copy_object(CopyObjectInput {
                src_bucket: "bkt".into(),
                src_key: "src.txt".into(),
                dst_bucket: "bkt".into(),
                dst_key: "dst.txt".into(),
                metadata_directive: Some(MetadataDirective::Replace {
                    content_type: Some("application/octet-stream".into()),
                    metadata: HashMap::from([("owner".into(), "bob".into())]),
                }),
                source_conditions: IfConditions::default(),
            })
            .unwrap();

        let meta = store
            .head_object(HeadObjectInput {
                bucket: "bkt".into(),
                key: "dst.txt".into(),
                conditions: IfConditions::default(),
            })
            .unwrap();
        assert_eq!(meta.content_type, Some("application/octet-stream".into()));
        assert_eq!(
            meta.user_metadata.get("owner").map(String::as_str),
            Some("bob")
        );
    }

    #[test]
    fn copy_missing_source_key() {
        let (_dir, store) = tmp_store();
        store
            .create_bucket(CreateBucketInput {
                bucket: "bkt".into(),
            })
            .unwrap();
        let err = store
            .copy_object(CopyObjectInput {
                src_bucket: "bkt".into(),
                src_key: "ghost.txt".into(),
                dst_bucket: "bkt".into(),
                dst_key: "dst.txt".into(),
                metadata_directive: None,
                source_conditions: IfConditions::default(),
            })
            .unwrap_err();
        assert_eq!(err.code(), "NoSuchKey");
    }
}
