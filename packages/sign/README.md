# sign — electronic signatures

Package `sign` (depends on `core`). Send a PDF to one or more signers, in order or in parallel,
with fields placed per signer; each signer signs through a personal link; when the last one
signs, the values are stamped onto the PDF, an audit certificate page is appended and the sealed
PDF is kept with its SHA-256 and a hash-chained audit trail.

**What this is:** an electronic signature with an audit trail (intent and consent captured,
signers identified by personal links and optional access codes, every step recorded and
tamper-evident). **What it is not:** a PAdES / advanced or qualified digital signature — no
certificate-based cryptographic signature is embedded in the PDF. Integrity is shown by SHA-256
values on the certificate and in the record, which `verify` recomputes.

Times are RFC 3339 UTC with milliseconds (`2026-10-29T14:03:12.000Z`). Errors use the platform's
shape `{"error", "message", "fields"?}`: `not_found` (404), `forbidden` (403), `unauthorized`
(401), `invalid` / `validation_failed` (422), `conflict` (409), `too_many_requests` (429),
`unavailable` (503); the signing endpoints add `gone` (410, below).

## Roles

| Role | Can |
|---|---|
| Sign User | create, edit (drafts), send and manage **their own** requests; read their own requests' audit trails and files; read templates and settings; use templates |
| Sign Manager | every request and audit trail; templates; Sign Settings (including rotating the signing key) |

Administrators can do everything. Signers need no account.

## Setup

1. The server needs `FERRUM_SECRET_KEY`: the signing key is a secret field.
2. **Sign Settings** (`SignSettings`, one per workspace):

| Field | Default | Meaning |
|---|---|---|
| `sender_name` | the workspace name | the organization signers see in emails, on the signing page and the certificate |
| `public_url` | the Mail app's `public_url` | e.g. `https://erp.example.com`: the base of emailed links (required when Mail is installed) |
| `link_expiry_days` | 30 (1–365) | a request without `expires_on` expires this many days after it is sent |
| `reminder_every_days` | 3 (0–60) | signers whose turn it is are reminded after this many days without signing, then at the same interval; 0 = off |
| `max_reminders` | 3 (0–20) | automatic reminders per signer at most |
| `signing_key` | created on first use | secret (write-only, never shown): the HMAC key of signer links and the salt of IP hashes |

Action `POST /api/v1/SignSettings/{id}/actions/rotate_signing_key` → `{rotated: true}` replaces
the key: **every outstanding link stops working**; run `resend` on open requests afterwards.

## Entities

### Sign Request (`SignRequest`, numbered `SIGN-{YYYY}-{seq}`)

| Field | |
|---|---|
| `title` (required), `message` | shown to signers (email, signing page) |
| `status` | read-only: `Draft` → `Sent` → `In Progress` → `Completed`, or `Declined` / `Expired` / `Voided` |
| `document` (required) | the PDF: upload it with `POST /api/files` and set its `/files/<id>` path. Only real PDFs are accepted (`%PDF-` header and a full parse; not encrypted; ≤ 25 MB, 1–1000 pages). The file must be your own upload or a file of a record you can read; it is attached to the request (or copied when it belongs to something else) |
| `document_name`, `document_sha256`, `page_count`, `page_sizes` | read-only, recorded from the PDF: `page_sizes` is `[[width, height], ...]` in points as displayed (rotation applied) |
| `signing_mode` | `Sequential` (default) or `Parallel` |
| `expires_on` | last day to sign, in the workspace's time zone; empty = `link_expiry_days` after sending (or the template's `expiry_days`) |
| `expires_at` | read-only: the instant links stop working — the end of `expires_on` in the workspace's IANA zone (DST-correct) |
| `reference_entity`, `reference_id` | optional: the record the request is about (e.g. `Quotation` + its id); you must be able to read it |
| `template` | read-only: the template it was made from |
| `sender_email`, `sent_at`, `completed_at`, `void_reason` | read-only |
| `sealed_document`, `sealed_sha256` | read-only: the sealed PDF (`/files/<id>`) and its SHA-256 |
| `seal_error` | read-only: why sealing last failed (it is retried) |
| `signers` | table of **Signer** rows |
| `fields` | table of **Sign Field** rows |

**Signer** (`SignRequestSigner`): `role` (required; unique in the request, case-insensitive),
`name`, `email` (both required to send), `signing_order` (default 1: sequential requests go to
lower numbers first, equal numbers sign in parallel), `access_code` (write-only input, 4–64
characters: kept only as an Argon2id hash in `access_code_hash`; tell it to the signer separately),
and read-only `status` (`Waiting` → `Sent` → `Viewed` → `Signed` | `Declined`), `sent_at`,
`viewed_at`, `signed_at`, `declined_at`, `decline_reason`, `ip_hash` (salted hash of the address
they signed or declined from), `user_agent`, `signature_image` / `signature_text`,
`initials_image` / `initials_text`, `reminders_sent`, `last_reminded_at`, `failed_attempts`,
`locked_until`, `submission_hash`.

**Sign Field** (`SignField`, also used by templates): `role` (the signer who fills it),
`field_type` — `Signature`, `Initials`, `Date Signed` (the signing date, workspace time), `Name`
(the signer's name), `Text`, `Checkbox` — `label`, `page` (from 1), `x`, `y` (points — 1/72 inch —
from the **top-left corner of the page as displayed**), `width`, `height` (≥ 4 points), `required`
(default true), and read-only `value` (what was filled: the text, `Yes`/`No`, the date, the name,
`Drawn signature` or the typed signature). A field must name an existing role and lie inside its
page: saving refuses anything else.

Drafts can be edited and deleted. Once sent a request cannot change, except **corrections**: the
`name` and `email` of signers who have not signed or declined (a `Corrected` event; a corrected
email retires the old link and, when it is that signer's turn, the new one goes out — emailed,
or in a notification to whoever corrected it). Requests that were sent cannot be deleted; void
them.

### Sign Template (`SignTemplate`)

`template_name` (unique), `title`, `message`, `document` (PDF, as above), `signing_mode`,
`expiry_days`, `roles` (**Sign Template Role**: `role`, `signing_order`, `default_name`,
`default_email`), `fields` (Sign Field rows naming template roles), `disabled`.

### Sign Event (`SignEvent`) — the audit trail

Read-only for everyone: written only by the engine, refused on insert/update/delete through the
API (also for administrators), and a database trigger refuses updates. Each event: `request`,
`seq` (1, 2, 3… per request), `event` (`Sent`, `Viewed`, `Signed`, `Declined`, `Reminded`,
`Resent`, `Corrected`, `Access Denied`, `Voided`, `Expired`, `Completed`), `occurred_at`, `role`,
`signer_name`, `signer_email`, `actor` (the signer's email, a user's email, or `system`),
`ip_hash`, `user_agent`, `details`, `sender`, `prev_hash` and `event_hash` =
SHA-256(`prev_hash` + "\n" + JSON of `[request, seq, event, occurred_at, role, signer_name,
signer_email, actor, ip_hash, user_agent, details]`). Senders read their own requests' trails.

Once a request is sent its original PDF belongs to its first `Sent` event, and the sealed PDF to
its `Completed` event: they download through `/files/{id}` for whoever may read the trail, and
nobody but an administrator can delete them (the sender included).

## Actions — `POST /api/v1/SignRequest/{id}/actions/{action}`

| Action | Params | Result | Refused when |
|---|---|---|---|
| `send` | — | `{status: "Sent", expires_at, signers: [{role, name, email, emailed, url?}]}`: the signers whose turn it is (`url` only when not emailed) | not a draft; no PDF or signers; a signer without name, email or a required `Signature` field; fields outside their pages; `expires_on` in the past; Mail installed but no public URL |
| `remind` | — | `{reminded: [...]}` as above, for everyone whose turn it is | not out for signature; expired; nobody waiting |
| `resend` | `signer?`: row id, role or email | `{resent: [...]}`: the invitation again (the same link) | as `remind`; the named signer cannot sign now |
| `void` | `reason?` (≤ 1000 characters) | `{status: "Voided"}`; with Mail, signers who had the link are told | not `Sent` / `In Progress` |
| `verify` | — | `{valid, status, original: {ok, sha256, expected}, sealed: {ok, sha256, expected, readable} \| null, audit_trail: {ok, events, head, problem}}` | — (a draft has nothing to verify) |

`verify` re-reads the stored original and sealed PDFs and recomputes their SHA-256, checks the
sealed PDF still parses, and recomputes the audit trail's chain (numbers without gaps, each event
linked to the previous one, each hash matching its content, one `Signed` event per signature, a
completed request ending with `Completed`). Any difference makes `valid` false and says where.

Without Mail, `send`, `remind`, `resend` and corrections also send the acting user an in-app
notification listing the links (the generic app shows notifications, not action results).

`POST /api/v1/SignTemplate/{id}/actions/create_request` (read access to the template, create
access to requests) with optional `{title, message, signing_mode, expires_on, reference_entity,
reference_id, signers: [{role, name, email, access_code}]}` → `{entity: "SignRequest", id,
number}`: a draft with one signer per template role (given values, else the role's defaults), the
template's fields, and its own copy of the PDF. Refused for a disabled template, unknown or
repeated roles, or a linked record the caller cannot read.

## Signing endpoints — `/api/site/{tenant}/sign/...` (public)

Signers are authenticated by the token in their link
(`{public_url}/api/site/{tenant}/sign/s/{token}`): 43 URL-safe characters — the signer row's id
and a 128-bit HMAC-SHA256 tag over the workspace, the request, the signer and the signer's email,
under the signing key. Tags are compared in constant time. A signer with an access code sends it
in the `X-Access-Code` header on every call; after 5 wrong codes the link is locked for 15
minutes. Responses carry `Cache-Control: no-store`, `Referrer-Policy: no-referrer`,
`X-Content-Type-Options: nosniff` and `X-Robots-Tag: noindex`.

A malformed, forged, tampered, retired (email corrected, key rotated) or other workspace's token
gets **404** — the same answer as an unknown one. A genuine link that cannot be used any more gets
**410** `{error: "gone", message, request_status, signer_status}`: the request is completed,
declined, voided or expired (also before maintenance marks it), or this signer already signed or
declined (links are single-use). In sequential mode only the signers whose turn it is can use
their link: the others get **403**.

| Method & path | Request | Response |
|---|---|---|
| `GET /s/{token}` | — | the signing page: self-contained HTML (no outside resources; CSP with a per-response nonce) that drives the endpoints below — PDF preview, the signer's fields, signature pad (draw or type), consent, sign, decline |
| `GET /signing/{token}` | — | `{request: {number, title, message, status, signing_mode, expires_at, document_name, document_sha256, pages: [{page, width, height}], sender: {organization, name, email}}, signer: {role, name, email, status}, fields: [{id, field_type, label, page, x, y, width, height, required}], needs: {signature, initials}, consent}` — only this signer's fields. The first access records `Viewed` |
| `GET /signing/{token}/document` | — | the original PDF (`application/pdf`), served only while its SHA-256 still matches (otherwise 503) |
| `POST /signing/{token}/sign` | `{consent: true, signature: {image: "data:image/png;base64,..."} \| {text}, initials?: {image} \| {text}, values?: {"<field id>": "text" \| true \| false}}` (≤ 1 MB) | `{status: "Signed", request_status, replayed}`; `request_status` is `Completed` when this signature sealed the document |
| `POST /signing/{token}/decline` | `{reason}` (1–1000 characters) | `{status: "Declined", request_status: "Declined", replayed}`; the request ends for everyone and the sender is notified |
| `POST /verify` | the PDF as the body (≤ 25 MB) | `{valid: true, sha256, request: {number, title, completed_at, signers: [{name, role, signed_at}]}}` when it is exactly a document sealed here whose stored copy still matches, else `{valid: false, sha256, message}` |

Signing rules: `consent` must be `true` (the signer agrees to sign electronically). A drawn
signature or initials is a PNG data URL of 20×10 to 3000×1500 pixels and at most 300 KB that
shows some ink (a blank canvas is refused); a typed one is 1–100 characters with a letter.
Initials default to the initials of the typed signature or of the name. `values` may only name
the signer's own `Text` and `Checkbox` fields; required text must be given and fit its box (at
4 points or more), required checkboxes must be `true`; text is at most 500 characters. Name and
date fields fill themselves. Each error comes back as `validation_failed` with the field.

**Idempotent and concurrent:** every call locks the request; a repeated identical submission
(the same body — e.g. a retry after a lost answer) returns the first answer with `replayed: true`
and changes nothing; a different submission after signing is a `conflict`. Signers of a parallel
request may sign at the same moment: each is recorded once and the document is sealed once.

## Sealing

When the last signer signs the request is sealed at once (and, if that fails, retried by
maintenance, the error kept in `seal_error` and the sender told once):

1. The stored original is re-hashed: a changed original is never sealed.
2. Every field's value is stamped onto its page inside its box: drawn marks as image XObjects
   (with their transparency), typed signatures in Helvetica-Oblique, text, names and dates in
   Helvetica shrunk to fit, a tick for checked boxes. The page's own content is wrapped so it
   cannot disturb the stamps; rotated pages are stamped upright.
3. The stamped document is written, then the **certificate** is appended as an incremental PDF
   update, so the signed document is a byte-exact prefix of the sealed file. The certificate
   lists the request (number, title, sender, sent, completed, order, document), the SHA-256 of the
   original, of the signed document (with its length: anyone can check it with
   `head -c <length> sealed.pdf | sha256sum`) and the audit trail's last hash, every signer
   (signed at, drawn or typed, access code, IP hash, device) and every event, and a statement that
   this is an electronic signature with an audit trail, not a PAdES signature.
4. The sealed PDF (`<title> - signed.pdf`) is stored and attached to the `Completed` event
   (details: its SHA-256, page count, and the signed document's hash and length); the request
   gets `sealed_document`, `sealed_sha256`, `completed_at`, status `Completed`. The sender gets a
   notification; with Mail, every signer gets an email with the sealed PDF attached.

Text uses the PDF standard fonts with WinAnsi encoding: Latin-1 and Windows-1252 characters show
as typed, Latin Extended-A letters (Polish, Czech, Turkish, Hungarian, Romanian…) as their base
letters, other scripts as `?`. Signatures drawn on the pad are images and show in any script.

## Mail

With the Mail app (`comms`) installed, emails are `Communication` records on the request's
timeline, sent and retried by Mail: invitations (when a signer's turn comes), reminders,
withdrawal notices on void, and the sealed copy on completion. Emailed links need a public URL
(Sign Settings, else Mail's). Without Mail, nothing is emailed: actions return the links, the
acting user and the sender get in-app notifications with them (the sender also when a signer's
turn comes or a reminder is due), and the sender shares them.

Emails are stored by Mail like any other: Mail managers can read sent mail, links included. For
sensitive documents give signers an access code, told to them separately; it is never emailed.

## Reports

All count only requests the caller may read (a Sign User sees their own).

| Report | Parameters | Rows |
|---|---|---|
| `sign_requests_by_status` | `from_date`, `to_date` (created; default the fiscal year to date) | every status with `requests` and `share` %, total |
| `sign_completion_time` | `from_date`, `to_date` (completed), `group_by`: `Month` / `Sender` / `Template` | `completed`, `average_hours`, `median_hours`, `fastest_hours`, `slowest_hours` from sending to completion, totals |
| `sign_pending_signatures` | — | per signer email whose turn it is on open requests: `name`, `pending`, `waiting_since`, `days_waiting`, `next_expiry` |

## Maintenance

Every minute (`maintenance`, one replica at a time), per workspace with the app; each item is
re-checked under its request's row lock and done in one transaction with its event, so it happens
once however often maintenance runs:

* **Expiry** — open requests past `expires_at` (unless everyone signed) become `Expired` with an
  `Expired` event; the sender is notified.
* **Reminders** — signers whose turn it is and who were sent their link (or last reminded)
  `reminder_every_days` ago get a reminder, at most `max_reminders`: emailed with Mail, otherwise
  the sender is notified with the link. Each reminder is counted and recorded (`Reminded`) once.
  Reminding by hand pushes the next automatic one back.
* **Sealing retries** — fully signed requests not sealed two minutes after their last change.
