//! Blob store: local disk by default, S3-compatible optional (ADR-0010).

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use futures::stream::BoxStream;
use futures::{StreamExt, TryStreamExt};
use object_store::aws::AmazonS3Builder;
use object_store::local::LocalFileSystem;
use object_store::path::Path as ObjPath;
use object_store::signer::Signer;
use object_store::{ObjectStore, ObjectStoreExt, WriteMultipart};

use crate::config::Config;

pub type ByteStream = BoxStream<'static, std::io::Result<Bytes>>;

/// What a [`hashing`] stream let through: its sha256 and size.
#[derive(Clone)]
pub struct Hashed(Arc<std::sync::Mutex<(sha2::Sha256, u64)>>);

impl Hashed {
    /// The hex sha256 and byte count so far (all of it once the stream ended).
    pub fn finish(&self) -> (String, u64) {
        use sha2::Digest;
        let g = self.0.lock().expect("not poisoned");
        (hex::encode(g.0.clone().finalize()), g.1)
    }
    pub fn bytes(&self) -> u64 {
        self.0.lock().expect("not poisoned").1
    }
}

/// `body`, hashed as it streams; past `limit` bytes it fails with `too_large`
/// (LFS objects, registry blobs: checked against their digest after).
pub fn hashing(body: ByteStream, limit: Option<u64>, too_large: &'static str) -> (ByteStream, Hashed) {
    use sha2::Digest;
    let hashed = Hashed(Arc::new(std::sync::Mutex::new((sha2::Sha256::new(), 0))));
    let h = hashed.clone();
    let stream = body
        .and_then(move |chunk| {
            let mut g = h.0.lock().expect("not poisoned");
            g.1 += chunk.len() as u64;
            let over = limit.is_some_and(|l| g.1 > l);
            if !over {
                g.0.update(&chunk);
            }
            async move { if over { Err(std::io::Error::other(too_large)) } else { Ok(chunk) } }
        })
        .boxed();
    (stream, hashed)
}

#[async_trait]
pub trait BlobStore: Send + Sync + 'static {
    /// Stream `body` to `key`. Returns the number of bytes written.
    async fn put_stream(&self, key: &str, body: ByteStream) -> anyhow::Result<u64>;
    async fn put_bytes(&self, key: &str, data: Bytes) -> anyhow::Result<()>;
    /// `(size, stream)`, or None when the key does not exist.
    async fn get(&self, key: &str) -> anyhow::Result<Option<(u64, ByteStream)>>;
    async fn size(&self, key: &str) -> anyhow::Result<Option<u64>>;
    /// Bytes `start..end` of an object (reading inside archives).
    async fn get_range(&self, key: &str, start: u64, end: u64) -> anyhow::Result<Bytes>;
    /// Bytes `start..end`, streamed.
    async fn get_range_stream(&self, key: &str, start: u64, end: u64) -> anyhow::Result<ByteStream>;
    async fn delete(&self, key: &str) -> anyhow::Result<()>;
    async fn rename(&self, from: &str, to: &str) -> anyhow::Result<()>;
    /// A presigned GET URL when the backend supports one (S3); None means the
    /// caller streams the object itself.
    async fn presigned_get(&self, key: &str, ttl: Duration) -> anyhow::Result<Option<url::Url>>;
    /// A presigned GET that makes the backend answer as a download with this
    /// type and `Content-Disposition` (S3's `response-*` parameters); None when
    /// the backend cannot.
    async fn presigned_download(&self, key: &str, ttl: Duration, content_type: &str, disposition: &str)
    -> anyhow::Result<Option<url::Url>>;
    fn backend(&self) -> &'static str;
    /// Every object, in no particular order (Admin > Storage scans).
    fn list(&self) -> BoxStream<'_, anyhow::Result<BlobMeta>>;
}

/// An object as a listing shows it.
#[derive(Debug, Clone)]
pub struct BlobMeta {
    pub key: String,
    pub size: u64,
    pub modified: chrono::DateTime<chrono::Utc>,
}

pub struct ObjectStoreBlobs {
    store: Arc<dyn ObjectStore>,
    signer: Option<Arc<dyn Signer>>,
    backend: &'static str,
    presign_downloads: bool,
}

impl ObjectStoreBlobs {
    pub fn from_config(cfg: &Config) -> anyhow::Result<Self> {
        match cfg.blobs.backend.as_str() {
            "local" | "" => {
                let dir = cfg.blobs_dir();
                std::fs::create_dir_all(&dir)?;
                let fs = LocalFileSystem::new_with_prefix(&dir)?;
                Ok(ObjectStoreBlobs {
                    store: Arc::new(fs),
                    signer: None,
                    backend: "local",
                    presign_downloads: cfg.blobs.s3_presign_downloads,
                })
            }
            "s3" => {
                let b = &cfg.blobs;
                let mut builder = AmazonS3Builder::from_env().with_bucket_name(&b.s3_bucket).with_region(&b.s3_region);
                if !b.s3_endpoint.is_empty() {
                    builder = builder.with_endpoint(&b.s3_endpoint).with_virtual_hosted_style_request(false);
                }
                if !b.s3_access_key_id.is_empty() {
                    builder = builder.with_access_key_id(&b.s3_access_key_id).with_secret_access_key(&b.s3_secret_access_key);
                }
                if b.s3_allow_http {
                    builder = builder.with_allow_http(true);
                }
                let s3 = Arc::new(builder.build()?);
                Ok(ObjectStoreBlobs {
                    store: s3.clone(),
                    signer: Some(s3),
                    backend: "s3",
                    presign_downloads: cfg.blobs.s3_presign_downloads,
                })
            }
            other => anyhow::bail!("unknown blobs.backend {other}"),
        }
    }
}

#[async_trait]
impl BlobStore for ObjectStoreBlobs {
    async fn put_stream(&self, key: &str, mut body: ByteStream) -> anyhow::Result<u64> {
        let path = ObjPath::from(key);
        let upload = self.store.put_multipart(&path).await?;
        let mut w = WriteMultipart::new(upload);
        let mut n = 0u64;
        while let Some(chunk) = body.next().await {
            let chunk = match chunk {
                Ok(c) => c,
                Err(e) => {
                    w.abort().await.ok();
                    return Err(e.into());
                }
            };
            n += chunk.len() as u64;
            w.wait_for_capacity(8).await?;
            w.write(&chunk);
        }
        w.finish().await?;
        Ok(n)
    }

    async fn put_bytes(&self, key: &str, data: Bytes) -> anyhow::Result<()> {
        self.store.put(&ObjPath::from(key), data.into()).await?;
        Ok(())
    }

    async fn get(&self, key: &str) -> anyhow::Result<Option<(u64, ByteStream)>> {
        match self.store.get(&ObjPath::from(key)).await {
            Ok(r) => {
                let size = r.meta.size;
                let s = r.into_stream().map_err(std::io::Error::other).boxed();
                Ok(Some((size, s)))
            }
            Err(object_store::Error::NotFound { .. }) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    async fn get_range(&self, key: &str, start: u64, end: u64) -> anyhow::Result<Bytes> {
        Ok(self.store.get_range(&ObjPath::from(key), start..end).await?)
    }

    async fn get_range_stream(&self, key: &str, start: u64, end: u64) -> anyhow::Result<ByteStream> {
        let opts = object_store::GetOptions { range: Some(object_store::GetRange::Bounded(start..end)), ..Default::default() };
        let r = self.store.get_opts(&ObjPath::from(key), opts).await?;
        Ok(r.into_stream().map_err(std::io::Error::other).boxed())
    }

    async fn size(&self, key: &str) -> anyhow::Result<Option<u64>> {
        match self.store.head(&ObjPath::from(key)).await {
            Ok(m) => Ok(Some(m.size)),
            Err(object_store::Error::NotFound { .. }) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    async fn delete(&self, key: &str) -> anyhow::Result<()> {
        match self.store.delete(&ObjPath::from(key)).await {
            Ok(()) | Err(object_store::Error::NotFound { .. }) => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    async fn rename(&self, from: &str, to: &str) -> anyhow::Result<()> {
        self.store.rename(&ObjPath::from(from), &ObjPath::from(to)).await?;
        Ok(())
    }

    async fn presigned_get(&self, key: &str, ttl: Duration) -> anyhow::Result<Option<url::Url>> {
        match &self.signer {
            Some(s) => Ok(Some(s.signed_url(axum::http::Method::GET, &ObjPath::from(key), ttl).await?)),
            None => Ok(None),
        }
    }

    async fn presigned_download(
        &self,
        key: &str,
        ttl: Duration,
        content_type: &str,
        disposition: &str,
    ) -> anyhow::Result<Option<url::Url>> {
        let Some(s) = self.signer.as_ref().filter(|_| self.presign_downloads) else { return Ok(None) };
        let opts = object_store::signer::SignedUrlOptions::new()
            .with_query([("response-content-type", content_type.to_string()), ("response-content-disposition", disposition.to_string())]);
        match s.signed_url_opts(axum::http::Method::GET, &ObjPath::from(key), ttl, &opts).await {
            Ok(u) => Ok(Some(u)),
            Err(object_store::Error::NotSupported { .. }) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    fn list(&self) -> BoxStream<'_, anyhow::Result<BlobMeta>> {
        self.store
            .list(None)
            .map(|m| m.map(|m| BlobMeta { key: m.location.to_string(), size: m.size, modified: m.last_modified }).map_err(Into::into))
            .boxed()
    }

    fn backend(&self) -> &'static str {
        self.backend
    }
}

/// `lfs/ab/cd/abcd…` — fan out so no directory holds millions of files.
pub fn lfs_key(oid: &str) -> String {
    format!("lfs/{}/{}/{}", &oid[0..2], &oid[2..4], oid)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn local_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = Config::default();
        cfg.blobs.local_dir = Some(dir.path().to_path_buf());
        let b = ObjectStoreBlobs::from_config(&cfg).unwrap();
        let body = futures::stream::iter(vec![Ok(Bytes::from_static(b"hello ")), Ok(Bytes::from_static(b"world"))]).boxed();
        assert_eq!(b.put_stream("tmp/x", body).await.unwrap(), 11);
        b.rename("tmp/x", "a/b/c").await.unwrap();
        assert_eq!(b.size("a/b/c").await.unwrap(), Some(11));
        let (size, s) = b.get("a/b/c").await.unwrap().unwrap();
        let data: Vec<Bytes> = s.try_collect().await.unwrap();
        assert_eq!(size, 11);
        assert_eq!(data.concat(), b"hello world");
        assert!(b.get("nope").await.unwrap().is_none());
        assert!(b.presigned_get("a/b/c", Duration::from_secs(60)).await.unwrap().is_none());
        b.delete("a/b/c").await.unwrap();
        assert_eq!(b.size("a/b/c").await.unwrap(), None);
    }
}

/// Everything but URL-unreserved characters, `/` included: one path segment.
const SEGMENT: &percent_encoding::AsciiSet = &percent_encoding::NON_ALPHANUMERIC.remove(b'-').remove(b'.').remove(b'_').remove(b'~');

pub fn encode_segment(s: &str) -> String {
    percent_encoding::utf8_percent_encode(s, SEGMENT).to_string()
}

/// `Content-Disposition` for a file named `name`: an ASCII `filename` for old
/// clients and the exact name as RFC 5987 `filename*`.
pub fn disposition(name: &str, inline: bool) -> String {
    let base = name.rsplit('/').next().unwrap_or(name);
    let ascii: String = base.chars().map(|c| if c.is_ascii_alphanumeric() || "-_.".contains(c) { c } else { '-' }).collect();
    let exact = encode_segment(base);
    format!("{}; filename=\"{ascii}\"; filename*=UTF-8''{exact}", if inline { "inline" } else { "attachment" })
}

/// Why streaming a request field into the blob store stopped.
#[derive(Debug)]
pub enum StreamError {
    /// More than the limit arrived.
    TooLarge,
    /// The client's body broke off.
    Interrupted(String),
    Store(anyhow::Error),
}

/// Stream one multipart field to `key`, at most `limit` bytes; returns its size.
/// On any error nothing is left under `key`.
pub async fn stream_field(
    blobs: &Arc<dyn BlobStore>,
    mut field: axum::extract::multipart::Field<'_>,
    key: &str,
    limit: u64,
) -> Result<u64, StreamError> {
    // the field borrows the request; the store wants a 'static stream
    let (tx, rx) = tokio::sync::mpsc::channel::<std::io::Result<Bytes>>(4);
    let store = blobs.clone();
    let k = key.to_string();
    let writer = tokio::spawn(async move { store.put_stream(&k, tokio_stream::wrappers::ReceiverStream::new(rx).boxed()).await });
    let mut size: u64 = 0;
    let mut problem = None;
    loop {
        match field.chunk().await {
            Ok(Some(chunk)) => {
                size += chunk.len() as u64;
                if size > limit {
                    problem = Some(StreamError::TooLarge);
                    let _ = tx.send(Err(std::io::Error::other("too large"))).await;
                    break;
                }
                if tx.send(Ok(chunk)).await.is_err() {
                    break;
                }
            }
            Ok(None) => break,
            Err(e) => {
                problem = Some(StreamError::Interrupted(e.to_string()));
                let _ = tx.send(Err(std::io::Error::other("upload interrupted"))).await;
                break;
            }
        }
    }
    drop(tx);
    let written = writer.await.map_err(|e| StreamError::Store(e.into())).and_then(|r| r.map_err(StreamError::Store));
    match (problem, written) {
        (None, Ok(n)) => Ok(n),
        (Some(p), _) | (None, Err(p)) => {
            let _ = blobs.delete(key).await;
            Err(p)
        }
    }
}
