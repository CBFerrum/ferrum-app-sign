// What the records API gives for Sign's records, as the screens use them. Decimals arrive as
// strings (docs/api.md), so a box's numbers are read with `num`.

import type { FieldType } from "./signing";

export type Status = "Draft" | "Sent" | "In Progress" | "Completed" | "Declined" | "Expired" | "Voided";
export type SignerStatus = "Waiting" | "Sent" | "Viewed" | "Signed" | "Declined";

export interface Signer {
  id?: string;
  role: string;
  name?: string | null;
  email?: string | null;
  signing_order: number;
  status?: SignerStatus;
  sent_at?: string | null;
  viewed_at?: string | null;
  signed_at?: string | null;
  declined_at?: string | null;
  decline_reason?: string | null;
  reminders_sent?: number | null;
  last_reminded_at?: string | null;
  /** Set when the signer has an access code; the code itself is never given back. */
  access_code_hash?: string | null;
  signature_text?: string | null;
}

export interface FieldRow {
  id?: string;
  role: string;
  field_type: FieldType;
  label?: string | null;
  page: number;
  x: number | string;
  y: number | string;
  width: number | string;
  height: number | string;
  required?: boolean;
  value?: string | null;
}

export interface SignRequest {
  id: string;
  version: number;
  number?: string | null;
  title: string;
  status: Status;
  message?: string | null;
  document?: string | null;
  document_name?: string | null;
  document_sha256?: string | null;
  page_count?: number | null;
  page_sizes?: [number, number][] | null;
  signing_mode: "Sequential" | "Parallel";
  expires_on?: string | null;
  expires_at?: string | null;
  reference_entity?: string | null;
  reference_id?: string | null;
  template?: string | null;
  sender_email?: string | null;
  sent_at?: string | null;
  completed_at?: string | null;
  void_reason?: string | null;
  sealed_document?: string | null;
  sealed_sha256?: string | null;
  seal_error?: string | null;
  signers: Signer[];
  fields: FieldRow[];
  created_at?: string | null;
  _titles?: Record<string, string>;
}

/** A line of the request list: the request without its signers and fields. */
export type RequestRow = Omit<SignRequest, "signers" | "fields"> & { signers?: Signer[] };

export interface SignEvent {
  id: string;
  seq: number;
  event: string;
  occurred_at: string;
  role?: string | null;
  signer_name?: string | null;
  signer_email?: string | null;
  actor?: string | null;
  details?: string | null;
  event_hash?: string | null;
}

export interface Template {
  id: string;
  version?: number;
  template_name: string;
  title?: string | null;
  message?: string | null;
  document?: string | null;
  document_name?: string | null;
  page_count?: number | null;
  page_sizes?: [number, number][] | null;
  signing_mode?: "Sequential" | "Parallel";
  expiry_days?: number | null;
  disabled?: boolean;
  roles?: { id?: string; role: string; signing_order?: number; default_name?: string | null; default_email?: string | null }[];
  fields?: FieldRow[];
}

/** A link handed back by an action that sent, reminded or resent: `url` only when it was not
 *  emailed (the workspace has no Mail), so the sender passes it on. */
export interface LinkRow {
  role: string;
  name?: string | null;
  email: string;
  emailed: boolean;
  url?: string;
}

export const num = (v: unknown, fallback = 0): number => {
  const n = typeof v === "number" ? v : Number(v);
  return Number.isFinite(n) ? n : fallback;
};

/** The pages' sizes as `{width, height}`, from the record's `[[width, height], …]`. */
export const pageSizes = (r: { page_sizes?: [number, number][] | null }) => (r.page_sizes ?? []).map(([width, height]) => ({ width: num(width, 612), height: num(height, 792) }));

/** A link given back as an absolute address (without a public URL it comes as a path). */
export const absolute = (url: string) => {
  try {
    return new URL(url, window.location.origin).href;
  } catch {
    return url;
  }
};
