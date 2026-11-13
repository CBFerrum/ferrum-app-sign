<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/assets/banner-dark.svg">
  <img alt="Ferrum Sign: send documents for electronic signature from your own server" src="docs/assets/banner-light.svg" width="100%">
</picture>

<br>

[![Version](https://img.shields.io/badge/version-0.3.1-4f46e5?style=flat-square)](CHANGELOG.md)
[![Licence: AGPL-3.0](https://img.shields.io/badge/licence-AGPL--3.0-blue?style=flat-square)](LICENSE)
[![Ferrum platform](https://img.shields.io/badge/runs%20on-Ferrum-2f5bea?style=flat-square)](https://github.com/CBFerrum/Ferrum)
[![CI](https://github.com/CBFerrum/ferrum-app-sign/actions/workflows/ci.yml/badge.svg)](https://github.com/CBFerrum/ferrum-app-sign/actions/workflows/ci.yml)
![Rust](https://img.shields.io/badge/Rust-server-b7410e?style=flat-square&logo=rust)
![PostgreSQL](https://img.shields.io/badge/PostgreSQL-data-336791?style=flat-square&logo=postgresql&logoColor=white)
![React](https://img.shields.io/badge/React%20%2B%20TypeScript-interface-0ea5e9?style=flat-square&logo=react&logoColor=white)
![Tests](https://img.shields.io/badge/tests-19%20Rust%20%C2%B7%20107%20interface-16a34a?style=flat-square)

### Send a PDF for signature, follow it, and keep a sealed copy with its evidence.

Ferrum Sign is for teams that send contracts, offers, forms and approvals to be signed and want to do it from their own server instead of paying per envelope. It replaces DocuSign, Adobe Acrobat Sign, Dropbox Sign and Zoho Sign for the common case, an electronic signature with a strong audit trail, and it links every request to the record it is about.

> [!IMPORTANT]
> Sign makes an **electronic signature with an audit trail**. It is not a PAdES, advanced or qualified digital signature, and no certificate-based signature is embedded in the PDF. The signing page says so. If your law or customer requires a qualified signature, do not use Sign for that document.

[💡 Why](#-why-ferrum-sign) · [📸 Screenshots](#-screenshots) · [✨ Features](#-features) · [📊 Compare](#-how-it-compares) · [📍 Roadmap](#-roadmap) · [🚀 Install](#-install) · [🔧 Develop](#-develop)

</div>

## 💡 Why Ferrum Sign

- 🏠 **Self-hosted, open source, AGPL-3.0.** The documents, the signatures and the evidence stay on your servers. No per-envelope or per-seat fee.
- 🔗 **Signing from any record.** A request can be about a quotation, an offer or a contract, and can be sent the moment the record's button is pressed. Other apps, such as Contracts, follow the result.
- 🧾 **Evidence anyone can check.** The signed document is a byte-exact prefix of the sealed file, so anyone can verify it with `head -c <length> sealed.pdf | sha256sum`, without trusting the server. Every step is in a hash-chained audit trail.
- 📱 **Signers need no account.** They open a personal, expiring, single-use link on any device, with an optional one-time code, and sign by drawing or typing.
- 🛣️ **Real routing.** Sequential, parallel or equal-order signers, copy-only recipients, signing groups where any one signs, and signers who can hand over to a colleague.
- 🤖 **For people and AI agents.** Requests, templates and the audit trail are ordinary Ferrum records, so they are in the REST and MCP APIs and in events other apps and automations can use.
- 🌍 **Many languages.** The interface and the signing page come in English, Arabic and Hindi and mirror for right to left.

## 🎯 At a glance

| | |
|---|---|
| 📦 **Version** | 0.3.1 |
| 🖥️ **Kind** | Its own interface at `/sign/` and a page for signers at `/sign/s/{workspace}/{token}`, with Ferrum's records screens for the rest |
| ➕ **What it adds** | 7 entities in one package, 2 roles, 3 reports, a launcher app, a Create menu entry, one message template, 4 events other apps can follow, an assistant |
| 🧱 **Depends on** | The platform's `core` package. Nothing else is required |
| 🤝 **Works with** | Mail (emails are Communications on the request, sent and retried by Mail), Contracts (follows a signature), any record a request is about |
| 🌍 **Languages** | English, Arabic, Hindi |
| 🧪 **Tests** | 19 Rust test functions and 107 interface tests, counted from the source |
| 📄 **Licence** | AGPL-3.0-only |

## 📸 Screenshots

<picture><source media="(prefers-color-scheme: dark)" srcset="docs/assets/screens/sign-sign-request-detail-dark.webp"><img alt="Sign request — open record" src="docs/assets/screens/sign-sign-request-detail-light.webp" width="100%"></picture>
<p align="center"><b>📝 Sign request — open record</b><br><sub>Read and edit one sign request, with its history and related records.</sub></p>

<table>
<tr>
<td width="50%" valign="top">
<picture><source media="(prefers-color-scheme: dark)" srcset="docs/assets/screens/sign-templates-dark.webp"><img alt="Sign — templates" src="docs/assets/screens/sign-templates-light.webp"></picture>
<p><b>📝 Sign — templates</b><br><sub>Keep documents you send for signature again and again as templates.</sub></p>
</td>
<td width="50%" valign="top">
<picture><source media="(prefers-color-scheme: dark)" srcset="docs/assets/screens/sign-new-request-dark.webp"><img alt="Sign — new request" src="docs/assets/screens/sign-new-request-light.webp"></picture>
<p><b>📝 Sign — new request</b><br><sub>Upload a document, add who must sign and send it.</sub></p>
</td>
</tr>
<tr>
<td width="50%" valign="top">
<picture><source media="(prefers-color-scheme: dark)" srcset="docs/assets/screens/sign-sign-request-list-dark.webp"><img alt="Sign requests — list" src="docs/assets/screens/sign-sign-request-list-light.webp"></picture>
<p><b>📝 Sign requests — list</b><br><sub>Browse, search and filter sign requests.</sub></p>
</td>
<td width="50%" valign="top">
<picture><source media="(prefers-color-scheme: dark)" srcset="docs/assets/screens/sign-requests-dark.webp"><img alt="Sign — requests" src="docs/assets/screens/sign-requests-light.webp"></picture>
<p><b>📝 Sign — requests</b><br><sub>See documents waiting for signatures and those already signed.</sub></p>
</td>
</tr>
<tr>
<td width="50%" valign="top">
<picture><source media="(prefers-color-scheme: dark)" srcset="docs/assets/screens/sign-report-sign-requests-by-status-dark.webp"><img alt="Requests by status — report" src="docs/assets/screens/sign-report-sign-requests-by-status-light.webp"></picture>
<p><b>📝 Requests by status — report</b><br><sub>Run requests by status and read the result.</sub></p>
</td>
<td width="50%"></td>
</tr>
</table>

### 📱 On a phone

<p align="center">
<picture><source media="(prefers-color-scheme: dark)" srcset="docs/assets/screens/sign-requests-phone-dark.webp"><img alt="Sign — requests" src="docs/assets/screens/sign-requests-phone-light.webp" width="260"></picture>
<picture><source media="(prefers-color-scheme: dark)" srcset="docs/assets/screens/sign-sign-request-list-phone-dark.webp"><img alt="Sign requests — list" src="docs/assets/screens/sign-sign-request-list-phone-light.webp" width="260"></picture>
</p>
<p align="center"><sub>See documents waiting for signatures and those already signed. · Browse, search and filter sign requests.</sub></p>

> [!TIP]
> The screenshots follow your GitHub theme: switch between light and dark to see both. The signer's own page, which opens from an emailed link, is built for phones: the next step is at the thumb.

## ✨ Features

### 📨 Sending a request

- **Any real PDF.** Upload a PDF or start from a template. Only real PDFs are accepted: the file must parse, must not be encrypted, and is at most 25 MB and 1 to 1,000 pages. A page generated as HTML by a server with no PDF service is refused with a message naming the missing service, so a request is never left stuck.
- **A few steps.** Who signs and in what order, where each of them signs, and a last look that says in words what is still missing.
- **Fields placed on the real pages.** Drag Signature, Initials, Date Signed, Name, Text and Checkbox fields from a palette, or add, move, resize with the arrow keys, copy and delete them from the keyboard. One colour per signer, undo, zoom and exact numbers for the selected box. Fields are checked against the page geometry, rotation included, and a field must name an existing signer role and lie inside its page.
- **Several signers.** Each has a role (unique in the request), name and email, and a routing order. Sequential requests go to the lower numbers first; equal numbers sign in parallel.
- **Recipients who do not sign.** A signer's action can be "Receive a Copy": they get the completed document but do not sign, and do not hold anything up.
- **Signing groups.** Put several signers in a group so that any one of them signs and the others become "Not Needed".
- **A message and an end date.** The message is shown in the email and on the signing page. A request expires on its end date (in the workspace's time zone, correct across daylight saving) or after the workspace's default number of days.
- **Linked to a record.** A request can be about any record you can read, such as a quotation, and shows on that record.
- **Signature page.** When a request has no fields, a signature page is added at the end.
- **Sign from any record.** A button on a record can generate a document and send it for signature in one step (`send_on_create`, `send_for_signature`). If the server has no PDF service, the request is refused whole in the button's own transaction, so the record keeps its status and nothing is left behind. A request that still cannot be sent at upkeep tells its sender and raises the event `sign.request_failed`.
- **Reminders per request.** Reminder cadence and the maximum number can be set on a request; Sign Settings holds the workspace's cadence and maximum.

### 🧩 Templates

- **Reusable documents.** A template is a PDF with signer roles (with default names and emails), fields placed per role, a message, a routing mode and an expiry. Templates have the same placement screen as requests.
- **Use a template.** "Use" asks who signs this time and makes a draft request with one signer per role, the template's fields and its own copy of the PDF. A disabled template is refused.
- **Managed by Sign Managers.** Sign Users may read and use templates; Sign Managers keep them.

### ✍️ Signing

- **A page made for signers.** Opened from a personal link with no account at `/sign/s/{workspace}/{token}`. It shows only the pages that have something for them, with their boxes in place, and one button walks them through what is left: sign, add initials, fill in, tick, then read the agreement and finish. It works on a phone with the next step at the thumb.
- **Draw or type.** A drawn signature or initials must show some ink; a typed one is 1 to 100 characters. Initials default to those of the typed signature or the name. Name and date fields fill themselves.
- **Consent is explicit.** Signing needs the signer's agreement to sign electronically.
- **Decline with a reason.** The request ends for everyone and the sender is told.
- **Reassign.** When the sender allows it, a signer can hand over to a colleague (an event is recorded).
- **Plain answers when a link cannot be used.** The page says plainly when a link is spent, withdrawn, expired or not yet the signer's turn.
- **The kind of signature, said openly.** The page and the certificate say that this is an electronic signature with an audit trail, not a PAdES, advanced or qualified one, and how each signer was checked.
- **Real pages.** Every page of the document is drawn in the browser (with pdf.js, loaded only when a document opens), so signers read each page where they sign.
- **Safe and repeatable.** Each submission locks the request; a repeated identical submission (a retry after a lost answer) returns the first answer and changes nothing; signers of a parallel request may sign at the same moment and the document is sealed once.

### 🔐 Identity

- **Link security.** Personal links carry a 128-bit HMAC tag under a key kept as a sealed secret, are compared in constant time, expire, and are single-use. A forged, tampered or retired link gets the same answer as an unknown one. Responses are marked not to be cached, indexed or leaked through referrers.
- **Access code.** An optional code the sender tells the signer separately. It is stored only as an Argon2id hash and never emailed. Five wrong tries lock the link for 15 minutes.
- **One-time codes.** A request, or a single signer, can require a code by email or by text message. The code works once, for ten minutes, a sixth try is refused, and signing is refused until it is confirmed. The certificate says how each signer was checked. Text codes need the platform's text-message channel.
- **Identity providers.** An interface (`identity::IdentityProvider`) lets a developer add a stronger check, such as an ID document check. No provider ships yet.

### 🔎 Following a request

- **By where it stands.** Every request listed as in progress, drafts, completed or closed, with who is holding it up.
- **Each signer's progress** on the request's page: Waiting, Sent, Viewed, Signed, Declined or Not Needed, with times and reminders.
- **Actions.** Remind, send the links again (with links to copy when the workspace sends no email), correct a signer's name or email (before they sign or decline; a corrected email retires the old link), and void with a reason (signers who had the link are told).
- **The audit trail in words.** Each event told as a sentence.
- **Verify.** One click re-reads the stored original and sealed PDFs, recomputes their hashes, checks the sealed file still parses and walks the whole audit chain. It says which file or entry no longer matches.
- **The signed copy** is downloaded from the request.
- **Numbered.** Requests are numbered `SIGN-{year}-{sequence}`.

### 🧾 Sealing and evidence

- **Sealed the moment the last person signs** (and retried by the upkeep job if sealing fails, with the error kept and the sender told once).
- **Stamped onto the original pages.** Drawn marks as images with their transparency, typed signatures in Helvetica-Oblique, text, names and dates in Helvetica shrunk to fit, a tick for checked boxes. The page's own content is wrapped so it cannot disturb the stamps, and rotated pages are stamped upright. A changed original is never sealed.
- **A certificate page** is appended as an incremental PDF update, so the signed document is a byte-exact prefix of the sealed file. It lists the request (number, title, sender, sent and completed times, order, document), the SHA-256 of the original and of the signed document with its length, the audit trail's last hash, every signer (signed at, drawn or typed, how they were checked, a salted address hash, device) and every event.
- **A hash-chained audit trail.** Sent, Viewed, Signed, Declined, Reminded, Resent, Corrected, Access Denied, Voided, Expired, Completed, Code Sent, Identity Verified, Reassigned and Not Needed. Each event is chained to the previous one by SHA-256, is read-only for everyone (a database trigger refuses updates), and the signer's address is kept only as a salted hash.
- **Public verification.** A public endpoint checks a presented PDF: it answers whether it is exactly a document sealed here whose stored copy still matches, with its request number and signers.
- **Evidence is kept.** The original belongs to its first Sent event and the sealed PDF to its Completed event. Only an administrator can delete them, and a personal-data erasure keeps them, because they are the evidence of a signature.
- **A note on text.** Stamped text uses the PDF standard fonts: Latin-1 and Windows-1252 characters show as typed, Latin Extended-A letters as their base letters, other scripts as `?`. Signatures drawn on the pad are images and show in any script.

### ✉️ Notices and reminders

- **With the Mail app,** invitations (when a signer's turn comes), reminders, withdrawal notices and the sealed copy are Communications on the request's timeline, sent and retried by Mail.
- **Without Mail,** invitations, reminders and withdrawals go through the server's mailer, and the completion email carries the signed PDF when the mailer can attach files. When the workspace sends no email at all, actions return the links and the sender gets in-app notifications with them.
- **Automatic reminders and expiry.** Every minute, requests past their end become Expired (the sender is told), signers whose turn it is are reminded on the cadence up to the maximum, each once, and stuck sealings are retried. Expiry and sealing count a "Not Needed" group member and a copy-only recipient as done.
- **Events for other apps and automations.** `request_completed`, `request_declined`, `request_expired` and `request_failed`.

### 📊 Reports

| Report | What it shows |
|---|---|
| **Sign Requests By Status** | Every status with the number of requests and its share, in a date range |
| **Sign Completion Time** | Average, median, fastest and slowest time from sending to completion, grouped by month, sender or template |
| **Sign Pending Signatures** | Per signer whose turn it is on open requests: how many, waiting since, days waiting, next expiry |

All reports count only requests the caller may read, so a Sign User sees their own.

### 🛡️ Privacy, permissions and accessibility

- **Roles.** Sign User (sends their own requests, reads their own trails) and Sign Manager (every request, templates and settings including rotating the signing key). Signers need no account.
- **Personal data.** A person's request exports their signing events and the requests they sent, and says why they are kept.
- **Languages and direction.** Arabic and Hindi for the interface, the signing page and the names of records; right-to-left screens mirror.
- **Accessibility.** Fields can be placed and moved by keyboard, controls are named for assistive technology, and an automated lint runs with an empty baseline. A screen-reader audit has not been done yet.

## 🧭 How it works

### A request, from draft to sealed document

```mermaid
flowchart LR
    D["Draft - PDF, signers, fields"] -->|send| S["Sent - links go out"]
    S --> P["In Progress - signers sign in turn"]
    P --> C["Completed - sealed PDF and certificate"]
    S --> X["Declined, Voided or Expired"]
    P --> X
```

A draft becomes Sent when its first signers' links go out and In Progress when someone signs. When the last required signer signs, the request is sealed and becomes Completed. A signer declining ends it as Declined; the sender can void it; it expires on its end date.

### What happens when someone signs

```mermaid
flowchart TD
    L["Signer opens personal link"] --> V{"Link valid and their turn?"}
    V -->|no| G["Gone or not found"]
    V -->|yes| K["Optional code check"]
    K --> F["Fill fields, draw or type, consent"]
    F --> E["Event written to the hash chain"]
    E --> Z{"Last required signer?"}
    Z -->|no| N["Next signers get their links"]
    Z -->|yes| S["Stamp values, append certificate, store sealed PDF"]
    S --> M["Sender notified, signers get the copy"]
```

Every step is one transaction with its audit event, so it happens once however often a retry or the maintenance job runs.

### Inside Ferrum

```mermaid
flowchart LR
    subgraph Browser
        UI["Sign interface - /sign/"]
        SG["Signer page - no account"]
    end
    subgraph Server["Ferrum server"]
        API["REST, MCP, public endpoints"]
        K["Kernel - permissions, RLS, audit"]
        APP["Sign - 7 entities, 2 roles"]
    end
    DB[("PostgreSQL")]
    MAIL["Mail app"]
    UI --> API --> K --> DB
    SG --> API
    K <--> APP
    APP --> MAIL
```

Sign is ordinary Ferrum metadata and Rust, so requests get lists, forms, search, permissions, an audit trail and the REST and MCP APIs. The signer's page talks to a small set of public endpoints authenticated by the signer's link only.

## 🧱 Data model

| Entity | What it is | Key fields and states |
|---|---|---|
| Sign Request | A PDF sent to one or more signers (`SIGN-{year}-{sequence}`) | title, message, document, signing mode, verify identity, reassign, reminders, expiry, record it is about, sealed document and hash. Status: Draft, Sent, In Progress, Completed, Declined, Expired, Voided |
| Sign Request Signer | One signer or recipient of a request | role, name, email, phone, order, action (Sign or Receive a Copy), signing group, verify identity, status (Waiting, Sent, Viewed, Signed, Declined, Not Needed), times, decline reason |
| Sign Field | A box on a page | signer role, type (Signature, Initials, Date Signed, Name, Text, Checkbox), label, page, position and size in points, required, value |
| Sign Template | A reusable PDF with roles and fields | name, title, message, document, signing mode (Sequential or Parallel), expiry days, disabled |
| Sign Template Role | A role in a template | role, order, default name, default email |
| Sign Event | The append-only audit trail | request, sequence, event, time, signer, actor, hashed address, device, details, previous hash, event hash |
| Sign Settings | One per workspace | sender name, public URL, link expiry days, reminder cadence, maximum reminders, signing key (sealed secret) |

### 👥 Roles

| Role | What it can do |
|---|---|
| **Sign User** | Creates, edits (drafts), sends and manages their own requests, reads their own requests' audit trails and files, reads templates and settings and uses templates |
| **Sign Manager** | Every request and audit trail, templates, and Sign Settings including rotating the signing key |

### 🎬 Actions

| Action | What it does |
|---|---|
| Send | Sends a draft: the signers whose turn it is get their links |
| Remind | Reminds everyone whose turn it is |
| Resend | Sends the invitation again (the same link), to one signer or all |
| Void | Withdraws a request, with a reason |
| Verify | Recomputes the document hashes and the audit chain |
| Rotate Signing Key | Replaces the key that signs links; old links stop working |
| Create Request (on a template) | A draft request from a template |

### 🧮 Jobs and messages

- **Every minute (upkeep):** expire requests, send due reminders, retry sealing.
- **Message template:** "Signing code", the email or text that carries a one-time code (it works once, for ten minutes).

## 🔌 Integrations

| With | How | Needs |
|---|---|---|
| Mail (Comms) | Emails are Communications on the request's timeline, sent and retried by Mail, with the sealed copy attached | The Mail app; a public URL in Sign Settings or Mail's |
| Contracts | A contract is sent for signature and follows the result | The Contracts app |
| Any record | `reference_entity` and `reference_id` link a request to a record; a button can send from it | Read access to the record |
| Platform text-message channel | One-time codes by text message | A text channel set up in the platform |
| PDF service | Pages generated by other apps are made into PDFs before they can be signed | `FERRUM_PDF_URL` |
| Webhooks and automations | `request_completed`, `request_declined`, `request_expired`, `request_failed` | Platform webhooks |
| Public verification | Anyone can check a PDF against the stored original | Nothing |

## 📊 How it compares

Sign is compared with DocuSign, Adobe Acrobat Sign, Dropbox Sign, Zoho Sign and Documenso, the open-source option.

| Capability | Ferrum Sign | DocuSign | Acrobat Sign | Dropbox Sign | Zoho Sign | Documenso |
|---|:-:|:-:|:-:|:-:|:-:|:-:|
| Send a PDF for signature to several signers | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| Sequential and parallel routing | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| Signature, initials, date, name, text and checkbox fields | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| Dropdown, radio, number and attachment fields | ❌ | ✅ | ✅ | ✅ | ✅ | ✅ |
| Reusable templates with roles | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| Bulk send to a list | ❌ | ✅ | ✅ | ✅ | ✅ | 🟡 |
| Signers need no account, sign on a phone | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| Draw or type a signature | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| Copy-only recipients | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| Signing groups, any one signs | ✅ | ✅ | ✅ | 🟡 | 🟡 | ❌ |
| Signer hands over to a colleague | ✅ | ✅ | ✅ | 🟡 | ✅ | 🟡 |
| Access code | ✅ | ✅ | ✅ | ✅ | ✅ | 🟡 |
| One-time code by email | ✅ | ✅ | ✅ | ✅ | ✅ | 🟡 |
| One-time code by text message | 🟡 ¹ | ✅ | ✅ | ✅ | ✅ | ❌ |
| ID document and selfie check | ❌ | ✅ | ✅ | 🟡 | ✅ | ❌ |
| Aadhaar eSign | ❌ | 🟡 | 🟡 | ❌ | ✅ | ❌ |
| Advanced or qualified digital signature (PAdES, eIDAS) | ❌ | ✅ | ✅ | 🟡 | ✅ | ✅ |
| Tamper-evident audit trail and certificate | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| Signed copy checkable with a plain hash command | ✅ | ❌ | ❌ | ❌ | ❌ | ❌ |
| Reminders and expiry | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| Void and correct a signer's email | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| Word and Office files as the source | ❌ | ✅ | ✅ | ✅ | ✅ | ❌ |
| Several documents in one request | ❌ | ✅ | ✅ | ✅ | ✅ | 🟡 |
| In-person signing | ❌ | ✅ | ✅ | ✅ | ✅ | ❌ |
| Payments collected at signing | ❌ | ✅ | ✅ | ❌ | 🟡 | ❌ |
| Branding and editable email wording | 🟡 ² | ✅ | ✅ | ✅ | ✅ | ✅ |
| Webhooks and embedded signing | 🟡 ³ | ✅ | ✅ | ✅ | ✅ | ✅ |
| Sign from any business record, result returned to it | ✅ | 🟡 | 🟡 | 🟡 | 🟡 | 🟡 |
| Reports on status, turnaround and pending signers | ✅ | ✅ | ✅ | ✅ | ✅ | 🟡 |
| Native mobile apps | ❌ | ✅ | ✅ | ✅ | ✅ | ❌ |
| Typed signatures and text in any script | ❌ ⁴ | ✅ | ✅ | ✅ | ✅ | ✅ |
| Self-hosted | ✅ | ❌ | ❌ | ❌ | ❌ | ✅ |
| Open source | ✅ | ❌ | ❌ | ❌ | ❌ | ✅ |
| No per-envelope or per-seat fee | ✅ | ❌ | ❌ | ❌ | ❌ | ✅ |

¹ The option is built; it needs the platform's text-message channel, which depends on a provider you set up.
² Only the sender name is a setting today. The invitation, reminder and consent wording are fixed English text.
³ Sign records and their events are in the REST and MCP APIs and in the outbox, so webhooks and automations work. Signing itself is only by link.
⁴ Typed text in other scripts shows as `?` in the sealed PDF. Drawn signatures show in any script.

### 🏆 Where Sign stands out

- 🔍 A signed PDF anyone can check with `head -c` and `sha256sum`, and a public endpoint that confirms a document is one sealed here.
- 🔗 One signature app for every record, with the result returned to it and events for automations.
- 🏠 Open source and self-hosted, with no per-envelope or per-seat fee.
- 🛣️ Routing that covers sequential, parallel, groups, copy-only recipients and reassignment.
- 🔒 Evidence that survives a data-erasure request, with a clear reason.

### 🚧 Where the leaders are still ahead

- 📜 Advanced and qualified signatures, and Aadhaar eSign, through trust providers.
- 🪪 ID document checks and a shipped text-message provider.
- 📎 Word and Office files as the source, several documents in one request, bulk send and more field types.
- 🧑‍🤝‍🧑 In-person signing, payments at signing and embedded signing in other sites.
- 🎨 Branding and editable wording per workspace and template.
- 📱 Native mobile apps.

> [!NOTE]
> Leader columns are from public product information as of November 2026 and may change.

## 📍 Roadmap

### ✅ Recently shipped

- [x] **0.3.1**: A page can no longer start a signature; a request that cannot be sent raises an event; a test for one-time codes.
- [x] **0.3.0**: One-time codes by email or text, copy-only recipients, signing groups, reassigning, per-request reminders, sign from any record, events for completed, declined and expired requests, and the kind of signature on the certificate.
- [x] **0.2.19 to 0.2.18**: The upgraded design system and lighter first page.
- [x] **0.2.17 to 0.2.10**: Names, words and reports in Arabic and Hindi, right to left, times on a person's own clock.
- [x] **0.2.0**: The signer's page, the sending flow with placed fields, following a request, and emails that no longer need the Mail app.
- [x] **0.1.0**: First release: requests, templates, sealed PDFs with a certificate and a hash-chained trail.

### 🔜 Next up

**Signature strength**
- [ ] A real PDF digital signature (PAdES) with timestamps, so Acrobat shows it as valid.
- [ ] Advanced and qualified signatures through a trust service provider, and Aadhaar eSign through a provider.
- [ ] An ID document and selfie check through a provider.

**Preparing documents**
- [ ] Word and Office files as the source, and documents made from a record's fields.
- [ ] More field types (dropdown, radio, number, attachment) and field rules (preset values, validation, conditions).
- [ ] Bulk send from a list.
- [ ] Several documents in one request.

**Evidence and legal**
- [ ] The signer's address and device on the certificate when a setting allows storing them.
- [ ] Consent and notice wording per workspace and region.
- [ ] A retention setting and an evidence bundle (PDF, certificate and trail in one file).

### 🔭 Later

- [ ] In-person signing on one device, and print-sign-upload.
- [ ] Embedded signing in other sites and signed webhooks per event.
- [ ] Branding: logo, colours, and editable invitation, reminder and consent wording per template and language.
- [ ] Payments collected at signing through the Commerce payment connector.
- [ ] Reminders by text and a warning to the sender before a request expires.
- [ ] More reports: signer turnaround, expiring soon, template use.
- [ ] Template folders, sharing and usage counts.
- [ ] Stamped text in more scripts (embedded fonts).
- [ ] A screen-reader audit and a native mobile experience.

## 🚀 Install

Sign is a native app compiled into the official Ferrum server image. A workspace administrator installs it from **Apps** (`/store/`). Self-hosters who build their own image enable the `sign` feature of the server. It needs Ferrum platform `>=0.3, <0.4`.

**First steps**

1. The server needs `FERRUM_SECRET_KEY`: the signing key is a sealed secret field.
2. Open **Sign Settings** and set the sender name signers see, the public URL (used for emailed links, and required when the Mail app is installed), the default link expiry in days (30), the reminder cadence (3 days) and the maximum reminders (3). The signing key is created on first use.
3. Give people the **Sign User** role and the people who keep templates and see every request **Sign Manager**.
4. For emailed invitations, install the Mail app, or rely on the server's own mailer. Without any email, the sender gets the links in a notification and shares them.
5. For a button on another record to send a generated page for signature, run the platform's PDF service (`FERRUM_PDF_URL`): Sign signs PDFs only.
6. For text-message codes, set up the platform's text-message channel.

## 🔧 Develop

Clone it next to the platform and Mail:

```text
work/
├── ferrum/              https://github.com/CBFerrum/Ferrum (with Ferrum-Frontend cloned into ferrum/web)
├── ferrum-app-comms/    https://github.com/CBFerrum/ferrum-app-comms (its tests send emails through Mail)
└── ferrum-app-sign/
```

Point the platform dependencies at your checkout (this file is git-ignored):

```toml
# .cargo/config.toml
[patch."https://github.com/CBFerrum/Ferrum"]
ferrum-apps = { path = "../ferrum/crates/apps" }
ferrum-calc = { path = "../ferrum/crates/calc" }
ferrum-domain = { path = "../ferrum/crates/domain" }
ferrum-kernel = { path = "../ferrum/crates/kernel" }
ferrum-meta = { path = "../ferrum/crates/meta" }
```

The tests need PostgreSQL with an ordinary (non-superuser) role, so row-level security applies:

```bash
createuser ferrum --pwprompt && createdb -O ferrum ferrum_sign_test
export FERRUM_SIGN_TEST_DATABASE_URL=postgres://ferrum:ferrum@localhost:5432/ferrum_sign_test
cargo fmt -- --check
cargo clippy --all-targets -- -D warnings
cargo test
```

The interface lives in `ui/` and builds with Ferrum-Frontend's packages from the sibling checkout:

```bash
cd ui
npm run check       # UI lint and the size budget
npm run typecheck
npm test
npm run build       # writes ui/dist, which the crate embeds and which is committed
```

With a development server started with `FERRUM_DEV_UI=1`, `ferrum-app dev --ui` serves the pages from Vite behind the real bar and session, with hot reload. CI (`.github/workflows/ci.yml`) runs the Rust checks and the interface checks on every push and pull request. PDF parsing and stamping use `lopdf`, and drawn signatures are decoded with `png`.

## 📂 Project layout

```text
app.yaml              the manifest: id, version, launcher entry, signer page, Create menu
packages/sign/        entities, roles, reports, message template, events, SQL, README with the API
src/                  Rust: requests, templates, signing, sealing, PDF stamping, audit events,
                      identity checks, notices, upkeep jobs, public endpoints
ui/src/               React interface: requests, new request, field placement, templates,
                      the signer's page and signature pad
ui/src/locales/       Arabic and Hindi words
tests/                integration tests against PostgreSQL and the Mail app
docs/                 banner and screenshots
CHANGELOG.md          what changed in each version
```

## 📜 Changelog

Every release is described in [CHANGELOG.md](CHANGELOG.md). The latest is **0.3.1**; **0.3.0** added one-time codes, copy-only recipients, signing groups and signing from any record.

## 🤝 Contributing

Issues and pull requests are welcome at [github.com/CBFerrum/ferrum-app-sign](https://github.com/CBFerrum/ferrum-app-sign/issues). Run the checks in [Develop](#-develop) before you open a pull request.

## 📄 License

[AGPL-3.0-only](LICENSE), like the platform.

<div align="center">

<sub>Part of <a href="https://github.com/CBFerrum/Ferrum">Ferrum</a>, the open business platform · Made with 🦀 Rust and 🐘 PostgreSQL</sub>

</div>
