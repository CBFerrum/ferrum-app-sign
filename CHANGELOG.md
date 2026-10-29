# Changelog

## Unreleased

- Emails no longer need the Mail app: without it, invitations, reminders and withdrawals go
  through the server's mailer, and the completion email carries the signed PDF when the mailer
  can attach files.

## 0.1.0

- First release. Sign requests send a PDF — uploaded or from a reusable template, optionally linked to any record — to signers in a sequential routing order or in parallel, with signature, initials, date, name, text and checkbox fields placed per signer role and checked against the page geometry (rotation included); only real PDFs are accepted. Signers act through a self-contained signing page and public endpoints authenticated by HMAC link tokens (key in Sign Settings, constant-time checks, expiring, single-use, sequential turns enforced) and optional Argon2id access codes with lockout; they view the PDF with their own fields, sign with a validated drawn PNG or a typed signature, or decline, idempotently and safely under concurrency. The last signature seals the document: values stamped onto the original pages, an audit certificate appended as an incremental update (so the signed document is a verifiable byte prefix), the sealed PDF stored with its SHA-256, the sender notified and signers emailed a copy through the Mail app (copyable links and in-app notifications without it). An append-only, SHA-256 hash-chained audit trail records every step with salted IP hashes; `verify` recomputes the document and trail hashes and a public endpoint checks presented PDFs. Maintenance expires requests, sends reminders on a cadence (each once) and retries sealing; reports cover requests by status, time to complete and pending signatures by signer. It is an electronic signature with an audit trail, not a PAdES digital signature.
