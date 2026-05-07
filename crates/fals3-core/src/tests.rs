use std::collections::HashMap;

use crate::{
    error::Fals3Error,
    meta::{compute_etag, compute_md5_bytes, compute_multipart_etag},
    paths::{validate_bucket_name, validate_key},
    store::{
        AbortMultipartUploadInput, CompleteMultipartUploadInput, CompletedPart, CopyObjectInput,
        CreateBucketInput, CreateMultipartUploadInput, GetObjectInput, HeadObjectInput,
        IfConditions, ListObjectsV2Input, ListPartsInput, PutObjectInput, Store, UploadPartInput,
    },
};

// ─── Key validation edge cases ───────────────────────────────────────────────

#[test]
fn key_single_dot_valid() {
    assert!(validate_key(".").is_ok());
}

#[test]
fn key_double_slash_valid() {
    assert!(validate_key("a//b").is_ok());
}

#[test]
fn key_1024_chars_valid() {
    let key = "a".repeat(1024);
    assert!(validate_key(&key).is_ok());
}

#[test]
fn key_unicode_valid() {
    assert!(validate_key("こんにちは/世界.txt").is_ok());
}

#[test]
fn key_null_byte_in_middle_invalid() {
    assert!(validate_key("valid\0key").is_err());
}

// ─── Bucket name edge cases ───────────────────────────────────────────────────

#[test]
fn bucket_exactly_3_chars_valid() {
    assert!(validate_bucket_name("abc").is_ok());
}

#[test]
fn bucket_exactly_63_chars_valid() {
    let name = "a".repeat(63);
    assert!(validate_bucket_name(&name).is_ok());
}

#[test]
fn bucket_2_chars_invalid() {
    assert!(validate_bucket_name("ab").is_err());
}

#[test]
fn bucket_64_chars_invalid() {
    let name = "a".repeat(64);
    assert!(validate_bucket_name(&name).is_err());
}

#[test]
fn bucket_all_digits_valid() {
    assert!(validate_bucket_name("123").is_ok());
}

#[test]
fn bucket_single_hyphen_in_middle_valid() {
    assert!(validate_bucket_name("a-b").is_ok());
}

// ─── Pagination token round-trip ─────────────────────────────────────────────

#[test]
fn pagination_6_objects_3_pages() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();

    store
        .create_bucket(CreateBucketInput {
            bucket: "test".into(),
        })
        .unwrap();

    let keys = ["a", "b", "c", "d", "e", "f"];
    for key in &keys {
        store
            .put_object(PutObjectInput {
                bucket: "test".into(),
                key: key.to_string(),
                body: b"data".to_vec(),
                content_type: None,
                metadata: HashMap::new(),
                content_encoding: None,
                conditions: IfConditions::default(),
            })
            .unwrap();
    }

    let mut all_seen: Vec<String> = Vec::new();
    let mut token: Option<String> = None;
    let mut page_count = 0;

    loop {
        let out = store
            .list_objects_v2(ListObjectsV2Input {
                bucket: "test".into(),
                prefix: None,
                delimiter: None,
                max_keys: Some(2),
                continuation_token: token,
            })
            .unwrap();

        page_count += 1;
        for entry in &out.contents {
            assert!(
                !all_seen.contains(&entry.key),
                "key {} appeared twice",
                entry.key
            );
            all_seen.push(entry.key.clone());
        }

        if out.is_truncated {
            token = out.next_continuation_token;
            assert!(token.is_some(), "truncated response must have next token");
        } else {
            assert!(
                out.next_continuation_token.is_none(),
                "non-truncated response must not have next token"
            );
            break;
        }
    }

    assert_eq!(page_count, 3, "expected exactly 3 pages");
    assert_eq!(all_seen.len(), 6, "expected 6 keys total");

    // Verify all original keys were returned.
    for key in &keys {
        assert!(all_seen.contains(&key.to_string()), "missing key {key}");
    }
}

// ─── ETag stability ──────────────────────────────────────────────────────────

#[test]
fn etag_same_body_stable() {
    let body = b"hello world";
    assert_eq!(compute_etag(body), compute_etag(body));
}

#[test]
fn etag_different_bodies_differ() {
    assert_ne!(compute_etag(b"hello"), compute_etag(b"world"));
}

#[test]
fn etag_wrapped_in_double_quotes() {
    let etag = compute_etag(b"any content");
    assert!(etag.starts_with('"'), "etag must start with '\"'");
    assert!(etag.ends_with('"'), "etag must end with '\"'");
    assert!(etag.len() > 2, "etag must contain content between quotes");
}

// ─── Error code mapping ───────────────────────────────────────────────────────

#[test]
fn error_code_no_such_bucket() {
    assert_eq!(
        Fals3Error::NoSuchBucket { bucket: "b".into() }.code(),
        "NoSuchBucket"
    );
}

#[test]
fn error_code_bucket_already_exists() {
    assert_eq!(
        Fals3Error::BucketAlreadyExists { bucket: "b".into() }.code(),
        "BucketAlreadyExists"
    );
}

#[test]
fn error_code_bucket_not_empty() {
    assert_eq!(
        Fals3Error::BucketNotEmpty { bucket: "b".into() }.code(),
        "BucketNotEmpty"
    );
}

#[test]
fn error_code_invalid_bucket_name() {
    assert_eq!(
        Fals3Error::InvalidBucketName {
            name: "B".into(),
            reason: "uppercase".into()
        }
        .code(),
        "InvalidBucketName"
    );
}

#[test]
fn error_code_no_such_key() {
    assert_eq!(
        Fals3Error::NoSuchKey {
            bucket: "b".into(),
            key: "k".into()
        }
        .code(),
        "NoSuchKey"
    );
}

#[test]
fn error_code_invalid_object_key() {
    assert_eq!(
        Fals3Error::InvalidObjectKey {
            key: "".into(),
            reason: "empty".into()
        }
        .code(),
        "InvalidObjectKey"
    );
}

#[test]
fn error_code_precondition_failed() {
    assert_eq!(Fals3Error::PreconditionFailed.code(), "PreconditionFailed");
}

#[test]
fn error_code_not_modified() {
    assert_eq!(Fals3Error::NotModified.code(), "NotModified");
}

#[test]
fn error_code_no_such_upload() {
    assert_eq!(
        Fals3Error::NoSuchUpload {
            upload_id: "u".into()
        }
        .code(),
        "NoSuchUpload"
    );
}

#[test]
fn error_code_invalid_part() {
    assert_eq!(Fals3Error::InvalidPart.code(), "InvalidPart");
}

#[test]
fn error_code_invalid_part_order() {
    assert_eq!(Fals3Error::InvalidPartOrder.code(), "InvalidPartOrder");
}

#[test]
fn error_code_path_escape() {
    use std::path::PathBuf;
    assert_eq!(
        Fals3Error::PathEscape {
            path: PathBuf::from("/tmp/escape"),
            base: PathBuf::from("/tmp/safe")
        }
        .code(),
        "PathEscape"
    );
}

#[test]
fn error_code_io_internal() {
    let e = Fals3Error::Io(std::io::Error::other("disk full"));
    assert_eq!(e.code(), "InternalError");
}

#[test]
fn error_code_json_internal() {
    let json_err: serde_json::Error =
        serde_json::from_str::<serde_json::Value>("not json").unwrap_err();
    assert_eq!(Fals3Error::Json(json_err).code(), "InternalError");
}

#[test]
fn error_code_internal_string() {
    assert_eq!(
        Fals3Error::Internal("something went wrong".into()).code(),
        "InternalError"
    );
}

// ─── Conditional headers (preconditions) ─────────────────────────────────────

fn cond_store_with_object() -> (tempfile::TempDir, Store, String, u64) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    store
        .create_bucket(CreateBucketInput {
            bucket: "bkt".into(),
        })
        .unwrap();
    let put = store
        .put_object(PutObjectInput {
            bucket: "bkt".into(),
            key: "k".into(),
            body: b"hello".to_vec(),
            content_type: None,
            metadata: HashMap::new(),
            content_encoding: None,
            conditions: IfConditions::default(),
        })
        .unwrap();
    let meta = store
        .head_object(HeadObjectInput {
            bucket: "bkt".into(),
            key: "k".into(),
            conditions: IfConditions::default(),
        })
        .unwrap();
    (dir, store, put.etag, meta.last_modified)
}

#[test]
fn get_if_match_pass() {
    let (_d, s, etag, _) = cond_store_with_object();
    s.get_object(GetObjectInput {
        bucket: "bkt".into(),
        key: "k".into(),
        range: None,
        conditions: IfConditions {
            if_match: Some(etag),
            ..Default::default()
        },
    })
    .unwrap();
}

#[test]
fn get_if_match_wildcard_pass() {
    let (_d, s, _, _) = cond_store_with_object();
    s.get_object(GetObjectInput {
        bucket: "bkt".into(),
        key: "k".into(),
        range: None,
        conditions: IfConditions {
            if_match: Some("*".into()),
            ..Default::default()
        },
    })
    .unwrap();
}

#[test]
fn get_if_match_fail_returns_precondition_failed() {
    let (_d, s, _, _) = cond_store_with_object();
    let err = s
        .get_object(GetObjectInput {
            bucket: "bkt".into(),
            key: "k".into(),
            range: None,
            conditions: IfConditions {
                if_match: Some("\"deadbeef\"".into()),
                ..Default::default()
            },
        })
        .unwrap_err();
    assert_eq!(err.code(), "PreconditionFailed");
}

#[test]
fn get_if_none_match_returns_not_modified_when_etag_matches() {
    let (_d, s, etag, _) = cond_store_with_object();
    let err = s
        .get_object(GetObjectInput {
            bucket: "bkt".into(),
            key: "k".into(),
            range: None,
            conditions: IfConditions {
                if_none_match: Some(etag),
                ..Default::default()
            },
        })
        .unwrap_err();
    assert_eq!(err.code(), "NotModified");
}

#[test]
fn get_if_none_match_pass_when_etag_differs() {
    let (_d, s, _, _) = cond_store_with_object();
    s.get_object(GetObjectInput {
        bucket: "bkt".into(),
        key: "k".into(),
        range: None,
        conditions: IfConditions {
            if_none_match: Some("\"deadbeef\"".into()),
            ..Default::default()
        },
    })
    .unwrap();
}

#[test]
fn get_if_modified_since_returns_not_modified_when_unchanged() {
    let (_d, s, _, lm) = cond_store_with_object();
    let err = s
        .get_object(GetObjectInput {
            bucket: "bkt".into(),
            key: "k".into(),
            range: None,
            conditions: IfConditions {
                if_modified_since: Some(lm),
                ..Default::default()
            },
        })
        .unwrap_err();
    assert_eq!(err.code(), "NotModified");
}

#[test]
fn get_if_unmodified_since_fail_when_object_is_newer() {
    let (_d, s, _, lm) = cond_store_with_object();
    let err = s
        .get_object(GetObjectInput {
            bucket: "bkt".into(),
            key: "k".into(),
            range: None,
            conditions: IfConditions {
                if_unmodified_since: Some(lm.saturating_sub(1)),
                ..Default::default()
            },
        })
        .unwrap_err();
    assert_eq!(err.code(), "PreconditionFailed");
}

#[test]
fn get_if_match_takes_priority_over_if_none_match() {
    // If-Match (412) wins over If-None-Match (304) when both fire.
    let (_d, s, etag, _) = cond_store_with_object();
    let err = s
        .get_object(GetObjectInput {
            bucket: "bkt".into(),
            key: "k".into(),
            range: None,
            conditions: IfConditions {
                if_match: Some("\"deadbeef\"".into()),
                if_none_match: Some(etag),
                ..Default::default()
            },
        })
        .unwrap_err();
    assert_eq!(err.code(), "PreconditionFailed");
}

#[test]
fn put_if_none_match_star_fails_when_object_exists() {
    let (_d, s, _, _) = cond_store_with_object();
    let err = s
        .put_object(PutObjectInput {
            bucket: "bkt".into(),
            key: "k".into(),
            body: b"new".to_vec(),
            content_type: None,
            metadata: HashMap::new(),
            content_encoding: None,
            conditions: IfConditions {
                if_none_match: Some("*".into()),
                ..Default::default()
            },
        })
        .unwrap_err();
    assert_eq!(err.code(), "PreconditionFailed");
}

#[test]
fn put_if_none_match_star_succeeds_when_object_absent() {
    let dir = tempfile::tempdir().unwrap();
    let s = Store::open(dir.path()).unwrap();
    s.create_bucket(CreateBucketInput {
        bucket: "bkt".into(),
    })
    .unwrap();
    s.put_object(PutObjectInput {
        bucket: "bkt".into(),
        key: "k".into(),
        body: b"first".to_vec(),
        content_type: None,
        metadata: HashMap::new(),
        content_encoding: None,
        conditions: IfConditions {
            if_none_match: Some("*".into()),
            ..Default::default()
        },
    })
    .unwrap();
}

#[test]
fn put_if_match_succeeds_with_current_etag() {
    let (_d, s, etag, _) = cond_store_with_object();
    s.put_object(PutObjectInput {
        bucket: "bkt".into(),
        key: "k".into(),
        body: b"new".to_vec(),
        content_type: None,
        metadata: HashMap::new(),
        content_encoding: None,
        conditions: IfConditions {
            if_match: Some(etag),
            ..Default::default()
        },
    })
    .unwrap();
}

#[test]
fn put_if_match_fails_with_stale_etag() {
    let (_d, s, _, _) = cond_store_with_object();
    let err = s
        .put_object(PutObjectInput {
            bucket: "bkt".into(),
            key: "k".into(),
            body: b"new".to_vec(),
            content_type: None,
            metadata: HashMap::new(),
            content_encoding: None,
            conditions: IfConditions {
                if_match: Some("\"deadbeef\"".into()),
                ..Default::default()
            },
        })
        .unwrap_err();
    assert_eq!(err.code(), "PreconditionFailed");
}

#[test]
fn put_if_match_fails_when_object_absent() {
    let dir = tempfile::tempdir().unwrap();
    let s = Store::open(dir.path()).unwrap();
    s.create_bucket(CreateBucketInput {
        bucket: "bkt".into(),
    })
    .unwrap();
    let err = s
        .put_object(PutObjectInput {
            bucket: "bkt".into(),
            key: "k".into(),
            body: b"new".to_vec(),
            content_type: None,
            metadata: HashMap::new(),
            content_encoding: None,
            conditions: IfConditions {
                if_match: Some("\"any\"".into()),
                ..Default::default()
            },
        })
        .unwrap_err();
    assert_eq!(err.code(), "PreconditionFailed");
}

#[test]
fn copy_source_if_match_fail() {
    let (_d, s, _, _) = cond_store_with_object();
    let err = s
        .copy_object(CopyObjectInput {
            src_bucket: "bkt".into(),
            src_key: "k".into(),
            dst_bucket: "bkt".into(),
            dst_key: "dst".into(),
            metadata_directive: None,
            source_conditions: IfConditions {
                if_match: Some("\"deadbeef\"".into()),
                ..Default::default()
            },
        })
        .unwrap_err();
    assert_eq!(err.code(), "PreconditionFailed");
}

#[test]
fn copy_source_if_match_pass() {
    let (_d, s, etag, _) = cond_store_with_object();
    s.copy_object(CopyObjectInput {
        src_bucket: "bkt".into(),
        src_key: "k".into(),
        dst_bucket: "bkt".into(),
        dst_key: "dst".into(),
        metadata_directive: None,
        source_conditions: IfConditions {
            if_match: Some(etag),
            ..Default::default()
        },
    })
    .unwrap();
}

#[test]
fn etag_quote_normalisation_in_if_match() {
    // Provided etag without quotes still matches the stored quoted etag.
    let (_d, s, etag, _) = cond_store_with_object();
    let unquoted = etag.trim_matches('"').to_string();
    s.get_object(GetObjectInput {
        bucket: "bkt".into(),
        key: "k".into(),
        range: None,
        conditions: IfConditions {
            if_match: Some(unquoted),
            ..Default::default()
        },
    })
    .unwrap();
}

// ─── Multipart upload ────────────────────────────────────────────────────────

fn mp_store_with_bucket() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let s = Store::open(dir.path()).unwrap();
    s.create_bucket(CreateBucketInput {
        bucket: "bkt".into(),
    })
    .unwrap();
    (dir, s)
}

#[test]
fn multipart_etag_concat_md5s() {
    // Two parts, each "abc": md5("abc") = 900150983cd24fb0d6963f7d28e17f72.
    // multipart_etag = md5(<bytes of md5("abc")> || <bytes of md5("abc")>) + "-2".
    let part_md5 = compute_md5_bytes(b"abc");
    let etag = compute_multipart_etag(&[part_md5, part_md5]);
    let mut concat = Vec::new();
    concat.extend_from_slice(&part_md5);
    concat.extend_from_slice(&part_md5);
    let expected = format!("\"{:x}-2\"", md5::compute(&concat));
    assert_eq!(etag, expected);
    assert!(etag.ends_with("-2\""));
}

#[test]
fn multipart_full_roundtrip() {
    let (_d, s) = mp_store_with_bucket();

    let create = s
        .create_multipart_upload(CreateMultipartUploadInput {
            bucket: "bkt".into(),
            key: "big.bin".into(),
            content_type: Some("application/octet-stream".into()),
            metadata: HashMap::from([("k".into(), "v".into())]),
            content_encoding: None,
        })
        .unwrap();
    assert!(!create.upload_id.is_empty());

    let p1 = s
        .upload_part(UploadPartInput {
            upload_id: create.upload_id.clone(),
            part_number: 1,
            body: b"hello ".to_vec(),
        })
        .unwrap();
    let p2 = s
        .upload_part(UploadPartInput {
            upload_id: create.upload_id.clone(),
            part_number: 2,
            body: b"world".to_vec(),
        })
        .unwrap();
    assert_eq!(p1.part_number, 1);
    assert_eq!(p2.part_number, 2);

    let complete = s
        .complete_multipart_upload(CompleteMultipartUploadInput {
            upload_id: create.upload_id.clone(),
            parts: vec![
                CompletedPart {
                    part_number: 1,
                    etag: p1.etag.clone(),
                },
                CompletedPart {
                    part_number: 2,
                    etag: p2.etag.clone(),
                },
            ],
        })
        .unwrap();
    assert!(complete.etag.ends_with("-2\""));

    // Object now readable via standard GET.
    let got = s
        .get_object(GetObjectInput {
            bucket: "bkt".into(),
            key: "big.bin".into(),
            range: None,
            conditions: IfConditions::default(),
        })
        .unwrap();
    assert_eq!(got.body, b"hello world");
    assert_eq!(got.meta.etag, complete.etag);
    assert_eq!(
        got.meta.content_type,
        Some("application/octet-stream".into())
    );
    assert_eq!(
        got.meta.user_metadata.get("k").map(String::as_str),
        Some("v")
    );
    assert_eq!(got.meta.size, 11);
}

#[test]
fn multipart_metadata_preserved_through_complete() {
    let (_d, s) = mp_store_with_bucket();
    let create = s
        .create_multipart_upload(CreateMultipartUploadInput {
            bucket: "bkt".into(),
            key: "k".into(),
            content_type: Some("image/png".into()),
            metadata: HashMap::from([("uid".into(), "42".into())]),
            content_encoding: Some("gzip".into()),
        })
        .unwrap();
    let p = s
        .upload_part(UploadPartInput {
            upload_id: create.upload_id.clone(),
            part_number: 1,
            body: b"x".to_vec(),
        })
        .unwrap();
    s.complete_multipart_upload(CompleteMultipartUploadInput {
        upload_id: create.upload_id,
        parts: vec![CompletedPart {
            part_number: 1,
            etag: p.etag,
        }],
    })
    .unwrap();

    let meta = s
        .head_object(HeadObjectInput {
            bucket: "bkt".into(),
            key: "k".into(),
            conditions: IfConditions::default(),
        })
        .unwrap();
    assert_eq!(meta.content_type, Some("image/png".into()));
    assert_eq!(meta.content_encoding, Some("gzip".into()));
    assert_eq!(
        meta.user_metadata.get("uid").map(String::as_str),
        Some("42")
    );
}

#[test]
fn upload_part_rejects_unknown_upload_id() {
    let (_d, s) = mp_store_with_bucket();
    let err = s
        .upload_part(UploadPartInput {
            upload_id: "ghost".into(),
            part_number: 1,
            body: b"x".to_vec(),
        })
        .unwrap_err();
    assert_eq!(err.code(), "NoSuchUpload");
}

#[test]
fn upload_part_rejects_part_number_zero() {
    let (_d, s) = mp_store_with_bucket();
    let create = s
        .create_multipart_upload(CreateMultipartUploadInput {
            bucket: "bkt".into(),
            key: "k".into(),
            content_type: None,
            metadata: HashMap::new(),
            content_encoding: None,
        })
        .unwrap();
    let err = s
        .upload_part(UploadPartInput {
            upload_id: create.upload_id,
            part_number: 0,
            body: b"x".to_vec(),
        })
        .unwrap_err();
    assert_eq!(err.code(), "InvalidPart");
}

#[test]
fn upload_part_rejects_part_number_above_max() {
    let (_d, s) = mp_store_with_bucket();
    let create = s
        .create_multipart_upload(CreateMultipartUploadInput {
            bucket: "bkt".into(),
            key: "k".into(),
            content_type: None,
            metadata: HashMap::new(),
            content_encoding: None,
        })
        .unwrap();
    let err = s
        .upload_part(UploadPartInput {
            upload_id: create.upload_id,
            part_number: 10_001,
            body: b"x".to_vec(),
        })
        .unwrap_err();
    assert_eq!(err.code(), "InvalidPart");
}

#[test]
fn complete_rejects_unknown_upload() {
    let (_d, s) = mp_store_with_bucket();
    let err = s
        .complete_multipart_upload(CompleteMultipartUploadInput {
            upload_id: "ghost".into(),
            parts: vec![CompletedPart {
                part_number: 1,
                etag: "\"x\"".into(),
            }],
        })
        .unwrap_err();
    assert_eq!(err.code(), "NoSuchUpload");
}

#[test]
fn complete_rejects_empty_parts_list() {
    let (_d, s) = mp_store_with_bucket();
    let create = s
        .create_multipart_upload(CreateMultipartUploadInput {
            bucket: "bkt".into(),
            key: "k".into(),
            content_type: None,
            metadata: HashMap::new(),
            content_encoding: None,
        })
        .unwrap();
    let err = s
        .complete_multipart_upload(CompleteMultipartUploadInput {
            upload_id: create.upload_id,
            parts: vec![],
        })
        .unwrap_err();
    assert_eq!(err.code(), "InvalidPart");
}

#[test]
fn complete_rejects_descending_or_duplicate_part_numbers() {
    let (_d, s) = mp_store_with_bucket();
    let create = s
        .create_multipart_upload(CreateMultipartUploadInput {
            bucket: "bkt".into(),
            key: "k".into(),
            content_type: None,
            metadata: HashMap::new(),
            content_encoding: None,
        })
        .unwrap();
    let p1 = s
        .upload_part(UploadPartInput {
            upload_id: create.upload_id.clone(),
            part_number: 1,
            body: b"a".to_vec(),
        })
        .unwrap();
    let p2 = s
        .upload_part(UploadPartInput {
            upload_id: create.upload_id.clone(),
            part_number: 2,
            body: b"b".to_vec(),
        })
        .unwrap();

    // Descending
    let err = s
        .complete_multipart_upload(CompleteMultipartUploadInput {
            upload_id: create.upload_id.clone(),
            parts: vec![
                CompletedPart {
                    part_number: 2,
                    etag: p2.etag.clone(),
                },
                CompletedPart {
                    part_number: 1,
                    etag: p1.etag.clone(),
                },
            ],
        })
        .unwrap_err();
    assert_eq!(err.code(), "InvalidPartOrder");

    // Duplicate
    let err = s
        .complete_multipart_upload(CompleteMultipartUploadInput {
            upload_id: create.upload_id,
            parts: vec![
                CompletedPart {
                    part_number: 1,
                    etag: p1.etag.clone(),
                },
                CompletedPart {
                    part_number: 1,
                    etag: p1.etag,
                },
            ],
        })
        .unwrap_err();
    assert_eq!(err.code(), "InvalidPartOrder");
}

#[test]
fn complete_rejects_part_with_wrong_etag() {
    let (_d, s) = mp_store_with_bucket();
    let create = s
        .create_multipart_upload(CreateMultipartUploadInput {
            bucket: "bkt".into(),
            key: "k".into(),
            content_type: None,
            metadata: HashMap::new(),
            content_encoding: None,
        })
        .unwrap();
    let _ = s
        .upload_part(UploadPartInput {
            upload_id: create.upload_id.clone(),
            part_number: 1,
            body: b"a".to_vec(),
        })
        .unwrap();

    let err = s
        .complete_multipart_upload(CompleteMultipartUploadInput {
            upload_id: create.upload_id,
            parts: vec![CompletedPart {
                part_number: 1,
                etag: "\"deadbeef\"".into(),
            }],
        })
        .unwrap_err();
    assert_eq!(err.code(), "InvalidPart");
}

#[test]
fn complete_rejects_part_that_was_never_uploaded() {
    let (_d, s) = mp_store_with_bucket();
    let create = s
        .create_multipart_upload(CreateMultipartUploadInput {
            bucket: "bkt".into(),
            key: "k".into(),
            content_type: None,
            metadata: HashMap::new(),
            content_encoding: None,
        })
        .unwrap();
    let p1 = s
        .upload_part(UploadPartInput {
            upload_id: create.upload_id.clone(),
            part_number: 1,
            body: b"a".to_vec(),
        })
        .unwrap();

    let err = s
        .complete_multipart_upload(CompleteMultipartUploadInput {
            upload_id: create.upload_id,
            parts: vec![
                CompletedPart {
                    part_number: 1,
                    etag: p1.etag,
                },
                CompletedPart {
                    part_number: 5,
                    etag: "\"x\"".into(),
                },
            ],
        })
        .unwrap_err();
    assert_eq!(err.code(), "InvalidPart");
}

#[test]
fn abort_removes_upload_state() {
    let (_d, s) = mp_store_with_bucket();
    let create = s
        .create_multipart_upload(CreateMultipartUploadInput {
            bucket: "bkt".into(),
            key: "k".into(),
            content_type: None,
            metadata: HashMap::new(),
            content_encoding: None,
        })
        .unwrap();
    s.upload_part(UploadPartInput {
        upload_id: create.upload_id.clone(),
        part_number: 1,
        body: b"x".to_vec(),
    })
    .unwrap();
    s.abort_multipart_upload(AbortMultipartUploadInput {
        upload_id: create.upload_id.clone(),
    })
    .unwrap();

    // Subsequent ListParts on the aborted id should return NoSuchUpload.
    let err = s
        .list_parts(ListPartsInput {
            upload_id: create.upload_id,
        })
        .unwrap_err();
    assert_eq!(err.code(), "NoSuchUpload");
}

#[test]
fn abort_unknown_upload_is_idempotent() {
    let (_d, s) = mp_store_with_bucket();
    s.abort_multipart_upload(AbortMultipartUploadInput {
        upload_id: "ghost".into(),
    })
    .unwrap();
}

#[test]
fn list_parts_returns_uploaded_parts_in_order() {
    let (_d, s) = mp_store_with_bucket();
    let create = s
        .create_multipart_upload(CreateMultipartUploadInput {
            bucket: "bkt".into(),
            key: "k".into(),
            content_type: None,
            metadata: HashMap::new(),
            content_encoding: None,
        })
        .unwrap();
    let p3 = s
        .upload_part(UploadPartInput {
            upload_id: create.upload_id.clone(),
            part_number: 3,
            body: b"ccc".to_vec(),
        })
        .unwrap();
    let p1 = s
        .upload_part(UploadPartInput {
            upload_id: create.upload_id.clone(),
            part_number: 1,
            body: b"a".to_vec(),
        })
        .unwrap();
    let p2 = s
        .upload_part(UploadPartInput {
            upload_id: create.upload_id.clone(),
            part_number: 2,
            body: b"bb".to_vec(),
        })
        .unwrap();

    let listed = s
        .list_parts(ListPartsInput {
            upload_id: create.upload_id,
        })
        .unwrap();
    assert_eq!(listed.bucket, "bkt");
    assert_eq!(listed.key, "k");
    assert_eq!(listed.parts.len(), 3);
    assert_eq!(listed.parts[0].part_number, 1);
    assert_eq!(listed.parts[1].part_number, 2);
    assert_eq!(listed.parts[2].part_number, 3);
    assert_eq!(listed.parts[0].etag, p1.etag);
    assert_eq!(listed.parts[1].etag, p2.etag);
    assert_eq!(listed.parts[2].etag, p3.etag);
    assert_eq!(listed.parts[0].size, 1);
    assert_eq!(listed.parts[1].size, 2);
    assert_eq!(listed.parts[2].size, 3);
}

#[test]
fn re_upload_same_part_replaces_body_and_etag() {
    let (_d, s) = mp_store_with_bucket();
    let create = s
        .create_multipart_upload(CreateMultipartUploadInput {
            bucket: "bkt".into(),
            key: "k".into(),
            content_type: None,
            metadata: HashMap::new(),
            content_encoding: None,
        })
        .unwrap();
    let p1a = s
        .upload_part(UploadPartInput {
            upload_id: create.upload_id.clone(),
            part_number: 1,
            body: b"first".to_vec(),
        })
        .unwrap();
    let p1b = s
        .upload_part(UploadPartInput {
            upload_id: create.upload_id.clone(),
            part_number: 1,
            body: b"second-version".to_vec(),
        })
        .unwrap();
    assert_ne!(p1a.etag, p1b.etag);

    // Complete with the new etag — must succeed.
    let complete = s
        .complete_multipart_upload(CompleteMultipartUploadInput {
            upload_id: create.upload_id,
            parts: vec![CompletedPart {
                part_number: 1,
                etag: p1b.etag,
            }],
        })
        .unwrap();
    let got = s
        .get_object(GetObjectInput {
            bucket: "bkt".into(),
            key: "k".into(),
            range: None,
            conditions: IfConditions::default(),
        })
        .unwrap();
    assert_eq!(got.body, b"second-version");
    assert!(complete.etag.ends_with("-1\""));
}

#[test]
fn create_multipart_upload_validates_bucket_and_key() {
    let (_d, s) = mp_store_with_bucket();
    let err = s
        .create_multipart_upload(CreateMultipartUploadInput {
            bucket: "ghost".into(),
            key: "k".into(),
            content_type: None,
            metadata: HashMap::new(),
            content_encoding: None,
        })
        .unwrap_err();
    assert_eq!(err.code(), "NoSuchBucket");

    let err = s
        .create_multipart_upload(CreateMultipartUploadInput {
            bucket: "bkt".into(),
            key: "../escape".into(),
            content_type: None,
            metadata: HashMap::new(),
            content_encoding: None,
        })
        .unwrap_err();
    assert_eq!(err.code(), "InvalidObjectKey");
}
