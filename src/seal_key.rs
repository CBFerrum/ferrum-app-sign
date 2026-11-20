//! The workspace's seal key: the private key and certificate that digitally seal every finished document (`pades.rs`).
//! One is made at the first seal (a P-256 key and a self-signed certificate for it, in one transaction under a lock on the
//! settings row, so two seals at once agree on one) and kept in the vault, as Sign Settings' secret `seal_key`; a key and
//! certificate from a certificate authority replace it. Keys are P-256 or P-384 only: an RSA key is refused (the `rsa`
//! crate has an open timing advisory, RUSTSEC-2023-0071, so Sign does not sign with it). Nothing here names a vendor.

use crate::request::obj;
use ferrum_kernel::{Ctx, Doc, Error, Kernel, Result, Tx};
use serde_json::json;
use sha2::{Digest, Sha256, Sha384};
use std::str::FromStr;
use uuid::Uuid;
use x509_cert::Certificate;
use x509_cert::der::asn1::{BitString, ObjectIdentifier, OctetString};
use x509_cert::der::pem::LineEnding;
use x509_cert::der::{Decode, Encode, EncodePem};
use x509_cert::ext::Extension;
use x509_cert::ext::pkix::{KeyUsage, KeyUsages};
use x509_cert::name::Name;
use x509_cert::serial_number::SerialNumber;
use x509_cert::spki::{AlgorithmIdentifierOwned, SubjectPublicKeyInfoOwned};
use x509_cert::time::Validity;

use p256::ecdsa::signature::Signer;
use p256::pkcs8::{DecodePrivateKey, EncodePrivateKey, EncodePublicKey};

/// The curve the key is on, and with it the hash its signatures use.
#[derive(Clone)]
pub enum Key {
    P256(p256::ecdsa::SigningKey),
    P384(p384::ecdsa::SigningKey),
}

/// A hash a signature uses.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Hash {
    S256,
    S384,
}

impl Hash {
    pub fn digest(self, parts: &[&[u8]]) -> Vec<u8> {
        match self {
            Hash::S256 => {
                let mut h = Sha256::new();
                parts.iter().for_each(|p| h.update(p));
                h.finalize().to_vec()
            }
            Hash::S384 => {
                let mut h = Sha384::new();
                parts.iter().for_each(|p| h.update(p));
                h.finalize().to_vec()
            }
        }
    }

    pub fn oid(self) -> ObjectIdentifier {
        match self {
            Hash::S256 => ObjectIdentifier::new_unwrap("2.16.840.1.101.3.4.2.1"),
            Hash::S384 => ObjectIdentifier::new_unwrap("2.16.840.1.101.3.4.2.2"),
        }
    }
}

impl Key {
    /// A PKCS#8 PEM private key on P-256 or P-384; anything else (an RSA key, a broken file) is refused with why.
    pub fn parse(pem: &str) -> std::result::Result<Key, String> {
        if let Ok(k) = p256::ecdsa::SigningKey::from_pkcs8_pem(pem) {
            return Ok(Key::P256(k));
        }
        if let Ok(k) = p384::ecdsa::SigningKey::from_pkcs8_pem(pem) {
            return Ok(Key::P384(k));
        }
        Err("the seal key must be a PKCS#8 PEM private key on curve P-256 or P-384; RSA keys are not accepted".into())
    }

    pub fn pem(&self) -> std::result::Result<String, String> {
        let pem = match self {
            Key::P256(k) => k.to_pkcs8_pem(LineEnding::LF).map(|z| z.to_string()),
            Key::P384(k) => k.to_pkcs8_pem(LineEnding::LF).map(|z| z.to_string()),
        };
        pem.map_err(|e| format!("the seal key cannot be written ({e})"))
    }

    pub fn hash(&self) -> Hash {
        match self {
            Key::P256(_) => Hash::S256,
            Key::P384(_) => Hash::S384,
        }
    }

    /// The OID of the signature algorithm (ECDSA with the curve's hash).
    pub fn algorithm(&self) -> ObjectIdentifier {
        ObjectIdentifier::new_unwrap(match self {
            Key::P256(_) => "1.2.840.10045.4.3.2",
            Key::P384(_) => "1.2.840.10045.4.3.3",
        })
    }

    pub fn spki(&self) -> std::result::Result<SubjectPublicKeyInfoOwned, String> {
        let der = match self {
            Key::P256(k) => k.verifying_key().to_public_key_der().map(|d| d.as_bytes().to_vec()),
            Key::P384(k) => k.verifying_key().to_public_key_der().map(|d| d.as_bytes().to_vec()),
        }
        .map_err(|e| format!("the seal's public key cannot be read ({e})"))?;
        SubjectPublicKeyInfoOwned::from_der(&der).map_err(|e| format!("the seal's public key cannot be read ({e})"))
    }

    /// An ECDSA signature over `message` (hashed with the curve's hash), DER-encoded.
    pub fn sign(&self, message: &[u8]) -> Vec<u8> {
        match self {
            Key::P256(k) => {
                let s: p256::ecdsa::DerSignature = k.sign(message);
                s.as_bytes().to_vec()
            }
            Key::P384(k) => {
                let s: p384::ecdsa::DerSignature = k.sign(message);
                s.as_bytes().to_vec()
            }
        }
    }
}

/// The seal: its key and certificate chain (DER, the seal's own certificate first).
#[derive(Clone)]
pub struct SealKey {
    pub key: Key,
    pub chain: Vec<Certificate>,
}

impl SealKey {
    pub fn certificate(&self) -> &Certificate {
        &self.chain[0]
    }

    /// SHA-256 of the seal's own certificate, as the PDF's signing-certificate attribute and the certificate page say.
    pub fn certificate_sha256(&self) -> Result<String> {
        Ok(hex::encode(Sha256::digest(self.certificate().to_der().map_err(|e| Error::internal(format!("the seal certificate: {e}")))?)))
    }

    /// Who the seal says it is, for people: the certificate's subject.
    pub fn subject(&self) -> String {
        self.certificate().tbs_certificate.subject.to_string()
    }
}

/// A self-signed certificate for `key`, good ten years, for signing and non-repudiation only.
pub(crate) fn self_signed(key: &Key, workspace: &str) -> std::result::Result<Certificate, String> {
    let label: String = workspace.chars().filter(|c| c.is_alphanumeric() || *c == ' ').take(60).collect();
    let name = Name::from_str(&format!("CN={} Sign seal", if label.trim().is_empty() { "Workspace" } else { label.trim() })).map_err(|e| format!("the certificate's name: {e}"))?;
    let alg = AlgorithmIdentifierOwned { oid: key.algorithm(), parameters: None };
    let usage = KeyUsage(KeyUsages::DigitalSignature | KeyUsages::NonRepudiation).to_der().map_err(|e| e.to_string())?;
    let extension = Extension { extn_id: ObjectIdentifier::new_unwrap("2.5.29.15"), critical: true, extn_value: OctetString::new(usage).map_err(|e| e.to_string())? };
    let mut serial: [u8; 16] = rand::random();
    serial[0] = (serial[0] & 0x7f) | 0x01;
    let tbs = x509_cert::TbsCertificate {
        version: x509_cert::certificate::Version::V3,
        serial_number: SerialNumber::new(&serial).map_err(|e| e.to_string())?,
        signature: alg.clone(),
        issuer: name.clone(),
        validity: Validity::from_now(std::time::Duration::from_secs(10 * 365 * 24 * 3600)).map_err(|e| e.to_string())?,
        subject: name,
        subject_public_key_info: key.spki()?,
        issuer_unique_id: None,
        subject_unique_id: None,
        extensions: Some(vec![extension]),
    };
    let signature = key.sign(&tbs.to_der().map_err(|e| e.to_string())?);
    Ok(Certificate { tbs_certificate: tbs, signature_algorithm: alg, signature: BitString::from_bytes(&signature).map_err(|e| e.to_string())? })
}

/// The seal as Sign Settings hold it, checked: the key parses (P-256 or P-384), the certificate's public key is the key's.
fn parse(key_pem: &str, certificates_pem: &str) -> std::result::Result<SealKey, String> {
    let key = Key::parse(key_pem)?;
    let chain = Certificate::load_pem_chain(certificates_pem.as_bytes()).map_err(|e| format!("the seal certificate cannot be read ({e})"))?;
    let own = chain.first().ok_or("the seal certificate is empty")?;
    let mine = key.spki()?.to_der().map_err(|e| e.to_string())?;
    if own.tbs_certificate.subject_public_key_info.to_der().map_err(|e| e.to_string())? != mine {
        return Err("the seal certificate is not for the seal key".into());
    }
    Ok(SealKey { key, chain })
}

/// What a bad seal is checked for when Sign Settings are saved: a key and certificate that do not match, an RSA key, a
/// certificate that is not one, are refused there, not at the first seal.
pub fn check(key_pem: &str, certificates_pem: &str) -> std::result::Result<(), String> {
    parse(key_pem, certificates_pem).map(|_| ())
}

async fn read(k: &Kernel, tx: &mut Tx, tenant: Uuid) -> Result<Option<(String, Option<String>, bool)>> {
    let row: Option<(Uuid, Option<String>, Option<String>)> =
        sqlx::query_as("select id, seal_certificate, seal::text from e_sign_settings where tenant_id = $1 limit 1").bind(tenant).fetch_optional(&mut **tx).await?;
    let Some((id, certificate, seal)) = row else { return Ok(None) };
    let pem = crate::keys::field(k, tx, tenant, "SignSettings", id, "seal_key").await?;
    Ok(pem.map(|p| (p.to_string(), certificate, seal.as_deref() != Some("None"))))
}

/// The seal, or `None` when the workspace chose no seal. The first call makes one, in a transaction of its own committed at
/// once, so concurrent first seals agree on one key; it is then read back through `tx`.
pub async fn load(k: &Kernel, tx: &mut Tx, tenant: Uuid) -> Result<Option<SealKey>> {
    let chosen: Option<String> = sqlx::query_scalar("select seal::text from e_sign_settings where tenant_id = $1 limit 1").bind(tenant).fetch_optional(&mut **tx).await?.flatten();
    if chosen.as_deref() == Some("None") {
        return Ok(None);
    }
    if read(k, tx, tenant).await?.is_none() {
        create(k, tenant).await?;
    }
    let (pem, certificates, _) = read(k, tx, tenant).await?.ok_or_else(|| Error::Retryable("the seal key is being created".into()))?;
    let certificates = certificates.ok_or_else(|| Error::invalid("Sign Settings hold a seal key without its certificate"))?;
    parse(&pem, &certificates).map(Some).map_err(Error::invalid)
}

async fn create(k: &Kernel, tenant: Uuid) -> Result<()> {
    let sys = Ctx::system(tenant, k.registry.schema(tenant).await?);
    for _ in 0..3 {
        let mut tx = k.db.tenant_tx(tenant).await?;
        let id: Option<Uuid> = sqlx::query_scalar("select id from e_sign_settings where tenant_id = $1 limit 1 for update").bind(tenant).fetch_optional(&mut *tx).await?;
        // Under the lock, again: another seal may have made it a moment ago.
        if let Some(id) = id
            && crate::keys::field(k, &mut tx, tenant, "SignSettings", id, "seal_key").await?.is_some()
        {
            return tx.commit().await.map_err(Into::into);
        }
        let name: String = sqlx::query_scalar("select name from tenant where id = $1").bind(tenant).fetch_one(&mut *tx).await?;
        let key = Key::P256(p256::ecdsa::SigningKey::random(&mut p256::elliptic_curve::rand_core::OsRng));
        let certificate = self_signed(&key, &name).map_err(Error::internal)?;
        let mut values = Doc::new();
        values.insert("seal_key".into(), json!(key.pem().map_err(Error::internal)?));
        values.insert("seal_certificate".into(), json!(certificate.to_pem(LineEnding::LF).map_err(|e| Error::internal(e.to_string()))?));
        let written = match id {
            Some(id) => k.update_tx(&sys, &mut tx, "SignSettings", id, values, None).await,
            None => k.insert_tx(&sys, &mut tx, "SignSettings", obj(serde_json::Value::Object(values))).await,
        };
        match written {
            Ok(_) => return tx.commit().await.map_err(Into::into),
            Err(Error::Conflict(_)) => continue,
            Err(e) => return Err(e),
        }
    }
    Err(Error::Retryable("the seal key is being created".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_made_seal_checks_and_a_foreign_certificate_does_not() {
        let key = Key::P256(p256::ecdsa::SigningKey::random(&mut p256::elliptic_curve::rand_core::OsRng));
        let other = Key::P384(p384::ecdsa::SigningKey::random(&mut p384::elliptic_curve::rand_core::OsRng));
        let cert = self_signed(&key, "Acme, Inc.").unwrap().to_pem(LineEnding::LF).unwrap();
        let foreign = self_signed(&other, "Acme").unwrap().to_pem(LineEnding::LF).unwrap();
        assert!(check(&key.pem().unwrap(), &cert).is_ok());
        assert!(check(&key.pem().unwrap(), &foreign).unwrap_err().contains("not for the seal key"));
        assert!(check(&other.pem().unwrap(), &foreign).is_ok(), "a P-384 key works too");
    }

    #[test]
    fn only_the_two_curves_are_accepted() {
        // A PKCS#8 header for an RSA key is not a P-256 or P-384 key: refused with the reason, whatever the bytes.
        let rsa = "-----BEGIN PRIVATE KEY-----\nMIIBVQIBADANBgkqhkiG9w0BAQEFAASCAT8wggE7AgEAAkEAu7vjTXgGDKrVZyeX\n-----END PRIVATE KEY-----\n";
        assert!(Key::parse(rsa).err().unwrap().contains("RSA keys are not accepted"));
        assert!(Key::parse("not a key").is_err());
    }
}
