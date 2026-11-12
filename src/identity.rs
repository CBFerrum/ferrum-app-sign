//! Identity providers (DC-40, DC-41, DC-42): national electronic identity schemes and qualified signature services
//! behind one interface. None ships with Sign; a provider is an app or a build that implements [`IdentityProvider`]
//! and is registered by the server. Sign's own step-up check is the platform's one-time code (signing.rs).
//!
//! What a provider is asked: to confirm who a signer is for one request, given the request's number and the SHA-256
//! of the document they will sign (so a provider that signs, a qualified trust service, signs that digest). It
//! answers with somewhere to send the signer (`Start::redirect`), then, on its return, with an [`Outcome`]: who it
//! found, at what level of assurance, its own reference for the check, and any signature it made over the digest.
//! Sign keeps the outcome in the audit trail (an "Identity Verified" event naming the provider, the level and the
//! reference) and on the certificate, never the provider's raw evidence beyond what [`Outcome::evidence`] holds.
//!
//! What one would implement: an Aadhaar eSign provider (through an ASP and an ESP: start = the ESP's signing form
//! with the digest, finish = its signed response, `signature` = the PKCS#7 it returns, `level` = "Aadhaar eSign");
//! an eIDAS provider over the Cloud Signature Consortium API (start = the authorisation URL, finish = the token
//! exchange and `signatures/signHash` over the digest, `level` = "advanced" or "qualified"); a bank-ID or national
//! eID login (OpenID Connect: start = the authorisation request, finish = the verified ID token, no signature).

use ferrum_kernel::Result;
use serde::{Deserialize, Serialize};

/// What a provider is asked about one signer of one request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Ask {
    pub tenant: uuid::Uuid,
    pub request_number: String,
    pub signer_name: String,
    pub signer_email: String,
    /// The SHA-256 of the document the signer will sign, in hex.
    pub document_sha256: String,
    /// Where the provider sends the signer back to (Sign's own page for this link).
    pub return_to: String,
}

/// Where to send the signer to begin, and the provider's handle for the check.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Start {
    pub redirect: String,
    pub reference: String,
}

/// What the provider found.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Outcome {
    /// The provider's name, as the certificate shows it.
    pub provider: String,
    /// The level of assurance in the provider's own words ("qualified", "Aadhaar eSign", "substantial").
    pub level: String,
    /// Who it found, as it names them.
    pub subject: String,
    /// The provider's reference for this check, for its own records and an auditor's.
    pub reference: String,
    /// A signature it made over the document's digest, when it signs (base64 of its PKCS#7 or CMS), else none.
    pub signature: Option<String>,
    /// What else it returned that the trail should keep (at most a few kilobytes).
    pub evidence: serde_json::Value,
}

/// A national electronic identity scheme or a qualified trust service.
#[async_trait::async_trait]
pub trait IdentityProvider: Send + Sync {
    /// A short stable id (`aadhaar-esign`, `csc-qtsp`).
    fn id(&self) -> &str;
    /// Begins the check for `ask`.
    async fn start(&self, ask: &Ask) -> Result<Start>;
    /// Ends it with what the signer's return carried (its query or form), for the check `start` began.
    async fn finish(&self, ask: &Ask, start: &Start, returned: &serde_json::Value) -> Result<Outcome>;
}

/// How an outcome reads in the audit trail's "Identity Verified" event and on the certificate.
pub fn trail_details(o: &Outcome) -> String {
    let signed = if o.signature.is_some() { "; the provider signed the document's digest" } else { "" };
    format!("{} ({}): {} · reference {}{signed}", o.provider, o.level, o.subject, o.reference)
}

#[cfg(test)]
mod tests {
    #[test]
    fn an_outcome_reads_plainly_in_the_trail() {
        let o = super::Outcome {
            provider: "Example eID".into(),
            level: "substantial".into(),
            subject: "Jane Doe".into(),
            reference: "abc-123".into(),
            signature: None,
            evidence: serde_json::Value::Null,
        };
        assert_eq!(super::trail_details(&o), "Example eID (substantial): Jane Doe · reference abc-123");
    }
}
