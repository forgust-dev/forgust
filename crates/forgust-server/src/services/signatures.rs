//! Signed commits: the OpenPGP or SSH signature in a commit's
//! `gpgsig` header, checked against the keys users registered (GPG keys, and
//! their SSH keys, which sign as well as authenticate, as in GitLab).
//!
//! A commit is **verified** when the signature is good, the key belongs to a
//! user, and the committer email is that user's confirmed address (and, for
//! OpenPGP, one of the key's own user IDs).

use forgust_db::users::User;
use serde::Serialize;
use utoipa::ToSchema;

use crate::state::AppState;

/// What a commit's signature says.
#[derive(Debug, Clone, Serialize, ToSchema, PartialEq)]
pub struct Verification {
    /// `PGP`, `SSH` or `X509`.
    pub signature_type: String,
    /// `verified`; `unverified` (good signature, but the committer email is not
    /// the key owner's confirmed address); `unknown_key` (no user registered
    /// the key); `bad_signature`; `unsupported` (X.509).
    pub verification_status: String,
    /// The key's fingerprint (OpenPGP: primary key, hex; SSH: `SHA256:...`).
    pub key_fingerprint: Option<String>,
    /// The key owner's username, when the key is registered.
    pub signer: Option<String>,
}

impl Verification {
    pub fn verified(&self) -> bool {
        self.verification_status == "verified"
    }
}

/// A commit object split for verification.
struct Signed {
    kind: &'static str,
    signature: String,
    /// The commit as signed: without its `gpgsig` header.
    payload: Vec<u8>,
    committer_email: String,
}

/// Find the signature header (and the committer) in a raw commit object.
fn split(raw: &[u8]) -> Option<Signed> {
    let header_end = raw.windows(2).position(|w| w == b"\n\n").map(|i| i + 1).unwrap_or(raw.len());
    let (headers, rest) = raw.split_at(header_end);
    let mut payload = Vec::with_capacity(raw.len());
    let mut signature: Option<String> = None;
    let mut committer_email = String::new();
    let mut in_sig = false;
    for line in headers.split_inclusive(|b| *b == b'\n') {
        if in_sig && line.first() == Some(&b' ') {
            let s = signature.as_mut().expect("in a signature");
            s.push_str(&String::from_utf8_lossy(&line[1..]));
            continue;
        }
        in_sig = false;
        if let Some(sig) = line.strip_prefix(b"gpgsig ") {
            signature = Some(String::from_utf8_lossy(sig).into_owned());
            in_sig = true;
            continue;
        }
        if let Some(c) = line.strip_prefix(b"committer ") {
            let c = String::from_utf8_lossy(c);
            if let (Some(a), Some(b)) = (c.find('<'), c.find('>')) {
                committer_email = c[a + 1..b].trim().to_string();
            }
        }
        payload.extend_from_slice(line);
    }
    payload.extend_from_slice(rest);
    let signature = signature?;
    let kind = if signature.contains("BEGIN PGP SIGNATURE") {
        "PGP"
    } else if signature.contains("BEGIN SSH SIGNATURE") {
        "SSH"
    } else {
        "X509"
    };
    Some(Signed { kind, signature, payload, committer_email })
}

/// The confirmed address of `user` is the committer's.
fn owns_email(user: &User, email: &str) -> bool {
    user.email_verified() && user.email.eq_ignore_ascii_case(email)
}

/// Check a raw commit's signature. None: the commit is not signed.
pub async fn verify(state: &AppState, raw: &[u8]) -> Option<Verification> {
    let s = split(raw)?;
    let mut v = Verification {
        signature_type: s.kind.to_string(),
        verification_status: "bad_signature".into(),
        key_fingerprint: None,
        signer: None,
    };
    match s.kind {
        "PGP" => verify_pgp(state, &s, &mut v).await,
        "SSH" => verify_ssh(state, &s, &mut v).await,
        _ => v.verification_status = "unsupported".into(),
    }
    Some(v)
}

async fn verify_pgp(state: &AppState, s: &Signed, v: &mut Verification) {
    use pgp::composed::{Deserializable, DetachedSignature, SignedPublicKey};
    use pgp::types::KeyDetails;
    let Ok((sig, _)) = DetachedSignature::from_armor_single(std::io::Cursor::new(s.signature.as_bytes())) else { return };
    let mut issuers: Vec<String> = sig.signature.issuer_key_id().iter().map(|k| hex::encode_upper(k.as_ref())).collect();
    for fp in sig.signature.issuer_fingerprint() {
        let b = fp.as_bytes();
        if b.len() >= 8 {
            issuers.push(hex::encode_upper(&b[b.len() - 8..]));
        }
    }
    v.verification_status = "unknown_key".into();
    for issuer in issuers {
        let Ok(Some(row)) = forgust_db::gpg_keys::by_key_id(&state.db, &issuer).await else { continue };
        let Ok((key, _)) = SignedPublicKey::from_armor_single(std::io::Cursor::new(row.armored.as_bytes())) else { continue };
        v.key_fingerprint = Some(row.fingerprint.clone());
        let good = if hex::encode_upper(key.primary_key.legacy_key_id().as_ref()) == issuer {
            sig.verify(&key.primary_key, &s.payload).is_ok()
        } else {
            key.public_subkeys
                .iter()
                .filter(|k| hex::encode_upper(k.key.legacy_key_id().as_ref()) == issuer)
                .any(|k| sig.verify(&k.key, &s.payload).is_ok())
        };
        if !good {
            v.verification_status = "bad_signature".into();
            return;
        }
        let user = forgust_db::users::find(&state.db, row.user_id).await.ok().flatten();
        v.signer = user.as_ref().map(|u| u.username.clone());
        let email = s.committer_email.to_ascii_lowercase();
        v.verification_status =
            if user.is_some_and(|u| owns_email(&u, &email)) && row.emails.contains(&email) { "verified" } else { "unverified" }.into();
        return;
    }
}

async fn verify_ssh(state: &AppState, s: &Signed, v: &mut Verification) {
    use russh::keys::ssh_key::{HashAlg, PublicKey, SshSig};
    let Ok(sig) = SshSig::from_pem(s.signature.trim()) else { return };
    let key = PublicKey::from(sig.public_key().clone());
    let fp = key.fingerprint(HashAlg::Sha256).to_string();
    v.key_fingerprint = Some(fp.clone());
    if key.verify("git", &s.payload, &sig).is_err() {
        return;
    }
    let row = forgust_db::ssh_keys::find_by_fingerprint(&state.db, &fp).await.ok().flatten().filter(|k| k.kind == "user");
    let Some(user_id) = row.and_then(|k| k.user_id) else {
        v.verification_status = "unknown_key".into();
        return;
    };
    let user = forgust_db::users::find(&state.db, user_id).await.ok().flatten();
    v.signer = user.as_ref().map(|u| u.username.clone());
    v.verification_status = if user.is_some_and(|u| owns_email(&u, &s.committer_email)) { "verified" } else { "unverified" }.into();
}

/// An uploaded OpenPGP public key: (fingerprint, key ids, emails), or why not.
pub fn parse_gpg_key(armored: &str) -> Result<(String, Vec<String>, Vec<String>), String> {
    use pgp::composed::{Deserializable, SignedPublicKey};
    use pgp::types::KeyDetails;
    if armored.contains("PRIVATE KEY BLOCK") {
        return Err("that is a private key: paste the public key (gpg --armor --export <id>)".into());
    }
    let (key, _) = SignedPublicKey::from_armor_single(std::io::Cursor::new(armored.trim().as_bytes()))
        .map_err(|_| "not an armored OpenPGP public key (gpg --armor --export <id>)".to_string())?;
    key.verify_bindings().map_err(|_| "the key's self-signatures do not verify".to_string())?;
    let fingerprint = format!("{:X}", key.primary_key.fingerprint());
    let mut ids = vec![hex::encode_upper(key.primary_key.legacy_key_id().as_ref())];
    ids.extend(key.public_subkeys.iter().map(|k| hex::encode_upper(k.key.legacy_key_id().as_ref())));
    let emails: Vec<String> = key
        .details
        .users
        .iter()
        .filter_map(|u| {
            let id = String::from_utf8_lossy(u.id.id()).into_owned();
            let (a, b) = (id.find('<')?, id.rfind('>')?);
            (a < b).then(|| id[a + 1..b].trim().to_ascii_lowercase())
        })
        .collect();
    Ok((fingerprint, ids, emails))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_signature_header_is_cut_out_of_the_payload() {
        let raw = b"tree 4b825dc642cb6eb9a060e54bf8d69288fbee4904\nauthor A <a@x> 1 +0000\ncommitter C <c@x.org> 1 +0000\ngpgsig -----BEGIN SSH SIGNATURE-----\n U1NI\n -----END SSH SIGNATURE-----\n\nmsg\n\nbody\n";
        let s = split(raw).unwrap();
        assert_eq!(s.kind, "SSH");
        assert_eq!(s.committer_email, "c@x.org");
        assert_eq!(s.signature, "-----BEGIN SSH SIGNATURE-----\nU1NI\n-----END SSH SIGNATURE-----\n");
        assert_eq!(
            String::from_utf8(s.payload).unwrap(),
            "tree 4b825dc642cb6eb9a060e54bf8d69288fbee4904\nauthor A <a@x> 1 +0000\ncommitter C <c@x.org> 1 +0000\n\nmsg\n\nbody\n"
        );
        assert!(split(b"tree x\ncommitter C <c@x> 1 +0000\n\nunsigned\n").is_none());
    }
}
