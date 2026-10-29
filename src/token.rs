//! Secrets behind signing links. A link's token is the signer row's id and a 128-bit
//! HMAC-SHA256 tag over the tenant, the request, the signer and the signer's email, under the
//! tenant's key — the `signing_key` secret field of Sign Settings (sealed by the kernel, never in a
//! document, export or audit trail). Nothing per link is stored: correcting a signer's email or
//! rotating the key retires their links, and tags are compared in constant time. The same key
//! salts the IP hashes kept in the audit trail. Access codes are Argon2id hashes.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ferrum_kernel::{Ctx, Doc, Error, Kernel, Result, Tx};
use hmac::{Hmac, Mac};
use serde_json::json;
use sha2::Sha256;
use uuid::Uuid;

const LINK: &[u8] = b"ferrum-sign/link/v1";
const IP: &[u8] = b"ferrum-sign/ip/v1";
/// Length of a token: 32 bytes (16 of row id, 16 of tag) in URL-safe base64 without padding.
pub const TOKEN_LEN: usize = 43;

fn link_mac(key: &[u8], tenant: Uuid, request: Uuid, signer: Uuid, email: &str) -> Hmac<Sha256> {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC accepts any key length");
    for part in [LINK, tenant.as_bytes(), request.as_bytes(), signer.as_bytes(), email.trim().to_lowercase().as_bytes()] {
        mac.update(&(part.len() as u32).to_be_bytes());
        mac.update(part);
    }
    mac
}

/// The token of a signer's link.
pub fn link_token(key: &[u8], tenant: Uuid, request: Uuid, signer: Uuid, email: &str) -> String {
    let tag = link_mac(key, tenant, request, signer, email).finalize().into_bytes();
    let mut raw = Vec::with_capacity(32);
    raw.extend_from_slice(signer.as_bytes());
    raw.extend_from_slice(&tag[..16]);
    URL_SAFE_NO_PAD.encode(raw)
}

/// The signer row a well-formed token names, and its tag. Anything else is not looked up.
pub fn parse(token: &str) -> Option<(Uuid, Vec<u8>)> {
    if token.len() != TOKEN_LEN || !token.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_') {
        return None;
    }
    let raw = URL_SAFE_NO_PAD.decode(token).ok()?;
    if raw.len() != 32 {
        return None;
    }
    Some((Uuid::from_slice(&raw[..16]).ok()?, raw[16..].to_vec()))
}

/// Whether `tag` is the link tag of this signer (constant time).
pub fn link_valid(key: &[u8], tenant: Uuid, request: Uuid, signer: Uuid, email: &str, tag: &[u8]) -> bool {
    tag.len() == 16 && link_mac(key, tenant, request, signer, email).verify_truncated_left(tag).is_ok()
}

/// The audit trail's salted hash of a client address (128 bits, hex): equal addresses compare
/// equal within the tenant, but the address itself is not kept.
pub fn ip_hash(key: &[u8], ip: &str) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(IP);
    mac.update(ip.trim().as_bytes());
    hex::encode(&mac.finalize().into_bytes()[..16])
}

pub async fn hash_code(code: &str) -> Result<String> {
    ferrum_kernel::auth::hash_password(code.to_string()).await
}

pub async fn code_matches(code: &str, hash: &str) -> bool {
    ferrum_kernel::auth::verify_password(code.to_string(), hash.to_string()).await
}

fn fresh_key() -> String {
    hex::encode(rand::random::<[u8; 32]>())
}

async fn read_key(k: &Kernel, tx: &mut Tx, tenant: Uuid) -> Result<Option<Vec<u8>>> {
    let id: Option<Uuid> = sqlx::query_scalar("select id from e_sign_settings where tenant_id = $1 limit 1").bind(tenant).fetch_optional(&mut **tx).await?;
    let Some(id) = id else { return Ok(None) };
    match k.secret(tx, tenant, "SignSettings", id, "signing_key").await? {
        Some(hexed) => hex::decode(hexed.trim()).map(Some).map_err(|_| Error::internal("the signing key in Sign Settings is not hex")),
        None => Ok(None),
    }
}

/// The tenant's signing key. On first use it is created in a transaction of its own (committed
/// at once, so concurrent first uses agree on one key) and read back through `tx`.
pub async fn signing_key(k: &Kernel, tx: &mut Tx, tenant: Uuid) -> Result<Vec<u8>> {
    if let Some(key) = read_key(k, tx, tenant).await? {
        return Ok(key);
    }
    create_key(k, tenant).await?;
    read_key(k, tx, tenant).await?.ok_or_else(|| Error::Retryable("the signing key is being created".into()))
}

async fn create_key(k: &Kernel, tenant: Uuid) -> Result<()> {
    let sys = Ctx::system(tenant, k.registry.schema(tenant).await?);
    for _ in 0..3 {
        let mut tx = k.db.tenant_tx(tenant).await?;
        let id: Option<Uuid> =
            sqlx::query_scalar("select id from e_sign_settings where tenant_id = $1 limit 1 for update").bind(tenant).fetch_optional(&mut *tx).await?;
        if let Some(id) = id
            && k.secret(&mut tx, tenant, "SignSettings", id, "signing_key").await?.is_some()
        {
            return tx.commit().await.map_err(Into::into);
        }
        let mut values = Doc::new();
        values.insert("signing_key".into(), json!(fresh_key()));
        let written = match id {
            Some(id) => k.update_tx(&sys, &mut tx, "SignSettings", id, values, None).await,
            None => k.insert_tx(&sys, &mut tx, "SignSettings", values).await,
        };
        match written {
            Ok(_) => return tx.commit().await.map_err(Into::into),
            // Created at the same moment elsewhere: use theirs.
            Err(Error::Conflict(_)) => continue,
            Err(e) => return Err(e),
        }
    }
    Err(Error::Retryable("the signing key is being created".into()))
}

/// Replaces the key: every outstanding link stops working.
pub async fn rotate_key(k: &Kernel, ctx: &Ctx, tx: &mut Tx, settings: Uuid) -> Result<()> {
    let mut values = Doc::new();
    values.insert("signing_key".into(), json!(fresh_key()));
    k.update_tx(&ferrum_domain::util::elevated(ctx), tx, "SignSettings", settings, values, None).await.map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_bind_tenant_request_signer_and_email() {
        let (key, other_key) = ([7u8; 32], [8u8; 32]);
        let (t, r, s) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
        let token = link_token(&key, t, r, s, "Jane@Client.test ");
        assert_eq!(token.len(), TOKEN_LEN);
        let (signer, tag) = parse(&token).unwrap();
        assert_eq!(signer, s);
        assert!(link_valid(&key, t, r, s, "jane@client.test", &tag), "emails compare case-insensitively");
        assert!(!link_valid(&other_key, t, r, s, "jane@client.test", &tag), "another key");
        assert!(!link_valid(&key, Uuid::now_v7(), r, s, "jane@client.test", &tag), "another tenant");
        assert!(!link_valid(&key, t, Uuid::now_v7(), s, "jane@client.test", &tag), "another request");
        assert!(!link_valid(&key, t, r, Uuid::now_v7(), "jane@client.test", &tag), "another signer");
        assert!(!link_valid(&key, t, r, s, "jane@other.test", &tag), "a corrected email");
        assert!(!link_valid(&key, t, r, s, "jane@client.test", &tag[..15]), "a shortened tag");
        let mut flipped = token.clone().into_bytes();
        flipped[40] = if flipped[40] == b'A' { b'B' } else { b'A' };
        let (_, bad) = parse(std::str::from_utf8(&flipped).unwrap()).unwrap();
        assert!(!link_valid(&key, t, r, s, "jane@client.test", &bad), "a tampered tag");
        assert!(parse("short").is_none() && parse(&format!("{}!", &token[..42])).is_none() && parse(&format!("{token}A")).is_none());
    }

    #[test]
    fn ip_hashes_are_keyed() {
        let a = ip_hash(&[1u8; 32], "203.0.113.7");
        assert_eq!(a.len(), 32);
        assert_eq!(a, ip_hash(&[1u8; 32], " 203.0.113.7"));
        assert_ne!(a, ip_hash(&[2u8; 32], "203.0.113.7"));
        assert_ne!(a, ip_hash(&[1u8; 32], "203.0.113.8"));
    }
}
