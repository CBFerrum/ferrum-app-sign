// Preparing a request to send: the draft a sender edits (who signs, in what order, which boxes), what
// is saved, and what is missing before it can go. Pure, so the rules are tested without a browser. The
// server is the authority and checks all of this again at `send`; this tells the sender sooner and in
// words about people rather than fields.

import type { SignRequest, Template } from "./model";
import { type Box, lacksSignature, newKey, sameBoxes, toBoxes, toRows } from "./placement";
import { t as tr } from "@ferrum/client";

export interface SignerDraft {
  key: string;
  id?: string;
  role: string;
  name: string;
  email: string;
  signing_order: number;
  /** Typed by the sender, sent once, never given back. */
  accessCode: string;
  /** The signer already has a code on the server. */
  hasCode: boolean;
}

export interface Draft {
  title: string;
  message: string;
  signing_mode: "Sequential" | "Parallel";
  /** `YYYY-MM-DD`, or empty for the workspace's default. */
  expires_on: string;
  signers: SignerDraft[];
  boxes: Box[];
}

export const draftOf = (r: SignRequest): Draft => ({
  title: r.title ?? "",
  message: r.message ?? "",
  signing_mode: r.signing_mode === "Parallel" ? "Parallel" : "Sequential",
  expires_on: r.expires_on ?? "",
  signers: (r.signers ?? []).map((s) => ({
    key: s.id ?? newKey(),
    id: s.id,
    role: s.role ?? "",
    name: s.name ?? "",
    email: s.email ?? "",
    signing_order: s.signing_order ?? 1,
    accessCode: "",
    hasCode: !!s.access_code_hash,
  })),
  boxes: toBoxes(r.fields ?? []),
});

/** What is saved: the whole draft, signers and boxes as the full lists the API takes. */
export const bodyOf = (d: Draft) => ({
  title: d.title.trim(),
  message: d.message.trim() || null,
  signing_mode: d.signing_mode,
  expires_on: d.expires_on || null,
  signers: d.signers.map((s) => ({
    ...(s.id ? { id: s.id } : {}),
    role: s.role.trim(),
    name: s.name.trim() || null,
    email: s.email.trim() || null,
    signing_order: s.signing_order,
    ...(s.accessCode ? { access_code: s.accessCode } : {}),
  })),
  fields: toRows(d.boxes),
});

/** Whether there is anything to save. */
export const changed = (a: Draft, b: Draft) =>
  a.title.trim() !== b.title.trim() ||
  a.message.trim() !== b.message.trim() ||
  a.signing_mode !== b.signing_mode ||
  a.expires_on !== b.expires_on ||
  JSON.stringify(a.signers.map(({ key: _k, hasCode: _h, ...s }) => s)) !== JSON.stringify(b.signers.map(({ key: _k, hasCode: _h, ...s }) => s)) ||
  !sameBoxes(a.boxes, b.boxes);

/** "Signer 3": the first name for a role that nobody has. */
export function nextRole(signers: SignerDraft[]): string {
  const taken = new Set(signers.map((s) => s.role.trim().toLowerCase()));
  for (let n = signers.length + 1; ; n++) if (!taken.has(`signer ${n}`)) return tr("Signer {n}", { n });
}

export function addSigner(d: Draft): Draft {
  const last = Math.max(0, ...d.signers.map((s) => s.signing_order));
  const s: SignerDraft = { key: newKey(), role: nextRole(d.signers), name: "", email: "", signing_order: d.signing_mode === "Parallel" ? 1 : last + 1, accessCode: "", hasCode: false };
  return { ...d, signers: [...d.signers, s] };
}

/** Takes a signer out, and their boxes with them. */
export function removeSigner(d: Draft, key: string): Draft {
  const gone = d.signers.find((s) => s.key === key);
  if (!gone) return d;
  const role = gone.role.trim().toLowerCase();
  return { ...d, signers: d.signers.filter((s) => s.key !== key), boxes: d.boxes.filter((b) => b.role.trim().toLowerCase() !== role) };
}

/** Changes a signer; a new role name takes their boxes along. */
export function editSigner(d: Draft, key: string, change: Partial<SignerDraft>): Draft {
  const before = d.signers.find((s) => s.key === key);
  if (!before) return d;
  const boxes =
    change.role !== undefined && change.role !== before.role
      ? d.boxes.map((b) => (b.role.trim().toLowerCase() === before.role.trim().toLowerCase() ? { ...b, role: change.role as string } : b))
      : d.boxes;
  return { ...d, signers: d.signers.map((s) => (s.key === key ? { ...s, ...change } : s)), boxes };
}

/** One place up or down the list; the two swap their turns too, so the order still reads. */
export function moveSigner(d: Draft, key: string, by: -1 | 1): Draft {
  const i = d.signers.findIndex((s) => s.key === key);
  const j = i + by;
  if (i < 0 || j < 0 || j >= d.signers.length) return d;
  const list = [...d.signers];
  [list[i], list[j]] = [{ ...list[j], signing_order: list[i].signing_order }, { ...list[i], signing_order: list[j].signing_order }];
  return { ...d, signers: list };
}

/** Who signs together: the signers by turn, in turn order. */
export function turns(signers: SignerDraft[]): SignerDraft[][] {
  const by = new Map<number, SignerDraft[]>();
  for (const s of signers) by.set(s.signing_order, [...(by.get(s.signing_order) ?? []), s]);
  return [...by.entries()].sort((a, b) => a[0] - b[0]).map(([, list]) => list);
}

export const looksLikeEmail = (v: string) => /^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(v.trim());

export interface Problem {
  /** The step to fix it in. */
  step: "signers" | "fields" | "details";
  text: string;
  /** The signer it is about. */
  signer?: string;
}

/** What stops a request going out, in words. `today` is the workspace's date. */
export function sendProblems(d: Draft, today: string): Problem[] {
  const out: Problem[] = [];
  if (!d.title.trim()) out.push({ step: "details", text: "Give the request a title." });
  if (!d.signers.length) out.push({ step: "signers", text: "Add at least one person to sign." });
  const seen = new Set<string>();
  d.signers.forEach((s, i) => {
    const who = s.name.trim() || s.role.trim() || tr("Signer {value}", { value: i + 1 });
    const role = s.role.trim().toLowerCase();
    if (!role) out.push({ step: "signers", signer: s.key, text: tr("{who} needs a role.", { who }) });
    else if (seen.has(role)) out.push({ step: "signers", signer: s.key, text: tr("Two people are called “{value}”: roles must differ.", { value: s.role.trim() }) });
    seen.add(role);
    if (!s.name.trim()) out.push({ step: "signers", signer: s.key, text: tr("{value} needs a name.", { value: s.role.trim() || `Signer ${i + 1}` }) });
    if (!s.email.trim()) out.push({ step: "signers", signer: s.key, text: tr("{who} needs an email address.", { who }) });
    else if (!looksLikeEmail(s.email)) out.push({ step: "signers", signer: s.key, text: tr("“{value}” doesn't look like an email address.", { value: s.email.trim() }) });
  });
  for (const role of lacksSignature(d.boxes, d.signers.map((s) => s.role).filter((r) => r.trim()))) {
    const s = d.signers.find((x) => x.role === role);
    out.push({ step: "fields", signer: s?.key, text: tr("{value} has no signature box. Place a required Signature box for them.", { value: s?.name.trim() || role }) });
  }
  if (d.expires_on && d.expires_on < today) out.push({ step: "details", text: "The deadline is in the past." });
  return out;
}

/** The boxes of each signer, counted by kind, for the review. */
export function boxCounts(boxes: Box[], role: string): string {
  const mine = boxes.filter((b) => b.role.trim().toLowerCase() === role.trim().toLowerCase());
  if (!mine.length) return "no boxes";
  const n = (t: Box["field_type"]) => mine.filter((b) => b.field_type === t).length;
  const parts = [
    [n("Signature"), "signature"],
    [n("Initials"), "initials"],
    [n("Date Signed") + n("Name"), "automatic"],
    [n("Text"), "text"],
    [n("Checkbox"), "checkbox"],
  ] as const;
  return parts
    .filter(([c]) => c > 0)
    .map(([c, w]) => `${c} ${w}${c > 1 && w !== "initials" && w !== "automatic" ? (w === "checkbox" ? "es" : "s") : ""}`)
    .join(", ");
}

// ───────────────────────── from a template ─────────────────────────

/** A role of a template, filled in with a person. */
export interface Person {
  role: string;
  name: string;
  email: string;
  code: string;
}

/** The template's roles as people to fill in, prefilled with the defaults it holds. */
export const peopleOf = (t: Pick<Template, "roles">): Person[] => (t.roles ?? []).map((r) => ({ role: r.role, name: r.default_name ?? "", email: r.default_email ?? "", code: "" }));

/** What makes a draft from a template (`create_request`): only what was filled in. */
export function templateBody(title: string, message: string, expires: string, who: Person[]) {
  return {
    ...(title.trim() ? { title: title.trim() } : {}),
    ...(message.trim() ? { message: message.trim() } : {}),
    ...(expires ? { expires_on: expires } : {}),
    signers: who.map((p) => ({ role: p.role, name: p.name.trim(), email: p.email.trim(), ...(p.code ? { access_code: p.code } : {}) })),
  };
}

// ───────────────────────── the file to be signed ─────────────────────────

/** The server takes PDFs up to this size. */
export const MAX_MB = 25;

/** A request's title from its file's name: "Master_Agreement.pdf" → "Master Agreement". */
export const titleOf = (fileName: string) => fileName.replace(/\.pdf$/i, "").replace(/_+/g, " ").replace(/\s+/g, " ").trim();

/** A file's size as people read it: "776 bytes", "48 KB", "2.4 MB". */
export function sizeText(bytes: number): string {
  if (bytes < 1024) return `${bytes} ${bytes === 1 ? "byte" : "bytes"}`;
  if (bytes < 1000 * 1024) return tr("{n} KB", { n: Math.round(bytes / 1024) });
  return tr("{value} MB", { value: (bytes / 1024 / 1024).toFixed(1) });
}

/** Why a file cannot be sent, before it goes anywhere. */
export function refusal(f: { name: string; type: string; size: number }): string | null {
  if (!/\.pdf$/i.test(f.name) && f.type !== "application/pdf") return "Only PDF files can be signed here.";
  if (f.size > MAX_MB * 1024 * 1024) return tr("That file is larger than {MAX_MB} MB.", { MAX_MB });
  if (f.size === 0) return "That file is empty.";
  return null;
}
