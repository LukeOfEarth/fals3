use std::{
    collections::HashMap,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};

use crate::error::Result;

/// Metadata stored alongside every object as a `.fals3-meta.json` sidecar.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObjectMeta {
    /// Strong ETag: MD5 of body bytes as lowercase hex, wrapped in double-quotes.
    /// e.g. `"d41d8cd98f00b204e9800998ecf8427e"`
    pub etag: String,

    /// Unix timestamp (seconds) when the object was last written.
    pub last_modified: u64,

    /// MIME type supplied by the caller (e.g. `image/png`).
    pub content_type: Option<String>,

    /// User-defined metadata (`x-amz-meta-*` equivalent).
    #[serde(default)]
    pub user_metadata: HashMap<String, String>,

    /// Content-Encoding header value, if any.
    pub content_encoding: Option<String>,

    /// Storage class stub — always `"STANDARD"` in v1.
    #[serde(default = "default_storage_class")]
    pub storage_class: String,

    /// Object size in bytes.
    pub size: u64,
}

fn default_storage_class() -> String {
    "STANDARD".to_string()
}

impl ObjectMeta {
    /// Compute the ETag for `body` and build a new `ObjectMeta`.
    pub fn new(
        body: &[u8],
        content_type: Option<String>,
        user_metadata: HashMap<String, String>,
        content_encoding: Option<String>,
    ) -> Self {
        let etag = compute_etag(body);
        let last_modified = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        Self {
            etag,
            last_modified,
            content_type,
            user_metadata,
            content_encoding,
            storage_class: default_storage_class(),
            size: body.len() as u64,
        }
    }

    /// Build an `ObjectMeta` with a caller-supplied ETag.
    ///
    /// Used by `complete_multipart_upload`, where the ETag is the AWS
    /// multipart ETag (`"<md5-of-md5s>-<count>"`) rather than the MD5 of the
    /// assembled body.
    pub fn with_explicit_etag(
        size: u64,
        etag: String,
        content_type: Option<String>,
        user_metadata: HashMap<String, String>,
        content_encoding: Option<String>,
    ) -> Self {
        let last_modified = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        Self {
            etag,
            last_modified,
            content_type,
            user_metadata,
            content_encoding,
            storage_class: default_storage_class(),
            size,
        }
    }

    /// Read sidecar from disk.
    pub fn read(sidecar_path: &Path) -> Result<Self> {
        let bytes = std::fs::read(sidecar_path)?;
        let meta = serde_json::from_slice(&bytes)?;
        Ok(meta)
    }

    /// Write sidecar atomically: write to a temp file then rename.
    pub fn write(&self, sidecar_path: &Path) -> Result<()> {
        let json = serde_json::to_vec_pretty(self)?;
        // Place the temp file alongside the final sidecar so the rename is on
        // the same filesystem (guarantees atomicity on POSIX).
        let tmp_path = sidecar_path.with_extension("tmp");
        std::fs::write(&tmp_path, &json)?;
        std::fs::rename(&tmp_path, sidecar_path)?;
        Ok(())
    }
}

/// Compute `"<md5-hex>"` ETag from body bytes (matches AWS S3 single-part uploads).
pub fn compute_etag(body: &[u8]) -> String {
    let digest = md5::compute(body);
    format!("\"{digest:x}\"")
}

/// Raw 16-byte MD5 digest of `body`, used to build multipart ETags.
pub fn compute_md5_bytes(body: &[u8]) -> [u8; 16] {
    md5::compute(body).0
}

/// AWS-compatible multipart ETag: the MD5 of the concatenated raw MD5 digests
/// of each part, formatted as `"<hex>-<part-count>"`.
///
/// ```text
/// final = MD5(part1_md5_bytes || part2_md5_bytes || ... || partN_md5_bytes)
/// etag  = "{final_hex}-{N}"
/// ```
pub fn compute_multipart_etag(part_md5s: &[[u8; 16]]) -> String {
    let mut concat = Vec::with_capacity(part_md5s.len() * 16);
    for d in part_md5s {
        concat.extend_from_slice(d);
    }
    let final_md5 = md5::compute(&concat);
    let n = part_md5s.len();
    format!("\"{final_md5:x}-{n}\"")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn etag_empty_body() {
        // MD5 of empty string is well-known.
        assert_eq!(compute_etag(b""), "\"d41d8cd98f00b204e9800998ecf8427e\"");
    }

    #[test]
    fn etag_hello() {
        // MD5("hello") = 5d41402abc4b2a76b9719d911017c592
        assert_eq!(
            compute_etag(b"hello"),
            "\"5d41402abc4b2a76b9719d911017c592\""
        );
    }

    #[test]
    fn meta_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let sidecar = dir.path().join("obj.fals3-meta.json");

        let original = ObjectMeta::new(
            b"hello world",
            Some("text/plain".into()),
            HashMap::from([("uid".into(), "42".into())]),
            None,
        );
        original.write(&sidecar).unwrap();

        let loaded = ObjectMeta::read(&sidecar).unwrap();
        assert_eq!(loaded.etag, original.etag);
        assert_eq!(loaded.content_type, Some("text/plain".into()));
        assert_eq!(
            loaded.user_metadata.get("uid").map(String::as_str),
            Some("42")
        );
        assert_eq!(loaded.size, 11);
        assert_eq!(loaded.storage_class, "STANDARD");
    }

    #[test]
    fn atomic_write_replaces_existing() {
        let dir = tempfile::tempdir().unwrap();
        let sidecar = dir.path().join("obj.fals3-meta.json");

        let m1 = ObjectMeta::new(b"v1", Some("text/plain".into()), HashMap::new(), None);
        m1.write(&sidecar).unwrap();

        let m2 = ObjectMeta::new(b"version2", None, HashMap::new(), None);
        m2.write(&sidecar).unwrap();

        let loaded = ObjectMeta::read(&sidecar).unwrap();
        assert_eq!(loaded.etag, m2.etag);
        assert_eq!(loaded.size, 8);
    }
}
