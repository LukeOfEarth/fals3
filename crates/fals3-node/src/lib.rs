// fals3-node: NAPI-RS layer that exposes fals3-core to JavaScript/TypeScript.

#![allow(clippy::pedantic)]

use std::{collections::HashMap, sync::Arc};

use napi::bindgen_prelude::*;
use napi_derive::napi;

use fals3_core::{
    store::{
        AbortMultipartUploadInput, CompleteMultipartUploadInput, CompletedPart, CopyObjectInput,
        CreateBucketInput, CreateMultipartUploadInput, DeleteBucketInput, DeleteObjectInput,
        GetObjectInput, HeadBucketInput, HeadObjectInput, ListObjectsV2Input, ListPartsInput,
        MetadataDirective, PutObjectInput, Store, UploadPartInput,
    },
    Fals3Error, IfConditions,
};

// ─── Error conversion ────────────────────────────────────────────────────────

/// Throw a `Fals3Error` as a JavaScript `Error` whose `code` property is the
/// AWS-style error code (e.g. `"NoSuchBucket"`).
///
/// On the JS side:
///
/// ```js
/// try { s3.headBucket('ghost'); }
/// catch (err) {
///   err.code     // "NoSuchBucket"
///   err.message  // "[NoSuchBucket] The specified bucket does not exist"
/// }
/// ```
///
/// `napi::Error` is generic over the status type (`Error<S: AsRef<str>>`), and
/// when napi-rs writes the error into JS via `napi_create_error`, the status
/// string becomes the JS `error.code`. We bypass the default `napi::Status`
/// enum by constructing a `JsError<String>` with our AWS code as the status,
/// throwing it manually, and returning `Status::PendingException` so napi-rs
/// re-uses the already-thrown exception instead of constructing a new one.
fn throw_aws_err(env: Env, e: Fals3Error) -> napi::Error {
    let code = e.code().to_string();
    let message = format!("[{}] {}", code, e);
    let custom: napi::Error<String> = napi::Error::new(code, message);
    unsafe { napi::JsError::<String>::from(custom).throw_into(env.raw()) };
    napi::Error::new(napi::Status::PendingException, String::new())
}

// ─── JS-visible types ────────────────────────────────────────────────────────

#[napi(object)]
pub struct JsObjectMeta {
    pub etag: String,
    pub last_modified: f64,
    pub content_type: Option<String>,
    pub user_metadata: HashMap<String, String>,
    pub content_encoding: Option<String>,
    pub storage_class: String,
    pub size: f64,
}

#[napi(object)]
pub struct JsObjectEntry {
    pub key: String,
    pub etag: String,
    pub size: f64,
    pub last_modified: f64,
    pub storage_class: String,
}

#[napi(object)]
pub struct JsListOutput {
    pub contents: Vec<JsObjectEntry>,
    pub common_prefixes: Vec<String>,
    pub is_truncated: bool,
    pub next_continuation_token: Option<String>,
    pub key_count: u32,
}

#[napi(object)]
pub struct JsCopyOutput {
    pub etag: String,
    pub last_modified: f64,
}

/// HTTP-style precondition headers, accepted by `getObject`, `headObject`,
/// `putObject`, and `copyObject` (where they apply to the *source* object).
///
/// All fields are optional.  Pass `undefined` (or omit the argument entirely)
/// when no precondition applies.
///
/// ```ts
/// // Atomic create: succeed only if `key` does not yet exist.
/// s3.putObject('bkt', 'key', body, undefined, undefined, undefined, {
///   ifNoneMatch: '*',
/// });
///
/// // Optimistic concurrency: only overwrite if ETag still matches.
/// s3.putObject('bkt', 'key', body, undefined, undefined, undefined, {
///   ifMatch: prevEtag,
/// });
/// ```
#[napi(object)]
pub struct JsIfConditions {
    pub if_match: Option<String>,
    pub if_none_match: Option<String>,
    /// Unix timestamp in seconds.
    pub if_modified_since: Option<f64>,
    /// Unix timestamp in seconds.
    pub if_unmodified_since: Option<f64>,
}

fn to_core_conditions(c: Option<JsIfConditions>) -> IfConditions {
    match c {
        None => IfConditions::default(),
        Some(c) => IfConditions {
            if_match: c.if_match,
            if_none_match: c.if_none_match,
            if_modified_since: c.if_modified_since.map(|f| f as u64),
            if_unmodified_since: c.if_unmodified_since.map(|f| f as u64),
        },
    }
}

// ─── Multipart upload types (JS-visible) ─────────────────────────────────────

#[napi(object)]
pub struct JsCreateMultipartUploadOutput {
    pub bucket: String,
    pub key: String,
    pub upload_id: String,
}

#[napi(object)]
pub struct JsUploadPartOutput {
    pub part_number: u32,
    pub etag: String,
}

/// Identifier of an uploaded part to include in `completeMultipartUpload`.
#[napi(object)]
pub struct JsCompletedPart {
    pub part_number: u32,
    pub etag: String,
}

#[napi(object)]
pub struct JsCompleteMultipartUploadOutput {
    pub bucket: String,
    pub key: String,
    /// AWS-style multipart ETag: `"<md5-of-md5s>-<part-count>"`.
    pub etag: String,
}

#[napi(object)]
pub struct JsPartEntry {
    pub part_number: u32,
    pub etag: String,
    pub size: f64,
    pub last_modified: f64,
}

#[napi(object)]
pub struct JsListPartsOutput {
    pub bucket: String,
    pub key: String,
    pub parts: Vec<JsPartEntry>,
}

#[napi(object)]
pub struct JsGetObjectOutput {
    pub body: Buffer,
    pub meta: JsObjectMeta,
}

#[napi(object)]
pub struct JsOpenOptions {
    pub base_dir: String,
}

// ─── Fals3 NAPI class ────────────────────────────────────────────────────────

/// JavaScript-facing S3 simulator class.
///
/// ```ts
/// const s3 = await Fals3.open({ baseDir: '/tmp/fals3-test' });
/// await s3.createBucket({ bucket: 'my-bucket' });
/// ```
#[napi]
pub struct Fals3 {
    inner: Arc<Store>,
}

#[napi]
impl Fals3 {
    /// Open (or create) a `Fals3` instance rooted at `options.baseDir`.
    #[napi(factory)]
    pub fn open(env: Env, options: JsOpenOptions) -> napi::Result<Self> {
        let store = Store::open(&options.base_dir).map_err(|e| throw_aws_err(env, e))?;
        Ok(Self {
            inner: Arc::new(store),
        })
    }

    // ── Bucket operations ─────────────────────────────────────────────────

    #[napi]
    pub fn create_bucket(&self, env: Env, bucket: String) -> napi::Result<()> {
        self.inner
            .create_bucket(CreateBucketInput { bucket })
            .map_err(|e| throw_aws_err(env, e))
    }

    #[napi]
    pub fn delete_bucket(&self, env: Env, bucket: String, force: Option<bool>) -> napi::Result<()> {
        self.inner
            .delete_bucket(DeleteBucketInput {
                bucket,
                force: force.unwrap_or(false),
            })
            .map_err(|e| throw_aws_err(env, e))
    }

    #[napi]
    pub fn head_bucket(&self, env: Env, bucket: String) -> napi::Result<()> {
        self.inner
            .head_bucket(HeadBucketInput { bucket })
            .map_err(|e| throw_aws_err(env, e))
    }

    // ── Object operations ─────────────────────────────────────────────────

    /// Put an object.  Returns the ETag.
    ///
    /// `conditions` (optional) accepts precondition headers — most usefully
    /// `ifNoneMatch: '*'` (atomic create) or `ifMatch: <etag>` (optimistic
    /// concurrency).  Failed preconditions throw with `err.code === 'PreconditionFailed'`.
    #[napi]
    pub fn put_object(
        &self,
        env: Env,
        bucket: String,
        key: String,
        body: Buffer,
        content_type: Option<String>,
        metadata: Option<HashMap<String, String>>,
        content_encoding: Option<String>,
        conditions: Option<JsIfConditions>,
    ) -> napi::Result<String> {
        let out = self
            .inner
            .put_object(PutObjectInput {
                bucket,
                key,
                body: body.to_vec(),
                content_type,
                metadata: metadata.unwrap_or_default(),
                content_encoding,
                conditions: to_core_conditions(conditions),
            })
            .map_err(|e| throw_aws_err(env, e))?;
        Ok(out.etag)
    }

    /// Get an object.  Returns body as a `Buffer` plus metadata.
    ///
    /// `conditions` (optional) accepts precondition headers.  Failed `ifMatch` /
    /// `ifUnmodifiedSince` throw with `err.code === 'PreconditionFailed'`;
    /// failed `ifNoneMatch` / `ifModifiedSince` throw with `err.code === 'NotModified'`.
    #[napi]
    pub fn get_object(
        &self,
        env: Env,
        bucket: String,
        key: String,
        range_start: Option<f64>,
        range_end: Option<f64>,
        conditions: Option<JsIfConditions>,
    ) -> napi::Result<JsGetObjectOutput> {
        let range = match (range_start, range_end) {
            (Some(s), Some(e)) => Some((s as u64, e as u64)),
            _ => None,
        };
        let out = self
            .inner
            .get_object(GetObjectInput {
                bucket,
                key,
                range,
                conditions: to_core_conditions(conditions),
            })
            .map_err(|e| throw_aws_err(env, e))?;
        Ok(JsGetObjectOutput {
            body: Buffer::from(out.body),
            meta: meta_to_js(out.meta),
        })
    }

    #[napi]
    pub fn head_object(
        &self,
        env: Env,
        bucket: String,
        key: String,
        conditions: Option<JsIfConditions>,
    ) -> napi::Result<JsObjectMeta> {
        let meta = self
            .inner
            .head_object(HeadObjectInput {
                bucket,
                key,
                conditions: to_core_conditions(conditions),
            })
            .map_err(|e| throw_aws_err(env, e))?;
        Ok(meta_to_js(meta))
    }

    #[napi]
    pub fn delete_object(&self, env: Env, bucket: String, key: String) -> napi::Result<()> {
        self.inner
            .delete_object(DeleteObjectInput { bucket, key })
            .map_err(|e| throw_aws_err(env, e))
    }

    /// ListObjectsV2.
    #[napi]
    pub fn list_objects_v2(
        &self,
        env: Env,
        bucket: String,
        prefix: Option<String>,
        delimiter: Option<String>,
        max_keys: Option<u32>,
        continuation_token: Option<String>,
    ) -> napi::Result<JsListOutput> {
        let out = self
            .inner
            .list_objects_v2(ListObjectsV2Input {
                bucket,
                prefix,
                delimiter,
                max_keys,
                continuation_token,
            })
            .map_err(|e| throw_aws_err(env, e))?;

        Ok(JsListOutput {
            contents: out
                .contents
                .into_iter()
                .map(|e| JsObjectEntry {
                    key: e.key,
                    etag: e.etag,
                    size: e.size as f64,
                    last_modified: e.last_modified as f64,
                    storage_class: e.storage_class,
                })
                .collect(),
            common_prefixes: out.common_prefixes,
            is_truncated: out.is_truncated,
            next_continuation_token: out.next_continuation_token,
            key_count: out.key_count,
        })
    }

    /// CopyObject.  Pass `replaceMetadata: { contentType?, metadata? }` to use REPLACE directive.
    ///
    /// `sourceConditions` (optional) accepts precondition headers evaluated
    /// against the **source** object (mirrors S3's `x-amz-copy-source-if-*`).
    #[napi]
    pub fn copy_object(
        &self,
        env: Env,
        src_bucket: String,
        src_key: String,
        dst_bucket: String,
        dst_key: String,
        replace_content_type: Option<String>,
        replace_metadata: Option<HashMap<String, String>>,
        source_conditions: Option<JsIfConditions>,
    ) -> napi::Result<JsCopyOutput> {
        let metadata_directive = if replace_content_type.is_some() || replace_metadata.is_some() {
            Some(MetadataDirective::Replace {
                content_type: replace_content_type,
                metadata: replace_metadata.unwrap_or_default(),
            })
        } else {
            None
        };

        let out = self
            .inner
            .copy_object(CopyObjectInput {
                src_bucket,
                src_key,
                dst_bucket,
                dst_key,
                metadata_directive,
                source_conditions: to_core_conditions(source_conditions),
            })
            .map_err(|e| throw_aws_err(env, e))?;

        Ok(JsCopyOutput {
            etag: out.etag,
            last_modified: out.last_modified as f64,
        })
    }

    // ── Multipart upload ──────────────────────────────────────────────────

    /// Begin a multipart upload.  Returns an `uploadId` to pass to subsequent
    /// `uploadPart`, `listParts`, `completeMultipartUpload`, and
    /// `abortMultipartUpload` calls.
    #[napi]
    pub fn create_multipart_upload(
        &self,
        env: Env,
        bucket: String,
        key: String,
        content_type: Option<String>,
        metadata: Option<HashMap<String, String>>,
        content_encoding: Option<String>,
    ) -> napi::Result<JsCreateMultipartUploadOutput> {
        let out = self
            .inner
            .create_multipart_upload(CreateMultipartUploadInput {
                bucket,
                key,
                content_type,
                metadata: metadata.unwrap_or_default(),
                content_encoding,
            })
            .map_err(|e| throw_aws_err(env, e))?;
        Ok(JsCreateMultipartUploadOutput {
            bucket: out.bucket,
            key: out.key,
            upload_id: out.upload_id,
        })
    }

    /// Upload one part of an in-flight multipart upload.  `partNumber` must
    /// be in `1..=10000`.  Returns the part's MD5 ETag — keep it and pass
    /// it back in `completeMultipartUpload`.
    #[napi]
    pub fn upload_part(
        &self,
        env: Env,
        upload_id: String,
        part_number: u32,
        body: Buffer,
    ) -> napi::Result<JsUploadPartOutput> {
        let out = self
            .inner
            .upload_part(UploadPartInput {
                upload_id,
                part_number,
                body: body.to_vec(),
            })
            .map_err(|e| throw_aws_err(env, e))?;
        Ok(JsUploadPartOutput {
            part_number: out.part_number,
            etag: out.etag,
        })
    }

    /// Finalise a multipart upload by concatenating its parts in
    /// ascending `partNumber` order and writing the assembled object.
    #[napi]
    pub fn complete_multipart_upload(
        &self,
        env: Env,
        upload_id: String,
        parts: Vec<JsCompletedPart>,
    ) -> napi::Result<JsCompleteMultipartUploadOutput> {
        let out = self
            .inner
            .complete_multipart_upload(CompleteMultipartUploadInput {
                upload_id,
                parts: parts
                    .into_iter()
                    .map(|p| CompletedPart {
                        part_number: p.part_number,
                        etag: p.etag,
                    })
                    .collect(),
            })
            .map_err(|e| throw_aws_err(env, e))?;
        Ok(JsCompleteMultipartUploadOutput {
            bucket: out.bucket,
            key: out.key,
            etag: out.etag,
        })
    }

    /// Cancel an in-flight multipart upload and discard its uploaded parts.
    /// Idempotent — aborting an unknown `uploadId` succeeds silently.
    #[napi]
    pub fn abort_multipart_upload(&self, env: Env, upload_id: String) -> napi::Result<()> {
        self.inner
            .abort_multipart_upload(AbortMultipartUploadInput { upload_id })
            .map_err(|e| throw_aws_err(env, e))
    }

    /// List the parts already uploaded for an in-flight multipart upload.
    #[napi]
    pub fn list_parts(&self, env: Env, upload_id: String) -> napi::Result<JsListPartsOutput> {
        let out = self
            .inner
            .list_parts(ListPartsInput { upload_id })
            .map_err(|e| throw_aws_err(env, e))?;
        Ok(JsListPartsOutput {
            bucket: out.bucket,
            key: out.key,
            parts: out
                .parts
                .into_iter()
                .map(|p| JsPartEntry {
                    part_number: p.part_number,
                    etag: p.etag,
                    size: p.size as f64,
                    last_modified: p.last_modified as f64,
                })
                .collect(),
        })
    }

    /// Return the version of the fals3-node package.
    #[napi]
    pub fn version() -> String {
        env!("CARGO_PKG_VERSION").to_string()
    }
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

fn meta_to_js(m: fals3_core::ObjectMeta) -> JsObjectMeta {
    JsObjectMeta {
        etag: m.etag,
        last_modified: m.last_modified as f64,
        content_type: m.content_type,
        user_metadata: m.user_metadata,
        content_encoding: m.content_encoding,
        storage_class: m.storage_class,
        size: m.size as f64,
    }
}
