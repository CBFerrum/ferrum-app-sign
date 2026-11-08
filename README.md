<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/assets/banner-dark.svg">
  <img alt="Ferrum Sign" src="docs/assets/banner-light.svg" width="100%">
</picture>

[![CI](https://github.com/CBFerrum/ferrum-app-sign/actions/workflows/ci.yml/badge.svg)](https://github.com/CBFerrum/ferrum-app-sign/actions/workflows/ci.yml) ![Version](https://img.shields.io/badge/version-0.2.0-a63056?style=flat-square) ![Ferrum](https://img.shields.io/badge/Ferrum-%3E%3D0.3%2C%20%3C0.4-2f5bea?style=flat-square) ![License](https://img.shields.io/badge/license-AGPL--3.0-3b82f6?style=flat-square) ![Rust](https://img.shields.io/badge/Rust-native%20app-b7410e?style=flat-square&logo=rust&logoColor=white) ![MCP](https://img.shields.io/badge/AI%20agents-MCP-8b5cf6?style=flat-square)

**A native app for [Ferrum](https://github.com/CBFerrum/Ferrum), the open-source, AI-first business platform.**

[Overview](#overview) · [Highlights](#highlights) · [Screenshots](#screenshots) · [How it works](#how-it-works) · [Data model](#data-model) · [Install](#install) · [Develop](#develop) · [Changelog](#changelog)

</div>

---

## Overview

Ferrum Sign sends PDFs for signature: an upload or a reusable template, linked to any record.
Signers sign in order or at once through personal, expiring, single-use links; values are stamped
onto the original pages and an audit certificate appended as an incremental update, so the signed
document is a byte-exact prefix of the sealed file; and a hash-chained audit trail records every
step.


| At a glance | |
|---|---|
| 📦 Version | `0.2.0` |
| 🧱 Platform | Ferrum `>=0.3, <0.4` |
| 🔗 Requires | nothing else |
| 🖥️ Interface | its own at `/sign/` (requests, sending, templates) and a page for signers, with Ferrum's records screens for the rest |
| 🗃️ Records | 7 entities in 1 package |
| 👥 Roles | 2: Sign User, Sign Manager |
| 📊 Reports | 3 |
| 🧪 Tests | 16 Rust tests, 106 interface tests |


> [!IMPORTANT]
> This is an electronic signature with an audit trail, not a PAdES digital signature: integrity rests on the SHA-256 values and the audit trail.


## Highlights

<table>
<tr>
<td width="50%" valign="top">

### 📄 Any PDF
An upload or a template, numbered and linked to a quotation or contract.

</td>
<td width="50%" valign="top">

### 🧭 Routing
Sequential or parallel signers; fields per role placed on any page.

</td>
</tr>
<tr>
<td width="50%" valign="top">

### 🔐 Personal links
HMAC tokens that expire, are single-use and checked in constant time; access codes.

</td>
<td width="50%" valign="top">

### ✒️ Sign or decline
A drawn or typed signature, or a decline with a reason.

</td>
</tr>
<tr>
<td width="50%" valign="top">

### 🧷 Sealed PDFs
Values stamped and an audit certificate appended; SHA-256 kept.

</td>
<td width="50%" valign="top">

### ⛓️ Audit trail
Append-only and hash-chained; a public endpoint verifies a sealed PDF.

</td>
</tr>
</table>

## Screenshots

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/assets/sign-dark.webp">
  <img alt="Sign requests and where each one stands." src="docs/assets/sign-light.webp" width="100%">
</picture>

<p align="center"><sub>Sign requests and where each one stands.</sub></p>

> [!TIP]
> The screenshots follow your GitHub theme: switch between light and dark to see both.

## How it works

### Signing and sealing

```mermaid
sequenceDiagram
    autonumber
    actor Sender
    participant Sign as Sign
    actor S1 as Signer 1
    actor S2 as Signer 2
    Sender->>Sign: send the PDF (order 1, then 2)
    Sign-->>S1: personal link
    S1->>Sign: sign (drawn or typed)
    Sign-->>S2: personal link
    S2->>Sign: sign
    Sign->>Sign: stamp values, append the audit certificate, hash
    Sign-->>Sender: sealed PDF and its SHA-256
```

### Inside Ferrum

```mermaid
flowchart LR
    subgraph Browser
        UI["Records screens<br/><small>/data/sign/</small>"]
        BAR["Ferrum bar<br/><small>launcher · search · bell</small>"]
    end
    subgraph Server["Ferrum server"]
        API["REST · MCP · live"]
        K["Kernel<br/><small>permissions · row-level security · audit</small>"]
        APP["Sign<br/><small>7 entities · 2 roles</small>"]
        OB[("Outbox")]
    end
    DB[("PostgreSQL")]
    UI --> API --> K --> DB
    BAR --> API
    K <--> APP
    K --> OB --> EV["Notifications · webhooks<br/>automations"]

    classDef app fill:#a63056,color:#fff,stroke:#a63056
    class APP app
```

Everything Sign adds is ordinary Ferrum metadata and Rust: its records get lists, forms,
search, permissions, an audit trail, exports and the REST and MCP APIs for free, so AI agents work
with them under the same rules as people.

## Data model

The main records and how they connect (7 entities in all; `packages/` has every field).

```mermaid
erDiagram
    SignRequest {
        string title PK
        string status
        file document
    }
    SignTemplate {
        string template_name PK
        file document
        string signing_mode
    }
    SignEvent {
        int seq
        string event PK
        datetime occurred_at
    }
    SignTemplate ||--o{ SignRequest : "template"
    SignRequest ||--|{ SignRequestSigner : "signers"
    SignRequest ||--|{ SignField : "fields"
    SignRequest ||--o{ SignEvent : "request"
```

### Roles

| Role | What it is for | Entities |
|---|---|---|
| **Sign User** | Sends their own documents for signature (uploads or templates), follows them and reads their audit trails. | 4 |
| **Sign Manager** | Every sign request and audit trail, templates and Sign Settings (including rotating the signing key). | 4 |

<details>
<summary><b>Reports (3)</b></summary>

- Sign Requests By Status
- Sign Completion Time
- Sign Pending Signatures

</details>

## Install

Workspace administrators install it from **Apps** (`/store/`) in Ferrum; it ships in the
official Ferrum server image. Self-hosters building their own image enable the
`sign` feature of the server. Everything it adds is also available to AI agents over
Ferrum's MCP server, within each person's permissions.

## Develop

Clone it next to the platform:

```text
work/
├── ferrum/            https://github.com/CBFerrum/Ferrum
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

The tests need PostgreSQL with an ordinary (non-superuser) role, so row-level security
applies:

```bash
createuser ferrum --pwprompt && createdb -O ferrum ferrum_sign_test
cargo test
```

`FERRUM_SIGN_TEST_DATABASE_URL` overrides the database URL.


Its tests also build on [ferrum-app-comms](https://github.com/CBFerrum/ferrum-app-comms), which Cargo takes from GitHub.
To work on it alongside this app, clone it into the same folder and add to the same file:

```toml
[patch."https://github.com/CBFerrum/ferrum-app-comms"]
ferrum-app-comms = { path = "../ferrum-app-comms" }
```


The interface lives in `ui/` and builds with Ferrum-Frontend's packages (clone it as `ferrum/web`
next to this app, as CI does):

```bash
cd ui
npm run build      # ui/dist, checked against the size budget
npm test
```

It has two entries that never load each other: the page a signer opens from their link (no
sign-in, `shell.public`), and the app for people who are signed in. The platform serves it once
`ferrum_app_sign::UI` is among the interfaces the server compiles in.

### Project layout

```text
app.yaml      the manifest: id, version, requirements, what it adds to Ferrum
packages/     metadata: entities, roles, reports, workflows, print formats
src/          the app's logic in Rust: hooks, actions, endpoints, upkeep jobs
ui/           its own interface (React + Vite), built into ui/dist
tests/        integration tests against a real PostgreSQL
CHANGELOG.md  what changed in each version
```

## Changelog

| Version | Highlights |
|---|---|
| **0.2.16** | Its names in Arabic and Hindi; the last English sentences translated. |
| **0.2.15** | Sentences built in code are words too, in Arabic and Hindi. |
| **0.2.14** | The assistant knows its records. |
| **0.2.13** | Rebuilt with Ferrum-Frontend 5b22e7b: its own words in Arabic and Hindi. |
| **0.2.12** | Rebuilt with Ferrum-Frontend 6537d65: once-shown answers kept; English isolated right to left; its own words translatable by a workspace. |
| **0.2.11** | Rebuilt with Ferrum-Frontend d5d98f2: a workspace's own interface words; pictures in attachment fields; a duplicate value said so; its own screens mirror right to left. |
| **0.2.10** | Rebuilt with Ferrum-Frontend 63280d3: right to left, pages mirror; the records pages' words in Arabic and Hindi. |
| **0.2.9** | Rebuilt with Ferrum-Frontend a64416a: a box whose default is 1 shows ticked; action dialogs say every refusal and ask once. |
| **0.2.8** | Rebuilt with Ferrum-Frontend a76295e: times on a person's own clock when they chose one. |
| **0.2.7** | Times on the workspace's clock; rebuilt with Ferrum-Frontend 24c3951. |
| **0.2.6** | Rebuilt with Ferrum-Frontend bb60115: empty numbers read as the server reads them; settings open as their record. |
| **0.2.5** | Resend Links asks which signer, Void asks why. |
| **0.2.4** | A small PDF's size reads in bytes or KB, not "0.00 MB". |
| **0.2.3** | Rebuilt with Ferrum-Frontend 00dea07: a workflow state's kept fields are read-only, its needed ones required. |
| **0.2.2** | Rebuilt with Ferrum-Frontend daeadda: actions say what they did. |
| **0.2.1** | Rebuilt with Ferrum-Frontend 80ff179: labels name their fields, and linked titles show their titles. |
| **0.2.0** | A page made for signers (their boxes in place, one button through what is left, plainly what kind of signature it is), sending a request in steps with boxes placed by dragging or from the keyboard, and following each request: who is holding it up, reminders, the trail told in words, a check of integrity, the signed copy. |
| **0.1.0** | First release. Sign requests send a PDF — uploaded or from a reusable template, optionally linked to any record — to signers in a sequential … |

Every version is in [CHANGELOG.md](CHANGELOG.md).

## License

[AGPL-3.0-only](LICENSE), like the platform. Part of [Ferrum](https://github.com/CBFerrum/Ferrum) ·
[Report an issue](https://github.com/CBFerrum/ferrum-app-sign/issues)
