// Following a request: which list it belongs to, how far along it is, who is holding it up, what
// each event of its trail says, and what a check of its integrity found. Pure, so the words are
// tested without a browser.

import type { SignEvent, Signer, SignRequest, Status } from "./model";
import { t as tr } from "@ferrum/client";

export type TabId = "progress" | "drafts" | "completed" | "closed";

export const TABS: { id: TabId; label: string; statuses: Status[]; empty: string }[] = [
  { id: "progress", get label() { return tr("In progress"); }, statuses: ["Sent", "In Progress"], get empty() { return tr("Nothing is waiting on a signature."); } },
  { id: "drafts", get label() { return tr("Drafts"); }, statuses: ["Draft"], get empty() { return tr("No drafts. A request you start and don't send waits here."); } },
  { id: "completed", get label() { return tr("Completed"); }, statuses: ["Completed"], get empty() { return tr("Nothing has been fully signed yet."); } },
  { id: "closed", get label() { return tr("Closed"); }, statuses: ["Declined", "Expired", "Voided"], get empty() { return tr("Nothing was declined, withdrawn or left to expire."); } },
];

export const tabOf = (status: string): TabId => TABS.find((t) => t.statuses.includes(status as Status))?.id ?? "progress";

export const isOpen = (status: string) => status === "Sent" || status === "In Progress";

const pending = (s: Signer) => s.status !== "Signed" && s.status !== "Declined";

/** The signers who may sign now: everyone still to sign in parallel; in turn, those with the lowest
 *  order among them (equal numbers sign together). */
export function turnOf(r: Pick<SignRequest, "signing_mode" | "signers">): Signer[] {
  const left = (r.signers ?? []).filter(pending);
  if (r.signing_mode === "Parallel") return left;
  const first = Math.min(...left.map((s) => s.signing_order ?? 1));
  return left.filter((s) => (s.signing_order ?? 1) === first);
}

const who = (s: Pick<Signer, "name" | "email" | "role">) => s.name?.trim() || s.email?.trim() || s.role;

export interface Standing {
  signed: number;
  total: number;
  /** Who the request is waiting on now. */
  holding: Signer[];
  text: string;
}

/** Where a request stands, in a line. */
export function standing(r: Pick<SignRequest, "status" | "signing_mode" | "signers">): Standing {
  const signers = r.signers ?? [];
  const signed = signers.filter((s) => s.status === "Signed").length;
  const total = signers.length;
  const holding = isOpen(r.status) ? turnOf(r).filter((s) => s.status === "Sent" || s.status === "Viewed") : [];
  let text: string;
  switch (r.status) {
    case "Draft":
      text = "Not sent yet";
      break;
    case "Completed":
      text = total === 1 ? "Signed" : tr("Signed by all {total}", { total });
      break;
    case "Declined": {
      const d = signers.find((s) => s.status === "Declined");
      text = d ? tr("{d} declined", { d: who(d) }) : "Declined";
      break;
    }
    case "Voided":
      text = "Withdrawn by the sender";
      break;
    case "Expired":
      text = tr("Expired with {signed} of {total} signed", { signed, total });
      break;
    default:
      text = holding.length
        ? (holding.length > 2 ? tr("Waiting for {who} and {n} others", { who: who(holding[0]), n: holding.length - 1 }) : holding.length === 2 ? tr("Waiting for {who} and 1 other", { who: who(holding[0]) }) : tr("Waiting for {who}", { who: who(holding[0]) }))
        : tr("{signed} of {total} signed", { signed, total });
  }
  return { signed, total, holding, text };
}

/** Where one signer is, for the list of signers. */
export function signerLine(s: Signer, when: (iso: string | null | undefined) => string): string {
  switch (s.status) {
    case "Signed":
      return tr("Signed {value}", { value: when(s.signed_at) });
    case "Declined":
      return tr("Declined {value}", { value: when(s.declined_at) });
    case "Viewed":
      return tr("Opened {value}, not signed yet", { value: when(s.viewed_at) });
    case "Sent":
      return tr("Link sent {value}, not opened yet", { value: when(s.sent_at) });
    default:
      return "Waiting for their turn";
  }
}

/** "Reminded 2 times, last 3 days ago" (nothing when never). */
export function reminders(s: Signer, ago: (iso: string) => string): string {
  const n = s.reminders_sent ?? 0;
  if (!n) return "";
  return (n === 1 ? tr("Reminded once") : tr("Reminded {n} times", { n })) + (s.last_reminded_at ? tr(", last {when}", { when: ago(s.last_reminded_at) }) : "");
}

export type StepState = "done" | "now" | "later" | "declined";

export function stepState(s: Signer, turn: Signer[]): StepState {
  if (s.status === "Signed") return "done";
  if (s.status === "Declined") return "declined";
  return turn.some((t) => t.id === s.id && t.role === s.role) ? "now" : "later";
}

// ───────────────────────── the trail ─────────────────────────

export interface Told {
  title: string;
  detail: string;
  /** For the mark beside it. */
  tone: "good" | "bad" | "plain";
}

/** One event of the trail in words. */
export function told(e: SignEvent): Told {
  const name = e.signer_name?.trim() || e.signer_email?.trim() || e.role?.trim() || "the signer";
  const detail = e.details?.trim() ?? "";
  switch (e.event) {
    case "Sent":
      return { title: tr("Sent to {name}", { name }), detail, tone: "plain" };
    case "Viewed":
      return { title: tr("{name} opened the document", { name }), detail: "", tone: "plain" };
    case "Signed":
      return { title: tr("{name} signed", { name }), detail: detail ? tr("Signature {detail}", { detail: detail.replace(/^signature /, "") }) : "", tone: "good" };
    case "Declined":
      return { title: tr("{name} declined", { name }), detail: detail ? `“${detail}”` : "", tone: "bad" };
    case "Reminded":
      return { title: tr("Reminder for {name}", { name }), detail, tone: "plain" };
    case "Resent":
      return { title: tr("Link sent again to {name}", { name }), detail, tone: "plain" };
    case "Corrected":
      return { title: tr("Details of {name} corrected", { name }), detail, tone: "plain" };
    case "Access Denied":
      return { title: tr("Wrong access code for {name}", { name }), detail, tone: "bad" };
    case "Voided":
      return { title: tr("Withdrawn by the sender"), detail: detail ? `“${detail}”` : "", tone: "bad" };
    case "Expired":
      return { title: tr("Expired before everyone signed"), detail: "", tone: "bad" };
    case "Completed":
      return { title: tr("Everyone signed and the document was sealed"), detail: "", tone: "good" };
    default:
      return { title: e.event, detail, tone: "plain" };
  }
}

// ───────────────────────── integrity ─────────────────────────

export interface Verify {
  valid: boolean;
  status?: string;
  original: { ok: boolean; sha256?: string; expected?: string | null; error?: string };
  sealed: { ok: boolean; sha256?: string; expected?: string | null; readable?: boolean; error?: string } | null;
  audit_trail: { ok: boolean; events: number; head?: string | null; problem?: string | null };
}

export interface Check {
  label: string;
  ok: boolean;
  detail: string;
  /** The fingerprint recorded, and the one found now, when there is one to compare. */
  recorded?: string;
  found?: string;
}

/** What a check found, line by line. */
export function checks(v: Verify): Check[] {
  const out: Check[] = [
    {
      label: tr("The original document"),
      ok: v.original.ok,
      detail: v.original.ok ? "Stored unchanged: it still has the fingerprint recorded when it was sent." : (v.original.error ?? "It no longer matches the fingerprint recorded when it was sent."),
      recorded: v.original.expected ?? undefined,
      found: v.original.sha256,
    },
  ];
  if (v.sealed) {
    out.push({
      label: tr("The signed copy"),
      ok: v.sealed.ok,
      detail: v.sealed.ok
        ? "Stored unchanged, and it opens as a PDF."
        : (v.sealed.error ?? (v.sealed.readable === false ? "It can't be read as a PDF." : "It no longer matches the fingerprint recorded when it was sealed.")),
      recorded: v.sealed.expected ?? undefined,
      found: v.sealed.sha256,
    });
  }
  out.push({
    label: tr("The audit trail"),
    ok: v.audit_trail.ok,
    detail: v.audit_trail.ok ? tr("{value}, each tied to the one before it.", { value: String((v.audit_trail.events === 1 ? tr("1 event") : tr("{n} events", { n: v.audit_trail.events }))) }) : (v.audit_trail.problem ?? "The trail doesn't add up."),
  });
  return out;
}

/** A SHA-256 shortened for reading; the whole value stays in the title and the copy. */
export const short = (hash: string | null | undefined) => (hash ? `${hash.slice(0, 8)}…${hash.slice(-6)}` : "");

/** The badge a status wears. */
export const STATUS_TONE: Record<Status, "neutral" | "blue" | "green" | "amber" | "red"> = {
  Draft: "neutral",
  Sent: "blue",
  "In Progress": "blue",
  Completed: "green",
  Declined: "red",
  Expired: "amber",
  Voided: "neutral",
};

export const toneOf = (status: string) => STATUS_TONE[status as Status] ?? "neutral";
