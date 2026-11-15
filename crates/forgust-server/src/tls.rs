//! The certificate authorities of `security.ca_file`: trusted, on top of the
//! usual ones, by every TLS connection the server opens.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use anyhow::Context;
use base64::Engine;
use rustls_pki_types::CertificateDer;
use rustls_pki_types::pem::PemObject;

use crate::config::Config;

struct Extra {
    certs: Vec<CertificateDer<'static>>,
    bundle: PathBuf,
}

static EXTRA: OnceLock<Option<Extra>> = OnceLock::new();
static BUNDLE: OnceLock<PathBuf> = OnceLock::new();

/// Read `security.ca_file`, once per process (later calls keep the first). Before
/// any client is built: one built earlier would not know these authorities.
pub fn init(cfg: &Config) -> anyhow::Result<()> {
    if EXTRA.get().is_some() {
        return Ok(());
    }
    let file = &cfg.security.ca_file;
    if file.as_os_str().is_empty() {
        let _ = EXTRA.set(None);
        return Ok(());
    }
    let certs = parse(&std::fs::read(file).with_context(|| format!("security.ca_file: reading {}", file.display()))?)
        .with_context(|| format!("security.ca_file: {}", file.display()))?;
    // a certificate rustls refuses would otherwise fail every request, later
    with(reqwest::Client::builder(), &certs).build().with_context(|| format!("security.ca_file: {}", file.display()))?;
    // absolute: git reads it from inside a repository
    let bundle = std::path::absolute(cfg.server.data_dir.join("run").join("ca-bundle.pem"))?;
    let _ = EXTRA.set(Some(Extra { certs, bundle }));
    Ok(())
}

fn parse(pem: &[u8]) -> anyhow::Result<Vec<CertificateDer<'static>>> {
    let certs = CertificateDer::pem_slice_iter(pem).collect::<Result<Vec<_>, _>>().map_err(|e| anyhow::anyhow!("not PEM: {e}"))?;
    anyhow::ensure!(!certs.is_empty(), "no certificate (PEM) in it");
    Ok(certs)
}

/// The extra authorities; none without `security.ca_file`.
pub fn certs() -> &'static [CertificateDer<'static>] {
    EXTRA.get().and_then(Option::as_ref).map(|e| e.certs.as_slice()).unwrap_or_default()
}

fn with(builder: reqwest::ClientBuilder, certs: &[CertificateDer<'_>]) -> reqwest::ClientBuilder {
    builder.tls_certs_merge(certs.iter().filter_map(|c| reqwest::Certificate::from_der(c).ok()))
}

/// What every HTTP client of the server starts from, in place of
/// `reqwest::Client::builder()`.
pub fn http_client() -> reqwest::ClientBuilder {
    with(reqwest::Client::builder(), certs())
}

/// The extra authorities as PEM; `None` without `security.ca_file`.
pub fn pem() -> Option<Vec<u8>> {
    let certs = certs();
    (!certs.is_empty()).then(|| to_pem(certs).into_bytes())
}

/// A file holding the system's authorities and the extra ones, for programs that
/// take one file in place of their defaults (git, pg_dump); `None` without
/// `security.ca_file`.
pub fn bundle_file() -> anyhow::Result<Option<&'static Path>> {
    let Some(extra) = EXTRA.get().and_then(Option::as_ref) else { return Ok(None) };
    if let Some(path) = BUNDLE.get() {
        return Ok(Some(path));
    }
    let mut pem = to_pem(&rustls_native_certs::load_native_certs().certs);
    pem.push_str(&to_pem(&extra.certs));
    let dir = extra.bundle.parent().context("security.ca_file: no directory for the bundle")?;
    std::fs::create_dir_all(dir)?;
    // written aside then renamed: another process of this instance may be reading it
    let tmp = extra.bundle.with_extension(format!("pem.{}", std::process::id()));
    std::fs::write(&tmp, pem)?;
    std::fs::rename(&tmp, &extra.bundle)?;
    Ok(Some(BUNDLE.get_or_init(|| extra.bundle.clone())))
}

fn to_pem(certs: &[CertificateDer<'_>]) -> String {
    let mut out = String::new();
    for cert in certs {
        out.push_str("-----BEGIN CERTIFICATE-----\n");
        let b64 = base64::engine::general_purpose::STANDARD.encode(cert);
        for line in b64.as_bytes().chunks(64) {
            out.push_str(std::str::from_utf8(line).unwrap_or_default());
            out.push('\n');
        }
        out.push_str("-----END CERTIFICATE-----\n");
    }
    out
}

/// A self-signed authority, for tests.
#[cfg(test)]
pub(crate) fn test_ca() -> Vec<CertificateDer<'static>> {
    parse(
        b"-----BEGIN CERTIFICATE-----\n\
MIIBizCCATGgAwIBAgIUbJK4hqgtSFpElhjOtgBG88KCa4wwCgYIKoZIzj0EAwIw\n\
GjEYMBYGA1UEAwwPRm9yZ3VzdCB0ZXN0IENBMCAXDTI2MTExNTEzNTkxNloYDzIx\n\
MjYxMDIyMTM1OTE2WjAaMRgwFgYDVQQDDA9Gb3JndXN0IHRlc3QgQ0EwWTATBgcq\n\
hkjOPQIBBggqhkjOPQMBBwNCAATkNJdZ1mM+xBluvr4Dm6UioU2a3NoOnW7Kb4F2\n\
kwN264vw90XRJ5kd+xZAL/IEdDY88uGi8tQfLF2fP633+0eFo1MwUTAdBgNVHQ4E\n\
FgQUQGM1amv0C0NG13w2MmlufVuhLcAwHwYDVR0jBBgwFoAUQGM1amv0C0NG13w2\n\
MmlufVuhLcAwDwYDVR0TAQH/BAUwAwEB/zAKBggqhkjOPQQDAgNIADBFAiAJQ2Jl\n\
Ms2v7WUFZhUGeXZLNk219frEm8HpefVAYJF5hAIhAOVTbOY4w6eOUjjNPESxyMYS\n\
1GYybHk8dI4Z3h6XZaT8\n\
-----END CERTIFICATE-----\n",
    )
    .unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pem_round_trip() {
        let certs = vec![CertificateDer::from(vec![1u8; 100]), CertificateDer::from(vec![2u8; 7])];
        assert_eq!(parse(to_pem(&certs).as_bytes()).unwrap(), certs);
    }

    #[test]
    fn clients_accept_the_authorities() {
        assert!(with(reqwest::Client::builder(), &test_ca()).build().is_ok());
        assert!(with(reqwest::Client::builder(), &[CertificateDer::from(vec![1u8; 100])]).build().is_err());
    }

    #[test]
    fn a_file_without_certificates_is_refused() {
        assert!(parse(b"").is_err());
        assert!(parse(b"-----BEGIN PRIVATE KEY-----\nAAAA\n-----END PRIVATE KEY-----\n").is_err());
    }
}
