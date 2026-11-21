//! RFC 3161 time stamps: the request for a digest, the call through the workspace's time-stamping authority connection (a
//! description of data, no vendor named here), and the check of what came back: granted, for the digest asked, with the
//! nonce sent. The stamp is kept in the signature as its unsigned attribute (`pades.rs`).

use ferrum_kernel::connections::{self, Call};
use ferrum_kernel::{Ctx, Error, Kernel, Result};
use serde_json::json;
use sha2::{Digest, Sha256};
use x509_cert::der::asn1::{GeneralizedTime, Int, ObjectIdentifier, OctetString};
use x509_cert::der::{Decode, Encode, Sequence};
use x509_cert::spki::AlgorithmIdentifierOwned;

/// `TimeStampReq` (RFC 3161 2.4.1): version 1, the imprint, a nonce, and the authority's certificate asked for.
#[derive(Sequence)]
struct Request {
    version: u8,
    imprint: Imprint,
    nonce: Int,
    cert_req: bool,
}

#[derive(Sequence, Clone, PartialEq, Eq)]
struct Imprint {
    algorithm: AlgorithmIdentifierOwned,
    digest: OctetString,
}

/// The request for a time stamp of `digest` (a SHA-256), and the nonce it carries.
pub fn request(digest: &[u8; 32]) -> std::result::Result<(Vec<u8>, Vec<u8>), String> {
    let mut nonce: [u8; 8] = rand::random();
    nonce[0] &= 0x7f;
    nonce[0] |= 0x01;
    let req = Request {
        version: 1,
        imprint: Imprint {
            algorithm: AlgorithmIdentifierOwned { oid: ObjectIdentifier::new_unwrap("2.16.840.1.101.3.4.2.1"), parameters: None },
            digest: OctetString::new(digest.to_vec()).map_err(|e| e.to_string())?,
        },
        nonce: Int::new(&nonce).map_err(|e| e.to_string())?,
        cert_req: true,
    };
    Ok((req.to_der().map_err(|e| e.to_string())?, nonce.to_vec()))
}

/// The token of a granted `TimeStampResp` for `digest` with `nonce`: its DER `ContentInfo`, and the time it states
/// (seconds since 1970). A refusal, a token for another digest, or one without the nonce sent, is an error.
pub fn check(response: &[u8], digest: &[u8; 32], nonce: &[u8]) -> std::result::Result<(Vec<u8>, i64), String> {
    let bad = |m: String| format!("the time-stamp answer cannot be read ({m})");
    // TimeStampResp { PKIStatusInfo { status INTEGER, … }, timeStampToken OPTIONAL }.
    let top = children(response).map_err(bad)?;
    let elements = children(top.first().ok_or("the time-stamp answer is empty")?.1).map_err(bad)?;
    let status = children(elements.first().ok_or("the time-stamp answer has no status")?.1).map_err(bad)?;
    let status = status.first().and_then(|s| s.1.last().copied()).ok_or("the time-stamp answer has no status")?;
    if status > 1 {
        return Err(format!("the time-stamping authority refused (status {status})"));
    }
    let token_der = elements.get(1).ok_or("the time-stamp answer holds no token")?.2.to_vec();
    let info = tst_info(&token_der)?;
    if info.digest != digest.as_slice() {
        return Err("the time stamp is for another digest".into());
    }
    // The nonce is a positive INTEGER: compared as its DER content, without leading zeros.
    let strip = |n: &[u8]| n.iter().skip_while(|b| **b == 0).copied().collect::<Vec<u8>>();
    if info.nonce.as_deref().map(strip) != Some(strip(nonce)) {
        return Err("the time stamp does not carry the nonce that was sent".into());
    }
    Ok((token_der, info.time))
}

/// The elements of one DER value's content as `(tag, content, whole element)`: single-byte tags, short and long length forms.
pub(crate) fn children(der: &[u8]) -> std::result::Result<Vec<(u8, &[u8], &[u8])>, String> {
    let mut out = Vec::new();
    let mut at = 0;
    while at < der.len() {
        let tag = der[at];
        let first = *der.get(at + 1).ok_or("a DER element is cut short")?;
        let (len, head) = if first < 0x80 {
            (first as usize, 2)
        } else {
            let n = (first & 0x7f) as usize;
            if n == 0 || n > 4 || at + 2 + n > der.len() {
                return Err("a DER length is not valid".into());
            }
            (der[at + 2..at + 2 + n].iter().fold(0usize, |a, b| (a << 8) | *b as usize), 2 + n)
        };
        let content = der.get(at + head..at + head + len).ok_or("a DER element is cut short")?;
        out.push((tag, content, &der[at..at + head + len]));
        at += head + len;
    }
    Ok(out)
}

/// What is checked of a token's `TSTInfo`.
struct Tst {
    digest: Vec<u8>,
    nonce: Option<Vec<u8>>,
    /// Seconds since 1970.
    time: i64,
}

/// `TSTInfo` of a token: `ContentInfo { contentType, [0] SignedData { version, algorithms, encapContentInfo { type, [0]
/// OCTET STRING }, … } }`, walked down to the octet string, whose `TSTInfo { version, policy, messageImprint, serialNumber,
/// genTime, accuracy?, ordering?, nonce?, … }` is then read element by element.
fn tst_info(token: &[u8]) -> std::result::Result<Tst, String> {
    let bad = |m: String| format!("the time-stamp token cannot be read ({m})");
    // The content of the one element `d` is.
    let first =
        |d: &[u8]| -> std::result::Result<Vec<u8>, String> { children(d).and_then(|c| c.first().map(|e| e.1.to_vec()).ok_or_else(|| "empty".to_string())) };
    // The content of the `n`th child of `d`'s content.
    let nth = |d: &[u8], n: usize| -> std::result::Result<Vec<u8>, String> {
        children(d).and_then(|c| c.get(n).map(|e| e.1.to_vec()).ok_or_else(|| "too short".to_string()))
    };
    let octets = (|| {
        let content_info = first(token)?;
        let signed_data = first(&nth(&content_info, 1)?)?;
        let encapsulated = nth(&signed_data, 2)?;
        first(&nth(&encapsulated, 1)?)
    })()
    .map_err(bad)?;
    let info = children(&octets).and_then(|c| c.first().map(|e| e.1.to_vec()).ok_or_else(|| "empty".to_string())).map_err(bad)?;
    let elements = children(&info).map_err(bad)?;
    let imprint = children(elements.get(2).ok_or("the token has no imprint")?.1).map_err(bad)?;
    let digest = imprint.get(1).ok_or("the token has no digest")?.1.to_vec();
    let time = GeneralizedTime::from_der(elements.get(4).ok_or("the token has no time")?.2)
        .map_err(|e| format!("the token's time: {e}"))?
        .to_unix_duration()
        .as_secs() as i64;
    // After the time: an optional accuracy (a SEQUENCE) and ordering (a BOOLEAN), then the nonce (an INTEGER) if any.
    let nonce = elements.iter().skip(5).find(|e| e.0 == 0x02).map(|e| e.1.to_vec());
    Ok(Tst { digest, nonce, time })
}

/// The time a token (DER `ContentInfo`) states, seconds since 1970.
pub fn time_of(token: &[u8]) -> std::result::Result<i64, String> {
    Ok(tst_info(token)?.time)
}

/// A time stamp of `digest` from the connection named `connection` (called outside any transaction): the token's DER and
/// the time it states. The request goes as a stored file the connection sends; both files are deleted again.
pub async fn stamp(k: &Kernel, sys: &Ctx, connection: &str, digest: &[u8; 32]) -> Result<(Vec<u8>, i64)> {
    let (req, nonce) = request(digest).map_err(Error::internal)?;
    let stored = k.store_file(sys, None, "timestamp-request.tsq", "application/timestamp-query", &req).await?;
    let file = stored.get("id").and_then(|v| v.as_str()).and_then(|v| v.parse::<uuid::Uuid>().ok()).ok_or_else(|| Error::internal("stored file without id"))?;
    let call = Call { connection: connection.to_string(), operation: "stamp".into(), inputs: json!({"request_file": file.to_string()}), ..Default::default() };
    let answered = connections::call(k, sys, Some("sign"), &call).await;
    let _ = k.delete_file(sys, file).await;
    let answered = answered?;
    let token_file = answered
        .get("token_file")
        .and_then(|v| v.as_str())
        .and_then(|v| v.parse::<uuid::Uuid>().ok())
        .ok_or_else(|| Error::Unavailable("the time-stamping authority gave no answer".into()))?;
    let bytes = k.read_file(sys, token_file).await.map(|f| f.2);
    let _ = k.delete_file(sys, token_file).await;
    check(&bytes?, digest, &nonce).map_err(Error::Unavailable)
}

/// SHA-256 of a signature: what is time-stamped (RFC 3161 and the signature-time-stamp attribute of CAdES).
pub fn imprint_of(signature: &[u8]) -> [u8; 32] {
    Sha256::digest(signature).into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use x509_cert::der::Any;

    /// `TSTInfo` as an authority writes it, to build answers with.
    #[derive(Sequence)]
    struct TstInfo {
        version: u8,
        policy: ObjectIdentifier,
        imprint: Imprint,
        serial: Int,
        gen_time: GeneralizedTime,
        #[asn1(optional = "true")]
        accuracy: Option<Any>,
        #[asn1(optional = "true")]
        nonce: Option<Int>,
    }

    /// A granted answer built as an authority would, for `digest` and `nonce`.
    pub fn answer(digest: &[u8; 32], nonce: &[u8], status: u8) -> Vec<u8> {
        let info = TstInfo {
            version: 1,
            policy: ObjectIdentifier::new_unwrap("1.2.3.4"),
            imprint: Imprint {
                algorithm: AlgorithmIdentifierOwned { oid: ObjectIdentifier::new_unwrap("2.16.840.1.101.3.4.2.1"), parameters: None },
                digest: OctetString::new(digest.to_vec()).unwrap(),
            },
            serial: Int::new(&[1]).unwrap(),
            gen_time: GeneralizedTime::from_unix_duration(std::time::Duration::from_secs(1_800_000_000)).unwrap(),
            accuracy: None,
            nonce: Some(Int::new(nonce).unwrap()),
        };
        // ContentInfo { id-signedData, [0] { 3, SET {}, { id-ct-TSTInfo, [0] OCTET STRING } } }: nothing else in the SignedData.
        let econtent = OctetString::new(info.to_der().unwrap()).unwrap().to_der().unwrap();
        let ts_info_type = [0x06, 0x0b, 0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x09, 0x10, 0x01, 0x04];
        let encap = tlv(0x30, &[ts_info_type.to_vec(), tlv(0xa0, &econtent)].concat());
        let signed = tlv(0x30, &[vec![0x02, 0x01, 0x03, 0x31, 0x00], encap].concat());
        let signed_data_type = [0x06, 0x09, 0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x07, 0x02];
        let token = tlv(0x30, &[signed_data_type.to_vec(), tlv(0xa0, &signed)].concat());
        tlv(0x30, &[vec![0x30, 0x03, 0x02, 0x01, status], token].concat())
    }

    /// One DER element: its tag, its length in the shortest form, its content.
    fn tlv(tag: u8, content: &[u8]) -> Vec<u8> {
        let n = content.len();
        let mut out = vec![tag];
        match n {
            0..=127 => out.push(n as u8),
            128..=255 => out.extend([0x81, n as u8]),
            _ => out.extend([0x82, (n >> 8) as u8, n as u8]),
        }
        out.extend_from_slice(content);
        out
    }

    #[test]
    fn a_token_for_another_imprint_or_nonce_is_refused() {
        let digest = [7u8; 32];
        let (_, nonce) = request(&digest).unwrap();
        let granted = answer(&digest, &nonce, 0);
        let (token, at) = check(&granted, &digest, &nonce).expect("a granted answer for this digest and nonce");
        assert!(!token.is_empty() && at == 1_800_000_000);
        assert!(check(&granted, &[8u8; 32], &nonce).unwrap_err().contains("another digest"));
        assert!(check(&granted, &digest, &[1, 2, 3]).unwrap_err().contains("nonce"));
        assert!(check(&answer(&digest, &nonce, 2), &digest, &nonce).unwrap_err().contains("refused"));
    }
}
