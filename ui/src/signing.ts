// What a signer's page decides, apart from drawing it: how the server's answers to a link are read,
// which things the signer has to do and in what order, and the body that signs. Pure, so the rules
// are tested without a browser. The server holds the rules (what a field accepts, who may sign now);
// this only shows what it says.

export type FieldType = "Signature" | "Initials" | "Date Signed" | "Name" | "Text" | "Checkbox";

export interface PageSize {
  page: number;
  width: number;
  height: number;
}

/** One of the signer's own boxes, in points from the top left of the page as displayed. */
export interface SigningField {
  id: string;
  field_type: FieldType;
  label?: string | null;
  page: number;
  x: number;
  y: number;
  width: number;
  height: number;
  required: boolean;
}

export interface SigningView {
  request: {
    number?: string | null;
    title: string;
    message?: string | null;
    status: string;
    signing_mode?: string | null;
    expires_at?: string | null;
    document_name?: string | null;
    document_sha256?: string | null;
    pages: PageSize[];
    sender: { organization?: string | null; name?: string | null; email?: string | null };
  };
  signer: { role?: string | null; name?: string | null; email?: string | null; status?: string | null };
  fields: SigningField[];
  needs: { signature: boolean; initials: boolean };
  consent: string;
}

// ───────────────────────── what a link answers ─────────────────────────

/** The workspace and the token in the address of a signer's page (`/sign/s/{workspace}/{token}`). */
export function locate(path: string): { tenant: string; token: string } | null {
  const m = /\/s\/([^/]+)\/([^/]+)\/?$/.exec(path);
  if (!m) return null;
  try {
    return { tenant: decodeURIComponent(m[1]), token: decodeURIComponent(m[2]) };
  } catch {
    return null;
  }
}

/** Why a link can no longer be used (the server's 410). */
export type Why = "signed" | "declined" | "completed" | "request_declined" | "voided" | "expired";

export type Reached =
  | { kind: "ready"; view: SigningView }
  | { kind: "code"; problem?: string }
  | { kind: "locked"; message: string }
  | { kind: "waiting"; message: string }
  | { kind: "invalid" }
  | { kind: "closed"; why: Why; requestStatus: string | null }
  | { kind: "failed"; message: string };

const asText = (v: unknown): string | null => (typeof v === "string" && v ? v : null);

const sentence = (m: string) => (m ? m.charAt(0).toUpperCase() + m.slice(1) : m);

/** Reads the answer to opening a link. `sentCode`: whether an access code went with the request
 *  (a refusal then means the code was wrong, not that none was given). */
export function classify(status: number, body: unknown, sentCode: boolean): Reached {
  const b = body && typeof body === "object" ? (body as Record<string, unknown>) : {};
  const message = asText(b.message) ?? "";
  if (status === 200) return { kind: "ready", view: body as SigningView };
  if (status === 401) return { kind: "code" };
  if (status === 403) {
    if (/not your turn/i.test(message)) return { kind: "waiting", message: sentence(message) };
    return sentCode ? { kind: "code", problem: "That code isn't right. Check it and try again." } : { kind: "failed", message: sentence(message) || "You can't open this link." };
  }
  if (status === 404) return { kind: "invalid" };
  if (status === 410) {
    const signer = asText(b.signer_status);
    const request = asText(b.request_status);
    let why: Why;
    if (signer === "Signed") why = "signed";
    else if (signer === "Declined") why = "declined";
    else if (request === "Completed") why = "completed";
    else if (request === "Declined") why = "request_declined";
    else if (request === "Voided") why = "voided";
    else why = "expired";
    return { kind: "closed", why, requestStatus: request };
  }
  if (status === 429) return { kind: "locked", message: sentence(message) || "Too many wrong codes. Try again in a few minutes." };
  return { kind: "failed", message: sentence(message) || `Something went wrong (${status}).` };
}

/** The words for a link that can no longer be used. */
export function closedCopy(why: Why, requestStatus: string | null): { title: string; text: string } {
  switch (why) {
    case "signed":
      return {
        title: "You've already signed",
        text:
          requestStatus === "Completed"
            ? "Everyone has now signed. The sender has the sealed copy."
            : "Thank you. The others still have to sign; the sender is told when everyone has.",
      };
    case "declined":
      return { title: "You declined to sign", text: "The sender was told and this request is closed. If you changed your mind, ask them to send a new one." };
    case "completed":
      return { title: "Everyone has signed", text: "This request is finished, so the link no longer works." };
    case "request_declined":
      return { title: "This request was declined", text: "Someone declined to sign, so it can't be signed any more. Contact the sender if you have questions." };
    case "voided":
      return { title: "The sender withdrew this request", text: "There is nothing to sign any more. Contact the sender if you think this is a mistake." };
    case "expired":
      return { title: "This request has expired", text: "The deadline has passed. Ask the sender to send a new request." };
  }
}

// ───────────────────────── what the signer has to do ─────────────────────────

/** A drawn mark (a PNG data URL) or a typed one. */
export type Mark = { image: string } | { text: string };

export interface Entry {
  signature: Mark | null;
  initials: Mark | null;
  /** By field id: text for a Text field, true or false for a Checkbox. */
  values: Record<string, string | boolean>;
}

export const emptyEntry = (): Entry => ({ signature: null, initials: null, values: {} });

export type TaskKind = "signature" | "initials" | "text" | "checkbox";

/** One thing to do. All Signature boxes are one task (one signature fills them all), likewise
 *  Initials; each Text and Checkbox box is its own. Name and Date Signed fill themselves. */
export interface Task {
  key: string;
  kind: TaskKind;
  label: string;
  required: boolean;
  /** The boxes it fills, first in reading order first. */
  fields: SigningField[];
}

/** Reading order: page by page, top to bottom (boxes within a few points of each other count as
 *  one line), then left to right. */
export function readingOrder(a: SigningField, b: SigningField): number {
  return a.page - b.page || Math.round(a.y / 8) - Math.round(b.y / 8) || a.x - b.x;
}

export function tasksOf(fields: SigningField[]): Task[] {
  const sorted = [...fields].sort(readingOrder);
  const out: Task[] = [];
  const shared = (kind: "signature" | "initials", type: FieldType, label: string) => {
    const mine = sorted.filter((f) => f.field_type === type);
    if (mine.length) out.push({ key: kind, kind, label, required: mine.some((f) => f.required), fields: mine });
  };
  shared("signature", "Signature", "Your signature");
  shared("initials", "Initials", "Your initials");
  for (const f of sorted) {
    if (f.field_type === "Text") out.push({ key: f.id, kind: "text", label: f.label || "Text", required: f.required, fields: [f] });
    if (f.field_type === "Checkbox") out.push({ key: f.id, kind: "checkbox", label: f.label || "Checkbox", required: f.required, fields: [f] });
  }
  return out.sort((a, b) => readingOrder(a.fields[0], b.fields[0]) || a.key.localeCompare(b.key));
}

export function isDone(t: Task, e: Entry): boolean {
  switch (t.kind) {
    case "signature":
      return e.signature !== null;
    case "initials":
      return e.initials !== null;
    case "text":
      return typeof e.values[t.key] === "string" && (e.values[t.key] as string).trim() !== "";
    case "checkbox":
      return e.values[t.key] === true;
  }
}

/** The tasks that have to be done before signing. */
export const requiredTasks = (tasks: Task[]) => tasks.filter((t) => t.required);

export function progress(tasks: Task[], e: Entry): { done: number; total: number } {
  const need = requiredTasks(tasks);
  return { done: need.filter((t) => isDone(t, e)).length, total: need.length };
}

export const complete = (tasks: Task[], e: Entry) => requiredTasks(tasks).every((t) => isDone(t, e));

/** The next required task still to do: the one after `from` if there is one, else the first. */
export function nextTask(tasks: Task[], e: Entry, from?: string): Task | null {
  const open = tasks.filter((t) => t.required && !isDone(t, e));
  if (!open.length) return null;
  const at = from ? tasks.findIndex((t) => t.key === from) : -1;
  return open.find((t) => tasks.indexOf(t) > at) ?? open[0];
}

/** What the primary button says it will do next. */
export function actionLabel(next: Task | null): string {
  if (!next) return "Review and sign";
  switch (next.kind) {
    case "signature":
      return "Add your signature";
    case "initials":
      return "Add your initials";
    case "text":
      return `Fill in “${next.label}”`;
    case "checkbox":
      return `Tick “${next.label}”`;
  }
}

/** The pages that have something for this signer, in order. */
export function pagesWithFields(fields: SigningField[]): number[] {
  return [...new Set(fields.map((f) => f.page))].sort((a, b) => a - b);
}

/** "pages 2 and 4", "page 3", "pages 1, 2 and 5". */
export function pagesText(pages: number[]): string {
  if (pages.length === 0) return "";
  if (pages.length === 1) return `page ${pages[0]}`;
  return `pages ${pages.slice(0, -1).join(", ")} and ${pages[pages.length - 1]}`;
}

/** Initials from a name, as the server makes them: a letter of each word, at most four. */
export function initialsOf(name: string): string {
  const letters = name.split(/\s+/).map((w) => w.match(/\p{L}/u)?.[0] ?? "");
  return Array.from(letters.join("").toUpperCase()).slice(0, 4).join("");
}

// ───────────────────────── signing ─────────────────────────

export interface SignBody {
  consent: true;
  signature?: Mark;
  initials?: Mark;
  values: Record<string, string | boolean>;
}

/** The body that signs. Checkboxes always go (true or false), text only when written. */
export function signBody(fields: SigningField[], e: Entry): SignBody {
  const values: Record<string, string | boolean> = {};
  for (const f of fields) {
    const v = e.values[f.id];
    if (f.field_type === "Checkbox") values[f.id] = v === true;
    if (f.field_type === "Text" && typeof v === "string" && v.trim()) values[f.id] = v.trim();
  }
  const body: SignBody = { consent: true, values };
  if (fields.some((f) => f.field_type === "Signature") && e.signature) body.signature = e.signature;
  if (fields.some((f) => f.field_type === "Initials") && e.initials) body.initials = e.initials;
  return body;
}

/** Where a server complaint belongs: a task's key, `consent`, or `""` for the whole form. */
export function problemKey(field: string, fields: SigningField[]): string {
  if (field === "signature" || field === "initials" || field === "consent") return field;
  const id = field.startsWith("values.") ? field.slice(7) : "";
  const f = fields.find((x) => x.id === id);
  if (!f) return "";
  return f.field_type === "Signature" ? "signature" : f.field_type === "Initials" ? "initials" : f.id;
}

/** The server's complaints about a submission, by where they belong. */
export function problemsOf(errors: { field: string; message: string }[], fields: SigningField[]): Record<string, string> {
  const out: Record<string, string> = {};
  for (const e of errors) {
    const key = problemKey(e.field, fields);
    out[key] = out[key] ? `${out[key]} ${sentence(e.message)}` : sentence(e.message);
  }
  return out;
}

/** What a field shows as its own state: waiting for the signer, being done, done, or filled by itself. */
export type BoxState = "todo" | "current" | "done" | "auto" | "optional";

export function boxState(f: SigningField, tasks: Task[], e: Entry, current: string | null): BoxState {
  if (f.field_type === "Name" || f.field_type === "Date Signed") return "auto";
  const t = tasks.find((x) => x.fields.some((y) => y.id === f.id));
  if (!t) return "auto";
  if (isDone(t, e)) return "done";
  if (t.key === current) return "current";
  return t.required ? "todo" : "optional";
}
