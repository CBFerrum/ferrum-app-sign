# Ferrum Sign

A native app for [Ferrum](https://github.com/CBFerrum/Ferrum), the open-source, AI-first ERP.

- Sign requests for any PDF — an upload or a reusable template — numbered and linkable to any record (a quotation, a contract)
- Signers in a sequential routing order (equal orders sign in parallel) or all at once; fields per signer role — signature, initials, date signed, name, text, checkbox — placed in PDF points on any page, rotated pages included, and refused when outside it
- Personal links: HMAC tokens under a key kept in Sign Settings, expiring with the request, single-use, checked in constant time; optional access codes (Argon2id hashes) with lockout
- A self-contained signing page and JSON endpoints: view the PDF with your own fields only, sign with a drawn (PNG) or typed signature, or decline with a reason; double submits and concurrent signers are safe
- Sealing: every value stamped onto the original pages and an audit certificate appended as an incremental update, so the signed document is a byte-exact, checkable prefix of the sealed file; the sealed PDF and its SHA-256 are kept, the sender is notified and every signer gets a copy by email
- An append-only, SHA-256 hash-chained audit trail — sent, viewed, signed, declined, reminded, resent, corrected, access denied, voided, expired, completed — with salted IP hashes and user agents
- Verify Integrity recomputes the original's, the sealed PDF's and the audit trail's hashes; a public endpoint tells anyone whether a PDF they hold is a genuine sealed document
- Emails through the Mail app when it is installed, otherwise through the server's mailer (the signed PDF travels with the completion email when the mailer can attach files); copyable links and in-app notifications when no email can go out. Automatic reminders on a cadence (each sent once), expiry, void, corrections of a signer's name or email, resending
- Reports: requests by status, time to complete, pending signatures by signer — each sender sees their own
- This is an **electronic signature with an audit trail, not a PAdES digital signature**: no certificate-based cryptographic signature is embedded in the PDF; integrity rests on the SHA-256 values and the audit trail
- `packages/sign/README.md` documents every entity, action, endpoint, setting and maintenance job

## Install

Workspace administrators install it from **Apps** in Ferrum; it ships in the official Ferrum
server image. Self-hosters building their own image enable the `sign` feature of the server.
Everything it adds is also available to AI agents over Ferrum's MCP server.

## Develop

Clone it next to the platform and point the platform dependencies at your checkout (this file
is git-ignored):

```toml
# .cargo/config.toml
[patch."https://github.com/CBFerrum/Ferrum"]
ferrum-kernel = { path = "../ferrum/crates/kernel" }
ferrum-meta = { path = "../ferrum/crates/meta" }
ferrum-domain = { path = "../ferrum/crates/domain" }
ferrum-calc = { path = "../ferrum/crates/calc" }

# The tests send mail through the real Mail app.
[patch."https://github.com/CBFerrum/ferrum-app-comms"]
ferrum-app-comms = { path = "../ferrum-app-comms" }
```

The tests need PostgreSQL with an ordinary (non-superuser) role — row-level security must apply:

```bash
createuser ferrum --pwprompt && createdb -O ferrum ferrum_sign_test
```

```bash
cargo test
```

`FERRUM_SIGN_TEST_DATABASE_URL` overrides the database URL. The app's metadata lives in `packages/` and is embedded
at build time; `app.yaml` is its manifest (see Ferrum's `docs/plan/apps.md`).

## License

AGPL-3.0-only, like the platform.
