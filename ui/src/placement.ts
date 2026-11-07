// Placing boxes on pages: where a new one goes, how one moves and grows (with a pointer or the
// keyboard), what undo remembers, and what is missing before a request can go out. Pure, so the
// rules are tested without a browser. The server checks the same things when the request is saved
// and sent (a box inside its page, a Signature box for everyone); this tells the sender sooner.

import type { Rect, Size } from "./geometry";
import { type FieldRow, num } from "./model";
import type { FieldType } from "./signing";
import { t as tr } from "@ferrum/client";

/** A box as the editor holds it, in points from the top left of the page as displayed. */
export interface Box extends Rect {
  /** Stays the same across saves, so the selection does. */
  key: string;
  /** The saved row's id, once there is one. */
  id?: string;
  role: string;
  field_type: FieldType;
  label: string;
  page: number;
  required: boolean;
}

export const MIN_SIZE = 4;

/** The kinds of box, with the size a new one has. */
export const KINDS: { type: FieldType; label: string; hint: string; size: [number, number] }[] = [
  { type: "Signature", get label() { return tr("Signature"); }, get hint() { return tr("The signer draws or types it"); }, size: [160, 44] },
  { type: "Initials", get label() { return tr("Initials"); }, get hint() { return tr("Their initials"); }, size: [56, 32] },
  { type: "Date Signed", get label() { return tr("Date signed"); }, get hint() { return tr("Filled in with the day they sign"); }, size: [96, 22] },
  { type: "Name", get label() { return tr("Name"); }, get hint() { return tr("Filled in with their name"); }, size: [160, 22] },
  { type: "Text", get label() { return tr("Text"); }, get hint() { return tr("A line they type"); }, size: [160, 22] },
  { type: "Checkbox", get label() { return tr("Checkbox"); }, get hint() { return tr("A box they tick"); }, size: [18, 18] },
];

export const kindOf = (t: FieldType) => KINDS.find((k) => k.type === t) ?? KINDS[0];

const clamp = (v: number, lo: number, hi: number) => Math.min(Math.max(v, lo), Math.max(lo, hi));

/** Whole points: tidy numbers, and a box lands where the eye put it. */
export const whole = (v: number) => Math.round(v);

/** A rectangle kept on its page and no smaller than the server accepts. */
export function fit(r: Rect, page: Size): Rect {
  const width = Math.min(Math.max(r.width, MIN_SIZE), page.width);
  const height = Math.min(Math.max(r.height, MIN_SIZE), page.height);
  return { x: clamp(r.x, 0, page.width - width), y: clamp(r.y, 0, page.height - height), width, height };
}

export const move = (r: Rect, dx: number, dy: number, page: Size): Rect => fit({ ...r, x: r.x + dx, y: r.y + dy }, page);

export type Handle = "n" | "s" | "e" | "w" | "ne" | "nw" | "se" | "sw";

export const HANDLES: Handle[] = ["nw", "n", "ne", "e", "se", "s", "sw", "w"];

/** Drags one edge or corner; the opposite side stays where it is, and nothing leaves the page. */
export function resize(r: Rect, handle: Handle, dx: number, dy: number, page: Size): Rect {
  let { x, y, width, height } = r;
  if (handle.includes("e")) width += dx;
  if (handle.includes("s")) height += dy;
  if (handle.includes("w")) {
    x += dx;
    width -= dx;
  }
  if (handle.includes("n")) {
    y += dy;
    height -= dy;
  }
  if (width < MIN_SIZE) {
    if (handle.includes("w")) x = r.x + r.width - MIN_SIZE;
    width = MIN_SIZE;
  }
  if (height < MIN_SIZE) {
    if (handle.includes("n")) y = r.y + r.height - MIN_SIZE;
    height = MIN_SIZE;
  }
  if (x < 0) {
    width += x;
    x = 0;
  }
  if (y < 0) {
    height += y;
    y = 0;
  }
  width = Math.min(width, page.width - x);
  height = Math.min(height, page.height - y);
  return { x, y, width: Math.max(width, MIN_SIZE), height: Math.max(height, MIN_SIZE) };
}

/** Grows or shrinks from the bottom right, as the keyboard does. */
export const grow = (r: Rect, dw: number, dh: number, page: Size): Rect => resize(r, "se", dw, dh, page);

/** What an arrow key does to the selected box: arrows move it a point (ten with Shift); with Alt
 *  they change its size instead. */
export function arrow(e: { key: string; shiftKey: boolean; altKey: boolean }): { dx: number; dy: number; size: boolean } | null {
  const step = e.shiftKey ? 10 : 1;
  const [dx, dy] = e.key === "ArrowLeft" ? [-step, 0] : e.key === "ArrowRight" ? [step, 0] : e.key === "ArrowUp" ? [0, -step] : e.key === "ArrowDown" ? [0, step] : [0, 0];
  return dx || dy ? { dx, dy, size: e.altKey } : null;
}

/** Points for pixels on a sheet shown `sheetPx` wide for a page `pagePt` wide. */
export const toPoints = (px: number, sheetPx: number, pagePt: number) => (sheetPx > 0 ? (px * pagePt) / sheetPx : 0);

const overlaps = (a: Rect, b: Rect) => a.x < b.x + b.width && b.x < a.x + a.width && a.y < b.y + b.height && b.y < a.y + a.height;

let counter = 0;
/** A key no other box has had in this page's life. */
export const newKey = () => `n${++counter}`;

/** A new box: centred on a point when one is given (a drop, a click), else in the first free place
 *  below the top third of the page, where signatures usually are. */
export function addBox(type: FieldType, role: string, page: number, size: Size, others: Box[], at?: { x: number; y: number }): Box {
  const [width, height] = kindOf(type).size;
  const base = { key: newKey(), role, field_type: type, label: "", page, required: true };
  if (at) return { ...base, ...fit({ x: whole(at.x - width / 2), y: whole(at.y - height / 2), width, height }, size) };
  const here = others.filter((b) => b.page === page);
  let r = fit({ x: 72, y: whole(size.height * 0.4), width, height }, size);
  for (let i = 0; i < 200 && here.some((b) => overlaps(b, r)); i++) {
    const below = { ...r, y: r.y + height + 8 };
    if (below.y + height > size.height) break;
    r = below;
  }
  return { ...base, ...r };
}

/** A copy a little below and to the right of the original, so it can be told apart. */
export function copyOf(b: Box, size: Size): Box {
  return { ...b, key: newKey(), id: undefined, ...fit({ x: b.x + 12, y: b.y + 12, width: b.width, height: b.height }, size) };
}

/** The same box on every other page, where there is not one like it already (initials on each page,
 *  say). Each copy keeps its place, or the nearest the page's size allows. */
export function spread(b: Box, sizes: Size[], all: Box[]): Box[] {
  const same = (o: Box, page: number) =>
    o.page === page && o.role.trim().toLowerCase() === b.role.trim().toLowerCase() && o.field_type === b.field_type && Math.abs(o.x - b.x) < 1 && Math.abs(o.y - b.y) < 1;
  return sizes.flatMap((size, i) => (i + 1 === b.page || all.some((o) => same(o, i + 1)) ? [] : [{ ...b, key: newKey(), id: undefined, page: i + 1, ...fit(rectOf(b), size) }]));
}

const rectOf = (b: Rect): Rect => ({ x: b.x, y: b.y, width: b.width, height: b.height });

// ───────────────────────── undo ─────────────────────────

export interface History<T> {
  past: T[];
  now: T;
  future: T[];
}

export const history = <T>(now: T): History<T> => ({ past: [], now, future: [] });

/** A new present; what came before can be undone (a hundred steps back). */
export function commit<T>(h: History<T>, next: T): History<T> {
  return next === h.now ? h : { past: [...h.past, h.now].slice(-100), now: next, future: [] };
}

export const undo = <T>(h: History<T>): History<T> => (h.past.length ? { past: h.past.slice(0, -1), now: h.past[h.past.length - 1], future: [h.now, ...h.future] } : h);

export const redo = <T>(h: History<T>): History<T> => (h.future.length ? { past: [...h.past, h.now], now: h.future[0], future: h.future.slice(1) } : h);

// ───────────────────────── rows, colours, and what is missing ─────────────────────────

/** The saved rows as boxes. */
export const toBoxes = (rows: FieldRow[]): Box[] =>
  rows.map((r) => ({
    key: r.id ?? newKey(),
    id: r.id,
    role: r.role,
    field_type: r.field_type,
    label: r.label ?? "",
    page: num(r.page, 1),
    x: num(r.x),
    y: num(r.y),
    width: num(r.width, 100),
    height: num(r.height, 30),
    required: r.required !== false,
  }));

/** The rows to save: decimals as strings, as the API wants them. */
export const toRows = (boxes: Box[]) =>
  boxes.map((b) => ({
    ...(b.id ? { id: b.id } : {}),
    role: b.role,
    field_type: b.field_type,
    label: b.label.trim() || null,
    page: b.page,
    x: String(whole(b.x)),
    y: String(whole(b.y)),
    width: String(whole(b.width)),
    height: String(whole(b.height)),
    required: b.required,
  }));

/** The same boxes, as far as saving is concerned. */
export const sameBoxes = (a: Box[], b: Box[]) => JSON.stringify(toRows(a)) === JSON.stringify(toRows(b));

/** The colour a signer's boxes have (0 to 7): their place in the list. */
export const colourOf = (role: string, roles: string[]) => {
  const i = roles.findIndex((r) => r.trim().toLowerCase() === role.trim().toLowerCase());
  return (i < 0 ? 0 : i) % 8;
};

/** The roles that have no required Signature box: the server will not send until they have. */
export const lacksSignature = (boxes: Box[], roles: string[]): string[] =>
  roles.filter((r) => !boxes.some((b) => b.field_type === "Signature" && b.required && b.role.trim().toLowerCase() === r.trim().toLowerCase()));

/** A box said in words, for people who do not see it. */
export const describe = (b: Box) => (b.label ? tr("{kind} “{label}” for {role}, page {page}", { kind: kindOf(b.field_type).label, label: b.label, role: b.role, page: b.page }) : tr("{kind} for {role}, page {page}", { kind: kindOf(b.field_type).label, role: b.role, page: b.page }));
