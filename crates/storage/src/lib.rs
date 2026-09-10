//! Cloud-neutral storage primitives for untrusted upload artifacts.
//!
//! Keys are deliberately constrained to relative, portable path segments so a
//! caller cannot escape the configured local development root. The same key
//! contract is suitable for an S3-compatible implementation later.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use chrono::Utc;
use hmac::{Hmac, Mac};
use reqwest::blocking::{Client, RequestBuilder};
use reqwest::StatusCode;
use sha2::{Digest, Sha256};
use url::Url;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ObjectKey(String);

impl ObjectKey {
    pub fn parse(value: impl Into<String>) -> Result<Self, StorageError> {
        let value = value.into();
        if value.is_empty()
            || value.starts_with('/')
            || value
                .split('/')
                .any(|part| part.is_empty() || part == "." || part == "..")
            || value.contains('\\')
            || value.contains('\0')
        {
            return Err(StorageError::InvalidKey);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("storage key must be a non-empty relative slash-separated path")]
    InvalidKey,
    #[error("object was not found")]
    NotFound,
    #[error("storage I/O failed")]
    Io(#[source] io::Error),
    #[error("storage configuration is invalid")]
    Configuration,
    #[error("remote object storage request failed")]
    Remote,
    #[error("remote object storage returned an invalid response")]
    InvalidResponse,
}

/// Minimal object-store surface shared by local and future S3 backends.
pub trait ObjectStorage: Send + Sync {
    fn put(&self, key: &ObjectKey, bytes: &[u8]) -> Result<(), StorageError>;
    fn get(&self, key: &ObjectKey) -> Result<Vec<u8>, StorageError>;
    fn delete(&self, key: &ObjectKey) -> Result<(), StorageError>;
}

#[derive(Clone, Debug)]
pub struct LocalObjectStorage {
    root: PathBuf,
}

impl LocalObjectStorage {
    pub fn new(root: impl Into<PathBuf>) -> Result<Self, StorageError> {
        let root = root.into();
        fs::create_dir_all(&root).map_err(StorageError::Io)?;
        Ok(Self { root })
    }

    fn path(&self, key: &ObjectKey) -> PathBuf {
        self.root.join(Path::new(key.as_str()))
    }
}

impl ObjectStorage for LocalObjectStorage {
    fn put(&self, key: &ObjectKey, bytes: &[u8]) -> Result<(), StorageError> {
        let destination = self.path(key);
        let parent = destination.parent().ok_or(StorageError::InvalidKey)?;
        fs::create_dir_all(parent).map_err(StorageError::Io)?;
        // A sibling temp file plus rename ensures readers never observe a
        // partially written upload after a crash or interrupted request.
        let temporary = destination.with_extension("uploading");
        fs::write(&temporary, bytes).map_err(StorageError::Io)?;
        fs::rename(&temporary, &destination).map_err(StorageError::Io)
    }

    fn get(&self, key: &ObjectKey) -> Result<Vec<u8>, StorageError> {
        fs::read(self.path(key)).map_err(|error| {
            if error.kind() == io::ErrorKind::NotFound {
                StorageError::NotFound
            } else {
                StorageError::Io(error)
            }
        })
    }

    fn delete(&self, key: &ObjectKey) -> Result<(), StorageError> {
        fs::remove_file(self.path(key)).map_err(|error| {
            if error.kind() == io::ErrorKind::NotFound {
                StorageError::NotFound
            } else {
                StorageError::Io(error)
            }
        })
    }
}

/// Path-style AWS Signature V4 client compatible with MinIO and S3 services.
///
/// It deliberately uses a small, blocking surface because object operations
/// are currently performed from request handlers through the synchronous
/// `ObjectStorage` trait. Callers that need high-throughput remote storage can
/// evolve the trait to async without changing the key contract.
#[derive(Clone)]
pub struct S3ObjectStorage {
    client: Client,
    endpoint: Url,
    bucket: String,
    region: String,
    access_key_id: String,
    secret_access_key: String,
}

impl S3ObjectStorage {
    pub fn new(
        endpoint: &str,
        bucket: &str,
        region: &str,
        access_key_id: &str,
        secret_access_key: &str,
    ) -> Result<Self, StorageError> {
        let endpoint = Url::parse(endpoint).map_err(|_| StorageError::Configuration)?;
        if !matches!(endpoint.scheme(), "http" | "https")
            || endpoint.host_str().is_none()
            || bucket.is_empty()
            || region.is_empty()
            || access_key_id.is_empty()
            || secret_access_key.is_empty()
        {
            return Err(StorageError::Configuration);
        }
        let client = Client::builder()
            .build()
            .map_err(|_| StorageError::Configuration)?;
        Ok(Self {
            client,
            endpoint,
            bucket: bucket.to_string(),
            region: region.to_string(),
            access_key_id: access_key_id.to_string(),
            secret_access_key: secret_access_key.to_string(),
        })
    }

    fn object_url(&self, key: &ObjectKey) -> Result<Url, StorageError> {
        let base = self.endpoint.as_str().trim_end_matches('/');
        Url::parse(&format!(
            "{base}/{}/{}",
            self.bucket,
            percent_encode_path(key.as_str())
        ))
        .map_err(|_| StorageError::Configuration)
    }

    fn signed_request(
        &self,
        method: &str,
        key: &ObjectKey,
        payload: &[u8],
    ) -> Result<RequestBuilder, StorageError> {
        let url = self.object_url(key)?;
        self.signed_request_url(method, url, payload)
    }

    fn signed_request_url(
        &self,
        method: &str,
        url: Url,
        payload: &[u8],
    ) -> Result<RequestBuilder, StorageError> {
        let now = Utc::now();
        let amz_date = now.format("%Y%m%dT%H%M%SZ").to_string();
        let date_stamp = now.format("%Y%m%d").to_string();
        let payload_hash = hex_sha256(payload);
        let host = url
            .host_str()
            .map(|host| match url.port() {
                Some(port) => format!("{host}:{port}"),
                None => host.to_string(),
            })
            .ok_or(StorageError::Configuration)?;
        let canonical_uri = url.path();
        let canonical_headers =
            format!("host:{host}\nx-amz-content-sha256:{payload_hash}\nx-amz-date:{amz_date}\n");
        let signed_headers = "host;x-amz-content-sha256;x-amz-date";
        let canonical_query = url.query().unwrap_or("");
        let canonical_request = format!(
            "{method}\n{canonical_uri}\n{canonical_query}\n{canonical_headers}\n{signed_headers}\n{payload_hash}"
        );
        let credential_scope = format!("{date_stamp}/{}/s3/aws4_request", self.region);
        let string_to_sign = format!(
            "AWS4-HMAC-SHA256\n{amz_date}\n{credential_scope}\n{}",
            hex_sha256(canonical_request.as_bytes())
        );
        let signing_key = signing_key(&self.secret_access_key, &date_stamp, &self.region)?;
        let signature = hex_hmac(&signing_key, &string_to_sign)?;
        let authorization = format!(
            "AWS4-HMAC-SHA256 Credential={}/{credential_scope}, SignedHeaders={signed_headers}, Signature={signature}",
            self.access_key_id
        );

        Ok(self
            .client
            .request(
                method.parse().map_err(|_| StorageError::Configuration)?,
                url,
            )
            .header("host", host)
            .header("x-amz-content-sha256", payload_hash)
            .header("x-amz-date", amz_date)
            .header("authorization", authorization))
    }

    fn execute(
        &self,
        request: RequestBuilder,
    ) -> Result<reqwest::blocking::Response, StorageError> {
        request.send().map_err(|_| StorageError::Remote)
    }

    /// Return every object key below `prefix` using the S3 ListObjectsV2 API.
    /// The request is paginated, so callers do not silently miss imports from
    /// a busy partner bucket.
    pub fn list_prefix(&self, prefix: &str) -> Result<Vec<ObjectKey>, StorageError> {
        let mut keys = Vec::new();
        let mut continuation_token: Option<String> = None;

        loop {
            let base = self.endpoint.as_str().trim_end_matches('/');
            let mut query = format!("list-type=2&prefix={}", percent_encode_query(prefix));
            if let Some(token) = &continuation_token {
                query.push_str("&continuation-token=");
                query.push_str(&percent_encode_query(token));
            }
            let url = Url::parse(&format!("{base}/{}?{query}", self.bucket))
                .map_err(|_| StorageError::Configuration)?;
            let response = self.execute(self.signed_request_url("GET", url, b"")?)?;
            if !response.status().is_success() {
                return Err(StorageError::Remote);
            }
            let body = response.text().map_err(|_| StorageError::Remote)?;
            for key in xml_tag_values(&body, "Key") {
                keys.push(ObjectKey::parse(key)?);
            }

            let truncated = xml_tag_values(&body, "IsTruncated")
                .into_iter()
                .next()
                .is_some_and(|value| value.eq_ignore_ascii_case("true"));
            if !truncated {
                break;
            }
            continuation_token = xml_tag_values(&body, "NextContinuationToken")
                .into_iter()
                .next();
            if continuation_token.is_none() {
                return Err(StorageError::InvalidResponse);
            }
        }
        Ok(keys)
    }
}

impl ObjectStorage for S3ObjectStorage {
    fn put(&self, key: &ObjectKey, bytes: &[u8]) -> Result<(), StorageError> {
        let response =
            self.execute(self.signed_request("PUT", key, bytes)?.body(bytes.to_vec()))?;
        if response.status().is_success() {
            Ok(())
        } else {
            Err(StorageError::Remote)
        }
    }

    fn get(&self, key: &ObjectKey) -> Result<Vec<u8>, StorageError> {
        let response = self.execute(self.signed_request("GET", key, b"")?)?;
        if response.status() == StatusCode::NOT_FOUND {
            return Err(StorageError::NotFound);
        }
        if !response.status().is_success() {
            return Err(StorageError::Remote);
        }
        response
            .bytes()
            .map(|b| b.to_vec())
            .map_err(|_| StorageError::Remote)
    }

    fn delete(&self, key: &ObjectKey) -> Result<(), StorageError> {
        let response = self.execute(self.signed_request("DELETE", key, b"")?)?;
        if response.status() == StatusCode::NOT_FOUND {
            return Err(StorageError::NotFound);
        }
        if response.status().is_success() {
            Ok(())
        } else {
            Err(StorageError::Remote)
        }
    }
}

fn percent_encode_path(value: &str) -> String {
    value
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                (byte as char).to_string()
            }
            _ => format!("%{byte:02X}"),
        })
        .collect()
}

fn percent_encode_query(value: &str) -> String {
    value
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (byte as char).to_string()
            }
            _ => format!("%{byte:02X}"),
        })
        .collect()
}

fn xml_tag_values(body: &str, tag: &str) -> Vec<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let mut values = Vec::new();
    let mut remaining = body;
    while let Some(start) = remaining.find(&open) {
        let content = &remaining[start + open.len()..];
        let Some(end) = content.find(&close) else {
            break;
        };
        values.push(xml_unescape(&content[..end]));
        remaining = &content[end + close.len()..];
    }
    values
}

fn xml_unescape(value: &str) -> String {
    value
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
}

fn hex_sha256(value: &[u8]) -> String {
    hex_bytes(&Sha256::digest(value))
}

fn hmac(key: &[u8], value: &str) -> Result<Vec<u8>, StorageError> {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).map_err(|_| StorageError::Configuration)?;
    mac.update(value.as_bytes());
    Ok(mac.finalize().into_bytes().to_vec())
}

fn hex_hmac(key: &[u8], value: &str) -> Result<String, StorageError> {
    Ok(hex_bytes(&hmac(key, value)?))
}

fn hex_bytes(value: &[u8]) -> String {
    value.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn signing_key(secret: &str, date_stamp: &str, region: &str) -> Result<Vec<u8>, StorageError> {
    let date = hmac(format!("AWS4{secret}").as_bytes(), date_stamp)?;
    let region = hmac(&date, region)?;
    let service = hmac(&region, "s3")?;
    hmac(&service, "aws4_request")
}

#[cfg(test)]
mod tests {
    use super::{
        percent_encode_path, percent_encode_query, signing_key, xml_tag_values, LocalObjectStorage,
        ObjectKey, ObjectStorage, S3ObjectStorage, StorageError,
    };
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn rejects_path_traversal_and_nonportable_keys() {
        for key in ["", "/etc/passwd", "a/../b", "a//b", "a\\b"] {
            assert!(matches!(
                ObjectKey::parse(key),
                Err(StorageError::InvalidKey)
            ));
        }
    }

    #[test]
    fn stores_and_removes_bytes_under_the_root() {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("openclaim-storage-{suffix}"));
        let store = LocalObjectStorage::new(&root).unwrap();
        let key = ObjectKey::parse("knowledge/payer-guide.txt").unwrap();
        store.put(&key, b"synthetic fixture only").unwrap();
        assert_eq!(store.get(&key).unwrap(), b"synthetic fixture only");
        store.delete(&key).unwrap();
        assert!(matches!(store.get(&key), Err(StorageError::NotFound)));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn s3_uses_path_style_urls_and_encodes_object_keys() {
        let store = S3ObjectStorage::new(
            "http://minio:9000",
            "denial-artifacts",
            "us-east-1",
            "access",
            "secret",
        )
        .unwrap();
        let key = ObjectKey::parse("knowledge/a report.txt").unwrap();

        assert_eq!(
            store.object_url(&key).unwrap().as_str(),
            "http://minio:9000/denial-artifacts/knowledge/a%20report.txt"
        );
        assert_eq!(percent_encode_path("a b/c"), "a%20b/c");
    }

    #[test]
    fn s3_signing_key_is_deterministic() {
        let first = signing_key("secret", "20260910", "us-east-1").unwrap();
        let second = signing_key("secret", "20260910", "us-east-1").unwrap();

        assert_eq!(first, second);
        assert_eq!(first.len(), 32);
    }

    #[test]
    fn list_response_keys_are_unescaped_and_query_values_are_canonical() {
        let body = "<ListBucketResult><Contents><Key>edi/inbound/a&amp;b.835</Key></Contents><IsTruncated>true</IsTruncated><NextContinuationToken>next&amp;token</NextContinuationToken></ListBucketResult>";
        assert_eq!(xml_tag_values(body, "Key"), vec!["edi/inbound/a&b.835"]);
        assert_eq!(
            xml_tag_values(body, "NextContinuationToken"),
            vec!["next&token"]
        );
        assert_eq!(
            percent_encode_query("edi/inbound a&b"),
            "edi%2Finbound%20a%26b"
        );
    }
}
