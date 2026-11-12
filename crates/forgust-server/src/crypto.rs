//! Tokens, password hashing and encryption at rest.

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use argon2::Argon2;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use rand::{Rng, RngCore};
use sha2::{Digest, Sha256};

/// Token prefixes by type (ADR-0015), so leaked tokens are easy to scan for.
pub mod prefix {
    pub const PERSONAL: &str = "fgpat-";
    pub const DEPLOY: &str = "fgdt-";
    pub const JOB: &str = "fgcbt-";
    pub const LFS_SSH: &str = "fglfs-";
    pub const SESSION: &str = "fgs-";
}

const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";

pub fn random_token(prefix: &str) -> String {
    let mut rng = rand::thread_rng();
    let body: String = (0..32).map(|_| ALPHABET[rng.gen_range(0..ALPHABET.len())] as char).collect();
    format!("{prefix}{body}")
}

pub fn random_urlsafe(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    rand::thread_rng().fill_bytes(&mut buf);
    URL_SAFE_NO_PAD.encode(buf)
}

/// Tokens are high-entropy, so a plain SHA-256 is enough for lookups.
pub fn token_hash(token: &str) -> Vec<u8> {
    Sha256::digest(token.as_bytes()).to_vec()
}

/// Argon2 runs (checks and new hashes) at once: no more than there are cores.
fn password_gate() -> &'static tokio::sync::Semaphore {
    static GATE: std::sync::OnceLock<tokio::sync::Semaphore> = std::sync::OnceLock::new();
    GATE.get_or_init(|| tokio::sync::Semaphore::new(std::thread::available_parallelism().map_or(2, |n| n.get())))
}

/// [`hash_password`] for request paths: off the async workers, behind the
/// same gate as sign-in checks.
pub async fn hash_password_gated(password: &str) -> anyhow::Result<String> {
    let _permit = password_gate().acquire().await?;
    let password = password.to_string();
    tokio::task::spawn_blocking(move || hash_password(&password)).await?
}

pub fn hash_password(password: &str) -> anyhow::Result<String> {
    let salt = SaltString::generate(&mut argon2::password_hash::rand_core::OsRng);
    Ok(Argon2::default().hash_password(password.as_bytes(), &salt).map_err(|e| anyhow::anyhow!("{e}"))?.to_string())
}

pub fn verify_password(password: &str, hash: &str) -> bool {
    PasswordHash::new(hash).map(|h| Argon2::default().verify_password(password.as_bytes(), &h).is_ok()).unwrap_or(false)
}

/// A hash to compare against when the user does not exist, so timing does not
/// reveal which usernames are taken.
pub fn dummy_verify(password: &str) {
    static DUMMY: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    let h = DUMMY.get_or_init(|| hash_password("dummy-password").unwrap_or_default());
    let _ = verify_password(password, h);
}

/// A sign-in's password check: off the async workers, and no more at once
/// than there are cores (each costs ~19 MB and real CPU time). `None`
/// checks against a dummy hash, so a missing account takes as long.
pub async fn check_password(password: &str, hash: Option<String>) -> bool {
    let Ok(_permit) = password_gate().acquire().await else { return false };
    let password = password.to_string();
    tokio::task::spawn_blocking(move || match hash {
        Some(h) => verify_password(&password, &h),
        None => {
            dummy_verify(&password);
            false
        }
    })
    .await
    .unwrap_or(false)
}

#[derive(Clone)]
pub struct SecretBox {
    cipher: Aes256Gcm,
    /// For signed links (unsubscribe): derived from the secret key, never the key itself.
    mac_key: [u8; 32],
}

impl std::fmt::Debug for SecretBox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SecretBox(..)")
    }
}

impl SecretBox {
    pub fn from_base64(key: &str) -> anyhow::Result<Self> {
        let raw = STANDARD.decode(key.trim())?;
        anyhow::ensure!(raw.len() == 32, "security.secret_key must be 32 bytes (base64)");
        let mac_key: [u8; 32] = Sha256::new().chain_update(b"forgust signed links v1\0").chain_update(&raw).finalize().into();
        Ok(SecretBox { cipher: Aes256Gcm::new_from_slice(&raw).map_err(|e| anyhow::anyhow!("{e}"))?, mac_key })
    }

    /// A signature of `msg` for a link (HMAC-SHA256, URL-safe).
    pub fn sign(&self, msg: &str) -> String {
        use hmac::{Hmac, Mac};
        let mut m = <Hmac<Sha256> as Mac>::new_from_slice(&self.mac_key).expect("any key length");
        m.update(msg.as_bytes());
        URL_SAFE_NO_PAD.encode(m.finalize().into_bytes())
    }

    pub fn verify(&self, msg: &str, sig: &str) -> bool {
        eq_ct(&self.sign(msg), sig)
    }

    pub fn generate_key() -> String {
        let mut k = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut k);
        STANDARD.encode(k)
    }

    /// AES-256-GCM; output is base64(nonce || ciphertext).
    pub fn encrypt(&self, plaintext: &[u8]) -> anyhow::Result<String> {
        let mut nonce = [0u8; 12];
        rand::thread_rng().fill_bytes(&mut nonce);
        let ct = self.cipher.encrypt(Nonce::from_slice(&nonce), plaintext).map_err(|e| anyhow::anyhow!("{e}"))?;
        let mut out = nonce.to_vec();
        out.extend(ct);
        Ok(STANDARD.encode(out))
    }

    pub fn decrypt(&self, sealed: &str) -> anyhow::Result<Vec<u8>> {
        let raw = STANDARD.decode(sealed)?;
        anyhow::ensure!(raw.len() > 12, "sealed value too short");
        let (nonce, ct) = raw.split_at(12);
        self.cipher.decrypt(Nonce::from_slice(nonce), ct).map_err(|e| anyhow::anyhow!("{e}"))
    }

    pub fn decrypt_str(&self, sealed: &str) -> anyhow::Result<String> {
        Ok(String::from_utf8(self.decrypt(sealed)?)?)
    }

    pub fn encrypt_json<T: serde::Serialize>(&self, value: &T) -> anyhow::Result<String> {
        self.encrypt(&serde_json::to_vec(value)?)
    }

    pub fn decrypt_json<T: serde::de::DeserializeOwned>(&self, sealed: &str) -> anyhow::Result<T> {
        Ok(serde_json::from_slice(&self.decrypt(sealed)?)?)
    }
}

/// Constant-time string comparison.
pub fn eq_ct(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_have_prefix_and_entropy() {
        let a = random_token(prefix::PERSONAL);
        let b = random_token(prefix::PERSONAL);
        assert!(a.starts_with("fgpat-"));
        assert_eq!(a.len(), 6 + 32);
        assert_ne!(a, b);
        assert_eq!(token_hash(&a), token_hash(&a));
    }

    #[test]
    fn password_roundtrip() {
        let h = hash_password("correct horse").unwrap();
        assert!(verify_password("correct horse", &h));
        assert!(!verify_password("wrong", &h));
    }

    #[test]
    fn secretbox_roundtrip() {
        let b = SecretBox::from_base64(&SecretBox::generate_key()).unwrap();
        let sealed = b.encrypt(b"JBSWY3DPEHPK3PXP").unwrap();
        assert_eq!(b.decrypt(&sealed).unwrap(), b"JBSWY3DPEHPK3PXP");
        assert!(SecretBox::from_base64(&SecretBox::generate_key()).unwrap().decrypt(&sealed).is_err());
    }
}
