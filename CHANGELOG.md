# Changelog

## 0.5.2

Needs Ferrum with `Ctx::at_screen()` (S1-P0c). Package `sign` 0.3.2 (one option, one field, one parameter). Interface
rebuilt (`ui/dist`).

- **In-person signing.** A signer's delivery can be In Person. On the request's page, "Sign in person here" asks the
  host to confirm they checked who is signing, then opens that signer's page on this device for 15 minutes. Only a
  person signed in at the screen can host: an app's call, an API key, a job or an agent is refused. The host is named
  on the signer, in the trail ("signed in person, hosted by …") and on the certificate.
- The send step asks how each signer signs: by email, or in person here.

## 0.5.1

Needs the same platform as 0.5.0. Package `sign` 0.3.1 (two signer fields, one action). No interface screen added; the
signer's page sends a session it was opened with.

- **A signer whose page another app opens.** A signer's `delivery` can be Link: nothing is mailed to them, and their
  link alone opens nothing. The app that asked (or the sender) calls `signing_link` and gets a link that opens that
  signer's page for 1 to 60 minutes (never past the request's expiry), bound to the signer and that moment by a tag
  under the workspace's signing key; framed only on a site the workspace allows for `sign/s`. Each link issued is in
  the trail. No reminders go to such a signer.
- **Forwarded links show.** Every device that opens a signing link is kept as a keyed hash; a second device is a
  "Viewed: another device" line in the trail.

## 0.5.0

Needs the same platform as 0.4.2. Package `sign` 0.3.0: callers declare `uses` at `^0.3` (Contracts' `^0.2` uses
move with it). No database change. Interface unchanged.

- **Other apps reach Sign through its operations.** Every action a person can press says what it does, what it returns
  and what "done" means (`contract:`), and declares the parameters it reads. Another app asks for a signature with
  `sign.sign_request.create` (`send_on_create`) through its `uses` and `Kernel::call_tx`: once per key, as the person it
  acts for. The Rust entry point `send_for_signature` and the unused `identity` interface are gone.
- **A void is told.** Voiding raises `sign.request_voided` with the reason, beside completed, declined, expired and
  failed.
- **Requests from other records ask for a code.** A request sent as it is made (by an app or a configured step) asks
  each signer to confirm a code sent to their own e-mail unless the caller chose otherwise; a request made on the screen
  asks for none unless chosen. The request's own default is now empty rather than None.
- **Verify Integrity is read-only.**

## 0.4.2

Needs Ferrum with K0-1 (`reports::readable_choice`). No database or interface change.

- **Reports read only what the caller may read.** Time to Complete grouped by Sender showed senders' addresses to a
  role whose rules hide them. Each report now refuses a grouping on a field the caller cannot read, and refuses a
  report whose figures come from one (completion times without `completed_at`, pending signers without `signers`)
  instead of answering with numbers computed from it.

## 0.4.1

Needs the same platform as 0.4.0. One database change: `packages/sign/sql/002_completed_is_sealed.sql` adds a check. No
interface change.

- **A seal that failed is retried for every request.** Upkeep retried sealing only when every row of a request had
  signed, so a request with a recipient of a copy, or with a signing group whose other members were not needed, stayed
  In Progress, fully signed and unsealed, after one failed try. It now retries every request that is signed by everyone
  who signs, and seals it once.
- **A request is never Completed without its seal.** The database refuses a Completed request with no sealed PDF or no
  SHA-256, whoever writes it.
- **A failure always reaches a person.** Notices meant for a request's sender (sealing failed, declined, expired) go to
  every Sign Manager when no person made the request (a configured step's or the system's).

## 0.4.0

No database or interface change. Needs Ferrum 0.4.0 (apps' upkeep runs one workspace at a time, on whichever replica is free).

- **Upkeep is one workspace's.** `maintenance(k, tenant)` sends, expires, reminds and reseals for the one workspace it is given; it no
  longer lists workspaces or checks for the package.

## 0.3.5

Needs the same platform as 0.3.4. No database change, interface unchanged.

- **Sign says what kind of signature it is.** The store card, the install dialog and the top of the README now state that
  Sign is a simple electronic signature with an audit trail, suited to internal approvals and agreements where a simple
  signature is accepted, and not a certificate-based, advanced or qualified one. Nothing about the signature itself changed.
- **The public signing routes take small requests.** Every route accepted a body of about 25 MB, so a signing link
  made the server read that much per request. Signing, declining, one-time codes and reassigning now take at most 1 MB
  (the signature image already could not be larger); only `POST /verify`, which takes a whole PDF, takes the large size.

## 0.3.4

Needs the same platform as 0.3.3. No database change. Interface only, `ui/dist` rebuilt with Ferrum-Frontend `06bd1a2`.

- The two routes to the screens apps no longer have (`apps/:app/settings`, `x/:app/:page`) are gone.
- The page starts through the frontend's `mountApp`; the
  interface lint has no baseline file any more.

## 0.3.3

* **Sign reads its signing key with its own handle on the vault**, in the same transaction as before, instead of the
  platform's reader that any app could use, which is deprecated. It registers as itself (`hooks.app("sign", …)`) to be
  given that handle; another app's test that registers Sign does the same. Needs a platform with the vault's field
  reader (0.3.8 on the platform's main from cdec881). Signers and senders see nothing new; while the vault is closed,
  signing links refuse plainly. No database or interface change.

## 0.3.2

* **A signer's link through the operator's link host.** With no public URL in Sign Settings nor in Mail's, a signer's
  link used to start at the workspace's staff address (`Kernel::link_base`). It now comes from the platform's address
  for people outside (`Kernel::person_link`): through the operator's link host when there is one
  (`https://{FERRUM_LINK_HOST}/go/{workspace}/p/api/site/{workspace}/sign/s/{token}`), so a link keeps working when the
  workspace's address changes, and leads to the workspace's portal host when it has one (which serves the signing
  endpoints, as every host of the workspace does); without a link host, the workspace's own address, as before. With
  a public URL set, nothing changes; with no address at all, the link is relative, as before. No database change, no
  interface change.

## 0.3.1

* **A page cannot start a signature, and the press is refused whole.** Sign needs a PDF, and a server with no PDF
  service (`FERRUM_PDF_URL`) generates a page (HTML). A configured button that generates a document, sends it with
  `sign.sign_request.create` and sets the record's status used to commit the status and leave a request that
  upkeep then could not send ("not a PDF"). The request now refuses a generated page in the button's own
  transaction, so the record keeps its status and no request or file is left. A file that is not a PDF is refused
  with a message naming the missing service. A request that still cannot be sent at upkeep tells its sender, as
  before, and now also raises the event `sign.request_failed` (request, number, reason, the record it is about).
* Tests: the one-time code is read from the platform's test channel (a code by text message), and the suite
  confirms a signer with the right code, refuses a wrong one, an expired one and a sixth try, and a code used once.

## 0.3.0

Closes the documents audit's tier-1 rows for Sign (DC-38, DC-40, DC-43 in words, DC-47) and DC-45, DC-46, DC-49.

* **One-time codes.** `verify_identity` on a request (None, Email Code, Text Code) and on a signer (As Request by
  default), a signer's `phone`; the endpoints `POST /signing/{token}/code` and `/verify`; the message template
  `sign.signing_code`; `identity_verified_at` and `identity_channel` on the signer; trail events Code Sent and Identity
  Verified; signing refused until the code is confirmed; the certificate says how each signer was checked.
* **Identity providers**: the interface `identity::IdentityProvider` (`Ask`, `Start`, `Outcome`, `trail_details`), no
  provider.
* **Recipients.** Signer `action` (Sign, Receive a Copy), `signing_group` (any one signs; the others Not Needed);
  request `allow_reassign` and `POST /signing/{token}/reassign` (`reassigned_from`, trail event Reassigned); reminders
  per request (`reminder_every_days`, `max_reminders`).
* **Sign from any record.** `send_on_create`, `source_sha256` and `signature_page` on Sign Request; a signature page
  added when a request has no fields; `send_for_signature`; upkeep sends a request whose file was generated in the
  same transaction; package events `request_completed`, `request_declined`, `request_expired`.
* **The kind of signature** on the certificate.
* Upkeep: expiry and sealing count a group member not needed and a copy-only recipient as done.

## 0.2.19

Rebuilt with Ferrum-Frontend 9e7190c, the design system as tuned after the landed build was reviewed. The left menu's icons are chosen so that none repeats on more than three rows and each fits its row; their drawings load beside the first page instead of inside it, so the page starts lighter; a row that scrolls sideways fades at the edge where there is more, in every browser and with reduced motion; a printed record leaves out empty fields and keeps its totals with the table; menus and popovers sit inside the page's landmarks for assistive technology; a selection bar folds what does not fit into More on a narrow screen. Nothing else changes.

## 0.2.18

The interface follows the upgraded design system, rebuilt with Ferrum-Frontend 76c4fad. Text uses the kit's type roles (nothing under 12px) and controls its height tokens; loading shows skeletons shaped like what is coming; empty, no matches, denied, offline and failed are told apart; status colours come from stored state ids. The interface check now runs the UI lint against an empty baseline.

## 0.2.17

The reports it defines in code (their titles and parameters) have names in Arabic and Hindi too, with its models' and manifest's (176 each in ui/src/locales/names). Translated by the model, to be reviewed by native speakers.

## 0.2.16

Rebuilt with Ferrum-Frontend 43d7779: the names of its models, fields and manifest in Arabic and Hindi (175 each in ui/src/locales/names, which the platform serves to every page of the workspace, so a record's labels, the launcher and the bar read in the person's language), and the sentences its pages still showed in English (split around a link or a figure, built in code from a template or word by word) whole and translated (429 words each in ui/src/locales). Translated by the model, to be reviewed by native speakers.

## 0.2.15

Rebuilt with Ferrum-Frontend 046a3a2: the sentences its pages built in code (counts, dates, what a toast or a screen reader says) go through t() too, as whole sentences where code chose a word, and carry built-in Arabic and Hindi (381 words each in ui/src/locales, translated by the model and to be reviewed by native speakers).

## 0.2.14

The assistant knows what its records mean in its pages (contributes.shell.assistant): their states, what counts and where figures come from, with questions it suggests asking there.

## 0.2.13

Rebuilt with Ferrum-Frontend 389b2af: its own words carry built-in Arabic and Hindi (337 words each, ui/src/locales, translated by the model and to be reviewed by native speakers), fetched for the page's language only. The labels it keeps in objects, its dialogs, its page titles and the words it keeps in variables go through t() as well, and the names and sizes it shows as they are keep their order right to left. Its pages are drawn once their words are in, and its entry carries only the parts of the kit it uses.

## 0.2.12

Rebuilt with Ferrum-Frontend 1f5981d: an action's link, code or token shown once is kept in a dialog to copy, and on a page that reads right to left, English left untranslated keeps its own order. Its own words go through the kit's t() (295 places), so a workspace's own words for its language reach them, and Admin, Interface words lists them under it (dist/words.json).

## 0.2.11

Rebuilt with Ferrum-Frontend 8a1c2e7: a workspace's own words for its interface (Admin, Interface words) reach its pages as they load; an attachment field that holds a picture shows it; and a value another record already has is said so, where the form said the record had been changed by someone else. Its own screens' margins, paddings, borders, alignment and insets go by the line's start and end, so they mirror right to left as the kit's do; what it places by coordinates (boards, timelines, charts, handles) stays as it was.

## 0.2.10

Rebuilt with Ferrum-Frontend 9d80f4f: right to left, its pages mirror as they read (icons that point along the line, margins and alignment by the line's start and end, arrow keys along it), and the records pages' words are in Arabic and Hindi.

## 0.2.9

Rebuilt with Ferrum-Frontend 204b713: a new record's box whose default is 1 shows ticked, as it is saved. An action's dialog says every refusal, the errors on what the action made (another record's rows) among them, and an action with parameters and a confirmation asks once.

## 0.2.8

Rebuilt with Ferrum-Frontend b39fffb: its pages show times on a person's own clock when they chose one on their Account page, while today and due dates stay on the workspace's day.

## 0.2.7

The team's requests are on the workspace's clock; the signer's page keeps the signer's own. Rebuilt with Ferrum-Frontend d350579, so its records' pages show times on the workspace's clock too, a draft numbered on submit is headed as a draft, and a record of an app that is not installed says so.

## 0.2.6

Rebuilt with Ferrum-Frontend 20d08e5: a record page reads an empty number or check as the server does, so an action whose condition needs one shows, and a workspace's settings open as their one record.

## 0.2.5

The actions ask for what they read: Resend Links can go to one signer, and Void keeps a reason, but neither asked, so their pages could not send them. They now declare the signer (an email or role) and the reason.

## 0.2.4

A PDF chosen to be signed shows its size as people read it: "776 bytes" or "48 KB" for a small file, not "0.00 MB". Built with Ferrum-Frontend 2c4c398.

## 0.2.3

Rebuilt with Ferrum-Frontend ae13c69: in a workflow state that names what may change, the other fields show read-only, and the fields the state needs show required.

## 0.2.2

Rebuilt with Ferrum-Frontend daeadda: after an action runs, a record page shows what the action said it did rather than only that it is done.

## 0.2.1

Rebuilt with Ferrum-Frontend 89b7cc7: a field's label names its control for assistive technology, and a record whose title is a link shows that record's title, not its id.

## 0.2.0

- The document's real pages, behind the boxes: in the editor, and for signers, who now read every page where they sign. Drawn in the browser by pdf.js, which arrives only when a document opens.
- A page made for signers, at `/sign/s/{workspace}/{token}`, opened from their link with no account. It shows the pages that have something for them with their boxes in place, and one button walks them through what is left: sign (drawn or typed), add initials, fill in, tick, then read the agreement and finish. It works on a phone, with the next step at the thumb. It says plainly when a link is spent, withdrawn, expired or not yet their turn, asks for the access code when there is one, lets them decline with a reason, and says what kind of signature this is: an electronic signature with an audit trail, not a PAdES, advanced or qualified one.
- Sending a request is a few steps: who signs and in what order, where each of them signs, and a last look that says in words what is still missing. Boxes are placed on the pages by dragging from a palette or with the keyboard (add, move, resize with the arrow keys, copy, delete), one colour per signer, with undo, zoom, and exact numbers for the selected box. Templates have the same screen for their boxes, and "Use" asks who signs this time.
- Following a request: every request by where it stands (in progress, drafts, completed, closed) with who is holding it up; on its page each signer's progress and reminders, a reminder or the links again (with links to copy when the workspace sends no email), correcting a name or email, withdrawing it with a reason, the audit trail told in words, a check of integrity that says which file or entry no longer matches, and the signed copy to download.
- Links already sent keep working: the old self-contained signing page hands over to the new page where the platform serves it, and does the job itself where it does not.
- The pages are drawn as sheets of the pages' size, not as pictures of the document: the platform has no PDF renderer yet. The PDF opens in the browser's own viewer from every screen that shows the sheets.
- Personal data: a person's request now exports their signing events and the requests they sent, and says why they are kept: they are the evidence of a signature.
- Emails no longer need the Mail app: without it, invitations, reminders and withdrawals go
  through the server's mailer, and the completion email carries the signed PDF when the mailer
  can attach files.

## 0.1.0

- First release. Sign requests send a PDF — uploaded or from a reusable template, optionally linked to any record — to signers in a sequential routing order or in parallel, with signature, initials, date, name, text and checkbox fields placed per signer role and checked against the page geometry (rotation included); only real PDFs are accepted. Signers act through a self-contained signing page and public endpoints authenticated by HMAC link tokens (key in Sign Settings, constant-time checks, expiring, single-use, sequential turns enforced) and optional Argon2id access codes with lockout; they view the PDF with their own fields, sign with a validated drawn PNG or a typed signature, or decline, idempotently and safely under concurrency. The last signature seals the document: values stamped onto the original pages, an audit certificate appended as an incremental update (so the signed document is a verifiable byte prefix), the sealed PDF stored with its SHA-256, the sender notified and signers emailed a copy through the Mail app (copyable links and in-app notifications without it). An append-only, SHA-256 hash-chained audit trail records every step with salted IP hashes; `verify` recomputes the document and trail hashes and a public endpoint checks presented PDFs. Maintenance expires requests, sends reminders on a cadence (each once) and retries sealing; reports cover requests by status, time to complete and pending signatures by signer. It is an electronic signature with an audit trail, not a PAdES digital signature.
