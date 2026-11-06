# Changelog

## 0.2.9

Rebuilt with Ferrum-Frontend a64416a: a new record's box whose default is 1 shows ticked, as it is saved. An action's dialog says every refusal, the errors on what the action made (another record's rows) among them, and an action with parameters and a confirmation asks once.

## 0.2.8

Rebuilt with Ferrum-Frontend a76295e: its pages show times on a person's own clock when they chose one on their Account page, while today and due dates stay on the workspace's day.

## 0.2.7

The team's requests are on the workspace's clock; the signer's page keeps the signer's own. Rebuilt with Ferrum-Frontend 24c3951, so its records' pages show times on the workspace's clock too, a draft numbered on submit is headed as a draft, and a record of an app that is not installed says so.

## 0.2.6

Rebuilt with Ferrum-Frontend bb60115: a record page reads an empty number or check as the server does, so an action whose condition needs one shows, and a workspace's settings open as their one record.

## 0.2.5

The actions ask for what they read: Resend Links can go to one signer, and Void keeps a reason, but neither asked, so their pages could not send them. They now declare the signer (an email or role) and the reason.

## 0.2.4

A PDF chosen to be signed shows its size as people read it: "776 bytes" or "48 KB" for a small file, not "0.00 MB". Built with Ferrum-Frontend 01b8ff0.

## 0.2.3

Rebuilt with Ferrum-Frontend 00dea07: in a workflow state that names what may change, the other fields show read-only, and the fields the state needs show required.

## 0.2.2

Rebuilt with Ferrum-Frontend daeadda: after an action runs, a record page shows what the action said it did rather than only that it is done.

## 0.2.1

Rebuilt with Ferrum-Frontend 80ff179: a field's label names its control for assistive technology, and a record whose title is a link shows that record's title, not its id.

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
