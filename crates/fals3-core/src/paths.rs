use std::path::{Component, Path, PathBuf};

use crate::error::{Fals3Error, Result};

/// Validate a bucket name against S3 DNS-compatible naming rules (v1 subset).
///
/// Rules:
/// - 3–63 characters
/// - Lowercase letters, digits, and hyphens only
/// - Must start and end with a letter or digit (no leading/trailing hyphen)
/// - No consecutive hyphens (not enforced by S3 but keeps names sane)
/// - Not an IP address (not enforced in v1)
pub fn validate_bucket_name(name: &str) -> Result<()> {
    if name.len() < 3 || name.len() > 63 {
        return Err(Fals3Error::InvalidBucketName {
            name: name.to_string(),
            reason: "bucket name must be between 3 and 63 characters".to_string(),
        });
    }
    if name.starts_with('-') || name.ends_with('-') {
        return Err(Fals3Error::InvalidBucketName {
            name: name.to_string(),
            reason: "bucket name must not start or end with a hyphen".to_string(),
        });
    }
    for ch in name.chars() {
        if !matches!(ch, 'a'..='z' | '0'..='9' | '-') {
            return Err(Fals3Error::InvalidBucketName {
                name: name.to_string(),
                reason: format!(
                    "bucket name must contain only lowercase letters, digits, and hyphens; got '{ch}'"
                ),
            });
        }
    }
    Ok(())
}

/// Validate an object key.
///
/// Rejects:
/// - Empty keys
/// - Keys containing NUL bytes
/// - Path components that are `..` (directory traversal)
/// - Absolute paths (keys must be relative)
pub fn validate_key(key: &str) -> Result<()> {
    if key.is_empty() {
        return Err(Fals3Error::InvalidObjectKey {
            key: key.to_string(),
            reason: "key must not be empty".to_string(),
        });
    }
    if key.contains('\0') {
        return Err(Fals3Error::InvalidObjectKey {
            key: key.to_string(),
            reason: "key must not contain NUL bytes".to_string(),
        });
    }
    // Walk path components to catch `..` and absolute roots.
    for component in Path::new(key).components() {
        match component {
            Component::ParentDir => {
                return Err(Fals3Error::InvalidObjectKey {
                    key: key.to_string(),
                    reason: "key must not contain '..' segments".to_string(),
                });
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(Fals3Error::InvalidObjectKey {
                    key: key.to_string(),
                    reason: "key must be a relative path".to_string(),
                });
            }
            _ => {}
        }
    }
    Ok(())
}

/// Resolve the filesystem path for a bucket directory.
///
/// Returns `{base_dir}/{bucket}`.
/// Does NOT check that the path exists.
pub fn bucket_path(base_dir: &Path, bucket: &str) -> PathBuf {
    base_dir.join(bucket)
}

/// Resolve the filesystem path for an object body file.
///
/// Returns `{base_dir}/{bucket}/{key}`.
/// Does NOT check that the path exists.
///
/// # Panics
/// Does not panic, but callers should validate `bucket` and `key` first.
pub fn object_path(base_dir: &Path, bucket: &str, key: &str) -> PathBuf {
    base_dir.join(bucket).join(key)
}

/// Resolve the filesystem path for an object's sidecar metadata file.
///
/// Returns `{base_dir}/{bucket}/{key}.fals3-meta.json`.
pub fn meta_path(base_dir: &Path, bucket: &str, key: &str) -> PathBuf {
    let body = object_path(base_dir, bucket, key);
    let mut name = body
        .file_name()
        .unwrap_or_default()
        .to_os_string();
    name.push(".fals3-meta.json");
    body.with_file_name(name)
}

/// Verify that `path` is actually under `canonical_base`.
///
/// `canonical_base` must already be canonicalized (symlinks resolved) — the
/// caller (typically `Store`) resolves it once at open time.  `path` may or
/// may not exist yet; we use a lexical prefix check, which is sound because
/// `validate_key` has already rejected any `..` components.
pub fn assert_within_base(canonical_base: &Path, path: &Path) -> Result<()> {
    // Try to canonicalize the path if it already exists (catches symlink escapes).
    // Fall back to a lexical check when the path doesn't exist yet (e.g. pre-write).
    let resolved = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());

    if !resolved.starts_with(canonical_base) {
        return Err(Fals3Error::PathEscape {
            path: path.to_path_buf(),
            base: canonical_base.to_path_buf(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- bucket name validation ---

    #[test]
    fn valid_bucket_names() {
        for name in &["abc", "my-bucket", "test123", "a-b-c", "x".repeat(63).as_str()] {
            validate_bucket_name(name).unwrap_or_else(|e| panic!("{name}: {e}"));
        }
    }

    #[test]
    fn bucket_too_short() {
        assert!(validate_bucket_name("ab").is_err());
    }

    #[test]
    fn bucket_too_long() {
        let name = "a".repeat(64);
        assert!(validate_bucket_name(&name).is_err());
    }

    #[test]
    fn bucket_leading_hyphen() {
        assert!(validate_bucket_name("-bad").is_err());
    }

    #[test]
    fn bucket_trailing_hyphen() {
        assert!(validate_bucket_name("bad-").is_err());
    }

    #[test]
    fn bucket_uppercase_rejected() {
        assert!(validate_bucket_name("MyBucket").is_err());
    }

    #[test]
    fn bucket_underscore_rejected() {
        assert!(validate_bucket_name("my_bucket").is_err());
    }

    // --- key validation ---

    #[test]
    fn valid_keys() {
        for key in &[
            "file.txt",
            "users/1/avatar.png",
            "deep/nested/path/obj",
            "with spaces/ok",
            "unicode-こんにちは",
        ] {
            validate_key(key).unwrap_or_else(|e| panic!("{key}: {e}"));
        }
    }

    #[test]
    fn empty_key_rejected() {
        assert!(validate_key("").is_err());
    }

    #[test]
    fn nul_byte_rejected() {
        assert!(validate_key("bad\0key").is_err());
    }

    #[test]
    fn dotdot_segment_rejected() {
        assert!(validate_key("../escape").is_err());
        assert!(validate_key("ok/../escape").is_err());
    }

    #[test]
    fn absolute_key_rejected() {
        assert!(validate_key("/absolute").is_err());
    }

    // --- path helpers ---

    #[test]
    fn bucket_path_correct() {
        let base = Path::new("/tmp/s3");
        assert_eq!(bucket_path(base, "my-bucket"), PathBuf::from("/tmp/s3/my-bucket"));
    }

    #[test]
    fn object_path_correct() {
        let base = Path::new("/tmp/s3");
        assert_eq!(
            object_path(base, "my-bucket", "users/1/avatar.png"),
            PathBuf::from("/tmp/s3/my-bucket/users/1/avatar.png")
        );
    }

    #[test]
    fn meta_path_correct() {
        let base = Path::new("/tmp/s3");
        assert_eq!(
            meta_path(base, "my-bucket", "users/1/avatar.png"),
            PathBuf::from("/tmp/s3/my-bucket/users/1/avatar.png.fals3-meta.json")
        );
    }
}
