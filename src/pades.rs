//! The workspace's digital seal on a sealed PDF: a CAdES detached signature (`ETSI.CAdES.detached`, PAdES baseline) made
//! with the workspace's key as a last incremental revision, with the PDF's DocMDP "no changes allowed" certification (P = 1),
//! an optional RFC 3161 time stamp of the signature (PAdES B-T), and the audit trail embedded as `audit-trail.json`. The
//! signature covers every byte of the file but its own `/Contents`, so any later change, an added revision included,
//! shows. Checking recomputes all of it from the file alone. The CMS structures are written with `der`.

use crate::seal_key::{Hash, SealKey};
use chrono::{DateTime, Utc};
use lopdf::{Dictionary, IncrementalDocument, Object, Stream, StringFormat};
use p256::ecdsa::signature::Verifier;
use p256::pkcs8::DecodePublicKey;
use sha2::{Digest, Sha256};
use x509_cert::Certificate;
use x509_cert::attr::Attribute;
use x509_cert::der::asn1::{ObjectIdentifier, OctetString, SetOfVec, UtcTime};
use x509_cert::der::{Any, Decode, Encode, Sequence};
use x509_cert::name::Name;
use x509_cert::serial_number::SerialNumber;
use x509_cert::spki::AlgorithmIdentifierOwned;

/// Bytes reserved for the CMS in `/Contents` (a certificate chain and a time-stamp token fit well within).
const PLACEHOLDER: usize = 24576;
/// The width the ByteRange numbers are written with until the real ones are known.
const WIDE: i64 = 1_000_000_000;

// ───────────────────────────── the CMS ─────────────────────────────

#[derive(Sequence)]
struct ContentInfo {
    kind: ObjectIdentifier,
    #[asn1(context_specific = "0", tag_mode = "EXPLICIT")]
    content: SignedData,
}

#[derive(Sequence)]
struct SignedData {
    version: u8,
    digest_algorithms: SetOfVec<AlgorithmIdentifierOwned>,
    encapsulated: Encapsulated,
    #[asn1(context_specific = "0", tag_mode = "IMPLICIT", constructed = "true", optional = "true")]
    certificates: Option<SetOfVec<Certificate>>,
    signer_infos: SetOfVec<SignerInfo>,
}

#[derive(Sequence)]
struct Encapsulated {
    kind: ObjectIdentifier,
}

#[derive(Sequence)]
struct SignerInfo {
    version: u8,
    issuer_and_serial: IssuerAndSerial,
    digest_algorithm: AlgorithmIdentifierOwned,
    #[asn1(context_specific = "0", tag_mode = "IMPLICIT", constructed = "true")]
    signed_attributes: SetOfVec<Attribute>,
    signature_algorithm: AlgorithmIdentifierOwned,
    signature: OctetString,
    #[asn1(context_specific = "1", tag_mode = "IMPLICIT", constructed = "true", optional = "true")]
    unsigned_attributes: Option<SetOfVec<Attribute>>,
}

/// A SET of one signer: there is never more, so nothing is ever ordered.
impl x509_cert::der::ValueOrd for SignerInfo {
    fn value_cmp(&self, _other: &Self) -> x509_cert::der::Result<std::cmp::Ordering> {
        Ok(std::cmp::Ordering::Equal)
    }
}

#[derive(Sequence)]
struct IssuerAndSerial {
    issuer: Name,
    serial: SerialNumber,
}

/// `ESSCertIDv2` with the default hash (SHA-256) left out, and `SigningCertificateV2` around it (RFC 5035).
#[derive(Sequence)]
struct EssCertId {
    hash: OctetString,
}

#[derive(Sequence)]
struct SigningCertificateV2 {
    certs: Vec<EssCertId>,
}

fn oid(s: &str) -> ObjectIdentifier {
    ObjectIdentifier::new_unwrap(s)
}

fn attribute(kind: &str, value: Any) -> std::result::Result<Attribute, String> {
    let mut values = SetOfVec::new();
    values.insert(value).map_err(|e| e.to_string())?;
    Ok(Attribute { oid: oid(kind), values })
}

// ───────────────────────────── the PDF side ─────────────────────────────

/// A PDF with the signature's place made, and what is needed to sign it.
pub struct Begun {
    bytes: Vec<u8>,
    /// Where `/Contents`' hex digits start and end.
    contents: (usize, usize),
    hash: Hash,
    attributes: SetOfVec<Attribute>,
    signature: Vec<u8>,
    chain: Vec<Certificate>,
    signer: (Name, SerialNumber),
    algorithm: (ObjectIdentifier, ObjectIdentifier),
}

impl Begun {
    /// The signature value: what a time-stamping authority is asked to stamp (its SHA-256, `tsa::imprint_of`).
    pub fn signature(&self) -> &[u8] {
        &self.signature
    }

    /// The finished PDF: the CMS, with the time-stamp token (DER `ContentInfo`) when there is one, patched into its place.
    pub fn finish(mut self, token: Option<&[u8]>) -> std::result::Result<Vec<u8>, String> {
        let unsigned = match token {
            Some(t) => {
                let mut set = SetOfVec::new();
                set.insert(attribute("1.2.840.113549.1.9.16.2.14", Any::from_der(t).map_err(|e| format!("the time-stamp token: {e}"))?)?)
                    .map_err(|e| e.to_string())?;
                Some(set)
            }
            None => None,
        };
        let mut digests = SetOfVec::new();
        digests.insert(AlgorithmIdentifierOwned { oid: self.hash.oid(), parameters: None }).map_err(|e| e.to_string())?;
        let mut certs = SetOfVec::new();
        for c in &self.chain {
            certs.insert(c.clone()).map_err(|e| e.to_string())?;
        }
        let mut infos = SetOfVec::new();
        infos
            .insert(SignerInfo {
                version: 1,
                issuer_and_serial: IssuerAndSerial { issuer: self.signer.0.clone(), serial: self.signer.1.clone() },
                digest_algorithm: AlgorithmIdentifierOwned { oid: self.hash.oid(), parameters: None },
                signed_attributes: self.attributes.clone(),
                signature_algorithm: AlgorithmIdentifierOwned { oid: self.algorithm.0, parameters: None },
                signature: OctetString::new(self.signature.clone()).map_err(|e| e.to_string())?,
                unsigned_attributes: unsigned,
            })
            .map_err(|e| e.to_string())?;
        let cms = ContentInfo {
            kind: oid("1.2.840.113549.1.7.2"),
            content: SignedData {
                version: 1,
                digest_algorithms: digests,
                encapsulated: Encapsulated { kind: oid("1.2.840.113549.1.7.1") },
                certificates: Some(certs),
                signer_infos: infos,
            },
        }
        .to_der()
        .map_err(|e| e.to_string())?;
        if cms.len() > PLACEHOLDER {
            return Err(format!("the signature ({} bytes) does not fit its place ({PLACEHOLDER} bytes)", cms.len()));
        }
        let hexed = hex::encode(&cms);
        let (from, to) = self.contents;
        self.bytes[from..from + hexed.len()].copy_from_slice(hexed.as_bytes());
        debug_assert!(to >= from + hexed.len());
        Ok(self.bytes)
    }
}

/// Makes the signature's place in `pdf` (a last incremental revision: an invisible signature field on page 1, the
/// certification, the embedded audit trail), then signs it with `seal` at `time`. A PDF already certified is refused:
/// nothing may be added after a DocMDP signature.
pub fn begin(pdf: &[u8], seal: &SealKey, trail: &[u8], time: DateTime<Utc>) -> std::result::Result<Begun, String> {
    let fail = |e: lopdf::Error| format!("the seal could not be added ({e})");
    let prev = crate::pdf::load(pdf)?;
    let root = prev.trailer.get(b"Root").and_then(Object::as_reference).map_err(fail)?;
    if prev.catalog().map_err(fail)?.has(b"Perms") {
        return Err("the PDF is already certified: nothing may be added after it".into());
    }
    let page = *prev.get_pages().get(&1).ok_or("the PDF has no pages")?;
    let mut inc = IncrementalDocument::create_from(pdf.to_vec(), prev);
    inc.opt_clone_object_to_new_document(root).map_err(fail)?;
    inc.opt_clone_object_to_new_document(page).map_err(fail)?;
    let doc = &mut inc.new_document;

    // The signature dictionary, its contents and ByteRange written wide: patched to their true values below.
    let mut reference = Dictionary::new();
    reference.set("Type", Object::Name(b"SigRef".to_vec()));
    reference.set("TransformMethod", Object::Name(b"DocMDP".to_vec()));
    let mut params = Dictionary::new();
    params.set("Type", Object::Name(b"TransformParams".to_vec()));
    params.set("P", 1i64);
    params.set("V", Object::Name(b"1.2".to_vec()));
    reference.set("TransformParams", params);
    reference.set("Data", Object::Reference(root));
    let mut sig = Dictionary::new();
    sig.set("Type", Object::Name(b"Sig".to_vec()));
    sig.set("Filter", Object::Name(b"Adobe.PPKLite".to_vec()));
    sig.set("SubFilter", Object::Name(b"ETSI.CAdES.detached".to_vec()));
    sig.set("ByteRange", vec![Object::Integer(0), Object::Integer(WIDE), Object::Integer(WIDE), Object::Integer(WIDE)]);
    sig.set("Contents", Object::String(vec![0u8; PLACEHOLDER], StringFormat::Hexadecimal));
    sig.set("M", Object::string_literal(format!("D:{}Z", time.format("%Y%m%d%H%M%S"))));
    sig.set("Name", Object::string_literal("Ferrum Sign workspace seal"));
    sig.set("Reference", vec![Object::Dictionary(reference)]);
    let sig_id = doc.add_object(sig);
    let mut widget = Dictionary::new();
    for (k, v) in [("Type", "Annot"), ("Subtype", "Widget"), ("FT", "Sig")] {
        widget.set(k, Object::Name(v.as_bytes().to_vec()));
    }
    widget.set("T", Object::string_literal("Seal"));
    widget.set("Rect", vec![Object::Integer(0), Object::Integer(0), Object::Integer(0), Object::Integer(0)]);
    widget.set("F", 132i64);
    widget.set("P", Object::Reference(page));
    widget.set("V", Object::Reference(sig_id));
    let widget_id = doc.add_object(widget);
    // The page lists the widget as an annotation.
    let annots = doc.get_dictionary_mut(page).map_err(fail)?;
    match annots.get_mut(b"Annots") {
        Ok(Object::Array(a)) => a.push(Object::Reference(widget_id)),
        _ => annots.set("Annots", vec![Object::Reference(widget_id)]),
    }
    // The audit trail as an embedded file.
    let mut file = Dictionary::new();
    file.set("Type", Object::Name(b"EmbeddedFile".to_vec()));
    file.set("Subtype", Object::Name(b"application#2Fjson".to_vec()));
    let stream = doc.add_object(Stream::new(file, trail.to_vec()));
    let mut ef = Dictionary::new();
    ef.set("F", Object::Reference(stream));
    let mut spec = Dictionary::new();
    spec.set("Type", Object::Name(b"Filespec".to_vec()));
    spec.set("F", Object::string_literal("audit-trail.json"));
    spec.set("UF", Object::string_literal("audit-trail.json"));
    spec.set("AFRelationship", Object::Name(b"Supplement".to_vec()));
    spec.set("EF", ef);
    let spec_id = doc.add_object(spec);
    // The catalog: the form with the field, the certification, the attachment.
    let catalog = doc.get_dictionary_mut(root).map_err(fail)?;
    let mut form = match catalog.get(b"AcroForm") {
        Ok(Object::Dictionary(d)) => d.clone(),
        _ => Dictionary::new(),
    };
    match form.get_mut(b"Fields") {
        Ok(Object::Array(a)) => a.push(Object::Reference(widget_id)),
        _ => form.set("Fields", vec![Object::Reference(widget_id)]),
    }
    form.set("SigFlags", 3i64);
    catalog.set("AcroForm", form);
    let mut perms = Dictionary::new();
    perms.set("DocMDP", Object::Reference(sig_id));
    catalog.set("Perms", perms);
    let mut names = Dictionary::new();
    names.set("Names", vec![Object::string_literal("audit-trail.json"), Object::Reference(spec_id)]);
    let mut tree = Dictionary::new();
    tree.set("EmbeddedFiles", names);
    if !catalog.has(b"Names") {
        catalog.set("Names", tree);
    }
    catalog.set("AF", vec![Object::Reference(spec_id)]);

    let mut bytes = Vec::new();
    inc.save_to(&mut bytes).map_err(|e| format!("the sealed PDF could not be written ({e})"))?;

    // Where the placeholders are written: the zeros of `/Contents <…>`, and the ByteRange's brackets.
    let zeros = format!("<{}>", "0".repeat(PLACEHOLDER * 2));
    let at = find_last(&bytes, zeros.as_bytes()).ok_or("the signature's place cannot be found")?;
    let (start, end) = (at, at + zeros.len());
    let br_key = find_last(&bytes[..start], b"/ByteRange").ok_or("the ByteRange cannot be found")?;
    let open = br_key + bytes[br_key..].iter().position(|b| *b == b'[').ok_or("the ByteRange cannot be found")?;
    let close = open + bytes[open..].iter().position(|b| *b == b']').ok_or("the ByteRange cannot be found")?;
    let ranges = format!("0 {start} {end} {}", bytes.len() - end);
    if ranges.len() > close - open - 1 {
        return Err("the ByteRange does not fit its place".into());
    }
    let mut written = ranges.into_bytes();
    written.resize(close - open - 1, b' ');
    bytes[open + 1..close].copy_from_slice(&written);

    // What is signed: every byte but the contents.
    let hash = seal.key.hash();
    let covered = hash.digest(&[&bytes[..start], &bytes[end..]]);
    let own = seal.certificate();
    let cert_hash = Sha256::digest(own.to_der().map_err(|e| e.to_string())?);
    let signing_certificate = SigningCertificateV2 { certs: vec![EssCertId { hash: OctetString::new(cert_hash.to_vec()).map_err(|e| e.to_string())? }] };
    let any = |v: &dyn EncodeAny| v.any();
    let mut attributes = SetOfVec::new();
    for a in [
        attribute("1.2.840.113549.1.9.3", any(&oid("1.2.840.113549.1.7.1")))?,
        attribute("1.2.840.113549.1.9.4", any(&OctetString::new(covered).map_err(|e| e.to_string())?))?,
        attribute(
            "1.2.840.113549.1.9.5",
            any(&UtcTime::from_unix_duration(std::time::Duration::from_secs(time.timestamp().max(0) as u64)).map_err(|e| e.to_string())?),
        )?,
        attribute("1.2.840.113549.1.9.16.2.47", any(&signing_certificate))?,
    ] {
        attributes.insert(a).map_err(|e| e.to_string())?;
    }
    // The signature is over the attributes as a SET (tag 0x31), not as the [0] they travel in.
    let signature = seal.key.sign(&attributes.to_der().map_err(|e| e.to_string())?);
    Ok(Begun {
        bytes,
        contents: (start + 1, end - 1),
        hash,
        attributes,
        signature,
        chain: seal.chain.clone(),
        signer: (own.tbs_certificate.issuer.clone(), own.tbs_certificate.serial_number.clone()),
        algorithm: (seal.key.algorithm(), seal.key.algorithm()),
    })
}

/// A value as the `Any` an attribute holds.
trait EncodeAny {
    fn any(&self) -> Any;
}

impl<T: Encode> EncodeAny for T {
    fn any(&self) -> Any {
        Any::from_der(&self.to_der().expect("a value encodes")).expect("an encoded value is an Any")
    }
}

fn find_last(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).rposition(|w| w == needle)
}

// ───────────────────────────── checking ─────────────────────────────

/// What checking a file's seal found.
#[derive(Debug, Clone, PartialEq)]
pub struct Checked {
    /// The signature verifies against the certificate in it, over every byte of the file but its own contents.
    pub valid: bool,
    /// Why not, when it is not.
    pub problem: Option<String>,
    pub subject: Option<String>,
    pub certificate_sha256: Option<String>,
    /// The time the authority's stamp states (seconds since 1970), when the signature carries one.
    pub timestamp: Option<i64>,
}

fn bad(problem: &str) -> Checked {
    Checked { valid: false, problem: Some(problem.into()), subject: None, certificate_sha256: None, timestamp: None }
}

/// The last signature of `pdf`, checked from the file alone: its ByteRange covers the whole file but the contents, the
/// digest in the CMS is that of those bytes, and the signature verifies under the certificate the CMS carries. `None` when
/// the file has no such signature.
pub fn check(pdf: &[u8]) -> Option<Checked> {
    let key = find_last(pdf, b"/ByteRange")?;
    let open = key + pdf[key..].iter().position(|b| *b == b'[')?;
    let close = open + pdf[open..].iter().position(|b| *b == b']')?;
    let nums: Vec<usize> = std::str::from_utf8(&pdf[open + 1..close]).ok()?.split_whitespace().filter_map(|n| n.parse().ok()).collect();
    let [a, b, c, d] = nums[..] else { return Some(bad("the signature's ByteRange is not valid")) };
    if a != 0 || b > c || c + d != pdf.len() {
        return Some(bad("the signature does not cover the whole file: something was added or changed after it"));
    }
    // The contents between the two ranges: `<hex…>` padded with zeros.
    let hex_part = std::str::from_utf8(pdf.get(b + 1..c.checked_sub(1)?)?).ok()?;
    let der = hex::decode(hex_part).ok()?;
    let Some(cms) = trim_der(&der) else { return Some(bad("the signature's contents cannot be read")) };
    Some(check_cms(cms, &pdf[..b], &pdf[c..]))
}

/// The leading DER element of `bytes` (the rest being padding).
fn trim_der(bytes: &[u8]) -> Option<&[u8]> {
    let first = *bytes.get(1)?;
    let total = if first < 0x80 {
        2 + first as usize
    } else {
        let n = (first & 0x7f) as usize;
        2 + n + bytes.get(2..2 + n)?.iter().fold(0usize, |a, x| (a << 8) | *x as usize)
    };
    bytes.get(..total)
}

fn check_cms(cms: &[u8], before: &[u8], after: &[u8]) -> Checked {
    let Ok(info) = ContentInfo::from_der(cms) else { return bad("the signature is not a CMS signature") };
    let sd = info.content;
    let (Some(certs), Some(signer)) = (sd.certificates.as_ref(), sd.signer_infos.iter().next()) else { return bad("the signature holds no certificate") };
    let hash = if signer.digest_algorithm.oid == Hash::S384.oid() { Hash::S384 } else { Hash::S256 };
    let attribute = |kind: &str| signer.signed_attributes.iter().find(|a| a.oid == oid(kind)).and_then(|a| a.values.iter().next().cloned());
    let digest = attribute("1.2.840.113549.1.9.4").and_then(|v| v.decode_as::<OctetString>().ok());
    if digest.as_ref().map(|d| d.as_bytes().to_vec()) != Some(hash.digest(&[before, after])) {
        return bad("the file's bytes no longer match the signature's digest");
    }
    let Some(cert) = certs
        .iter()
        .find(|c| c.tbs_certificate.serial_number == signer.issuer_and_serial.serial && c.tbs_certificate.issuer == signer.issuer_and_serial.issuer)
    else {
        return bad("the signer's certificate is not in the signature");
    };
    let Ok(spki) = cert.tbs_certificate.subject_public_key_info.to_der() else { return bad("the certificate's key cannot be read") };
    let Ok(signed) = signer.signed_attributes.to_der() else { return bad("the signed attributes cannot be read") };
    let ok = match signer.signature_algorithm.oid.to_string().as_str() {
        "1.2.840.10045.4.3.2" => p256::ecdsa::VerifyingKey::from_public_key_der(&spki).ok().and_then(|k| {
            let sig = p256::ecdsa::DerSignature::try_from(signer.signature.as_bytes()).ok()?;
            k.verify(&signed, &sig).ok()
        }),
        "1.2.840.10045.4.3.3" => p384::ecdsa::VerifyingKey::from_public_key_der(&spki).ok().and_then(|k| {
            let sig = p384::ecdsa::DerSignature::try_from(signer.signature.as_bytes()).ok()?;
            k.verify(&signed, &sig).ok()
        }),
        _ => return bad("the signature's algorithm is not one Sign checks"),
    };
    if ok.is_none() {
        return bad("the signature does not verify under the certificate in it");
    }
    let timestamp = signer
        .unsigned_attributes
        .as_ref()
        .and_then(|u| u.iter().find(|a| a.oid == oid("1.2.840.113549.1.9.16.2.14")))
        .and_then(|a| a.values.iter().next())
        .and_then(|v| v.to_der().ok())
        .and_then(|t| crate::tsa::time_of(&t).ok());
    Checked {
        valid: true,
        problem: None,
        subject: Some(cert.tbs_certificate.subject.to_string()),
        certificate_sha256: cert.to_der().ok().map(|d| hex::encode(Sha256::digest(d))),
        timestamp,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::seal_key::Key;

    fn seal() -> SealKey {
        let key = Key::P256(p256::ecdsa::SigningKey::random(&mut p256::elliptic_curve::rand_core::OsRng));
        let cert = crate::seal_key::self_signed(&key, "Acme").unwrap();
        SealKey { key, chain: vec![cert] }
    }

    fn sealed_pdf() -> Vec<u8> {
        // A page PDF through the engine's own sealing: stamped with nothing, a certificate page added.
        let original = crate::pdf::tests::sample(&[(612.0, 792.0, 0)]);
        crate::pdf::seal(&original, &[], "T", &|_, _| vec![crate::pdf::Para::new("Certificate", 12.0)]).expect("a sealed pdf").bytes
    }

    #[test]
    fn the_seal_verifies_and_covers_everything_but_its_contents() {
        let seal = seal();
        let pdf = sealed_pdf();
        let begun = begin(&pdf, &seal, b"{\"events\":[]}", Utc::now()).expect("a place for the seal");
        let out = begun.finish(None).expect("signed");
        let checked = check(&out).expect("a signature");
        assert!(checked.valid, "{:?}", checked.problem);
        assert!(checked.subject.unwrap().contains("Sign seal"));
        // The ByteRange covers the whole file but the contents: two ranges and the gap are the file.
        let key = find_last(&out, b"/ByteRange").unwrap();
        let text = String::from_utf8_lossy(&out[key..key + 80]).to_string();
        assert!(text.contains("[0 "), "{text}");
        assert!(out.windows(10).any(|w| w == b"/DocMDP\n  ".as_slice()) || String::from_utf8_lossy(&out).contains("DocMDP"), "the file is certified");
        assert!(String::from_utf8_lossy(&out).contains("audit-trail.json"), "the audit trail is embedded");
    }

    #[test]
    fn one_changed_byte_breaks_the_seal() {
        let seal = seal();
        let out = begin(&sealed_pdf(), &seal, b"{}", Utc::now()).unwrap().finish(None).unwrap();
        // A byte of the file's own pages and one just before the signature's place (the place itself, past the CMS, is padding
        // the ByteRange leaves out).
        let place = find_last(&out, b"/Contents <").unwrap();
        for at in [20usize, place - 40] {
            let mut changed = out.clone();
            changed[at] ^= 0x01;
            assert!(!check(&changed).unwrap().valid, "a change at {at}");
        }
    }

    #[test]
    fn an_added_revision_breaks_the_certification() {
        let seal = seal();
        let mut out = begin(&sealed_pdf(), &seal, b"{}", Utc::now()).unwrap().finish(None).unwrap();
        out.extend_from_slice(b"\n%added later\n");
        let checked = check(&out).unwrap();
        assert!(!checked.valid && checked.problem.unwrap().contains("added or changed after"));
    }

    #[test]
    fn a_certified_pdf_is_refused_as_a_source() {
        let seal = seal();
        let out = begin(&sealed_pdf(), &seal, b"{}", Utc::now()).unwrap().finish(None).unwrap();
        assert!(begin(&out, &seal, b"{}", Utc::now()).err().unwrap().contains("already certified"));
    }
}
