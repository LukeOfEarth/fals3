use std::path::PathBuf;

/// All errors produced by fals3-core.
///
/// The `code` method returns the AWS-compatible error code string so that
/// the NAPI layer (and JS tests) can assert `err.code === 'NoSuchBucket'`.
#[derive(Debug, thiserror::Error)]
pub enum Fals3Error {
    // --- bucket errors ---
    #[error("The specified bucket does not exist")]
    NoSuchBucket { bucket: String },

    #[error("The requested bucket name is not available")]
    BucketAlreadyExists { bucket: String },

    #[error("The bucket you tried to delete is not empty")]
    BucketNotEmpty { bucket: String },

    #[error("Invalid bucket name '{name}': {reason}")]
    InvalidBucketName { name: String, reason: String },

    // --- object errors ---
    #[error("The specified key does not exist")]
    NoSuchKey { bucket: String, key: String },

    #[error("Invalid object key '{key}': {reason}")]
    InvalidObjectKey { key: String, reason: String },

    // --- conditional request errors ---
    #[error("At least one of the pre-conditions you specified did not hold")]
    PreconditionFailed,

    #[error("The specified copy source is not modified")]
    NotModified,

    // --- multipart errors ---
    #[error("The specified upload does not exist")]
    NoSuchUpload { upload_id: String },

    #[error("One or more of the specified parts could not be found")]
    InvalidPart,

    #[error("The list of parts was not in ascending order")]
    InvalidPartOrder,

    // --- security / path errors ---
    #[error("Path '{path}' escapes base directory '{base}'")]
    PathEscape { path: PathBuf, base: PathBuf },

    // --- I/O errors ---
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    // --- serialization errors ---
    #[error("Metadata serialization error: {0}")]
    Json(#[from] serde_json::Error),

    // --- internal ---
    #[error("Internal error: {0}")]
    Internal(String),
}

impl Fals3Error {
    /// AWS-compatible error code, suitable for `err.code` in JS.
    pub fn code(&self) -> &'static str {
        match self {
            Self::NoSuchBucket { .. } => "NoSuchBucket",
            Self::BucketAlreadyExists { .. } => "BucketAlreadyExists",
            Self::BucketNotEmpty { .. } => "BucketNotEmpty",
            Self::InvalidBucketName { .. } => "InvalidBucketName",
            Self::NoSuchKey { .. } => "NoSuchKey",
            Self::InvalidObjectKey { .. } => "InvalidObjectKey",
            Self::PreconditionFailed => "PreconditionFailed",
            Self::NotModified => "NotModified",
            Self::NoSuchUpload { .. } => "NoSuchUpload",
            Self::InvalidPart => "InvalidPart",
            Self::InvalidPartOrder => "InvalidPartOrder",
            Self::PathEscape { .. } => "PathEscape",
            Self::Io(_) => "InternalError",
            Self::Json(_) => "InternalError",
            Self::Internal(_) => "InternalError",
        }
    }

    /// HTTP status code analogue (for documentation and future HTTP layer).
    pub fn http_status(&self) -> u16 {
        match self {
            Self::NoSuchBucket { .. } => 404,
            Self::BucketAlreadyExists { .. } => 409,
            Self::BucketNotEmpty { .. } => 409,
            Self::InvalidBucketName { .. } => 400,
            Self::NoSuchKey { .. } => 404,
            Self::InvalidObjectKey { .. } => 400,
            Self::PreconditionFailed => 412,
            Self::NotModified => 304,
            Self::NoSuchUpload { .. } => 404,
            Self::InvalidPart => 400,
            Self::InvalidPartOrder => 400,
            Self::PathEscape { .. } => 400,
            Self::Io(_) => 500,
            Self::Json(_) => 500,
            Self::Internal(_) => 500,
        }
    }
}

/// Convenience alias used throughout fals3-core.
pub type Result<T> = std::result::Result<T, Fals3Error>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_such_bucket_code() {
        let e = Fals3Error::NoSuchBucket { bucket: "x".into() };
        assert_eq!(e.code(), "NoSuchBucket");
        assert_eq!(e.http_status(), 404);
    }

    #[test]
    fn bucket_already_exists_code() {
        let e = Fals3Error::BucketAlreadyExists { bucket: "x".into() };
        assert_eq!(e.code(), "BucketAlreadyExists");
        assert_eq!(e.http_status(), 409);
    }

    #[test]
    fn no_such_key_code() {
        let e = Fals3Error::NoSuchKey { bucket: "b".into(), key: "k".into() };
        assert_eq!(e.code(), "NoSuchKey");
        assert_eq!(e.http_status(), 404);
    }

    #[test]
    fn precondition_failed_code() {
        assert_eq!(Fals3Error::PreconditionFailed.code(), "PreconditionFailed");
        assert_eq!(Fals3Error::PreconditionFailed.http_status(), 412);
    }

    #[test]
    fn io_error_maps_to_internal() {
        let e = Fals3Error::Io(std::io::Error::new(std::io::ErrorKind::Other, "disk full"));
        assert_eq!(e.code(), "InternalError");
        assert_eq!(e.http_status(), 500);
    }
}
