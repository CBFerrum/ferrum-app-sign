// Placing boxes on the pages: one colour per signer, a palette to drag from, and every box a thing
// that can be picked up, stretched at its edges, nudged with the arrow keys, copied, deleted and
// brought back with undo. Nothing here needs a pointer: the palette's buttons add a box to the page
// in view, the arrow keys move it, and the inspector holds its numbers. The sheets are not pictures
// of the document (see Sheet.tsx): the PDF is read beside this screen.

import { type CSSProperties, type KeyboardEvent as ReactKeyboardEvent, type PointerEvent as ReactPointerEvent, useCallback, useRef, useState } from "react";
import { Copy, Redo2, Trash, TriangleAlert, Undo2, ZoomIn, ZoomOut } from "lucide-react";
import { Alert, Button, Field } from "@ferrum/kit";
import type { Rect, Size } from "./geometry";
import { addBox, arrow, type Box, colourOf, commit, copyOf, describe, fit, grow, type Handle, HANDLES, history, kindOf, KINDS, lacksSignature, move, redo, resize, spread, toPoints, undo, whole } from "./placement";
import { PdfPage, usePdf } from "./PdfPage";
import { boxStyle, Sheet } from "./Sheet";
import type { FieldType } from "./signing";

// ───────────────────────── the boxes, with undo ─────────────────────────

export interface Boxes {
  boxes: Box[];
  /** A new set of boxes (or a change to the current ones): one step of undo. */
  set: (next: Box[] | ((prev: Box[]) => Box[])) => void;
  undo: () => void;
  redo: () => void;
  canUndo: boolean;
  canRedo: boolean;
  /** After saving: the ids the server gave, by box key, for every step of the history. */
  adopt: (ids: Map<string, string>) => void;
  /** Starts again from these boxes. */
  reset: (boxes: Box[]) => void;
}

export function useBoxes(initial: Box[]): Boxes {
  const [h, setH] = useState(() => history(initial));
  const set = useCallback((next: Box[] | ((prev: Box[]) => Box[])) => setH((x) => commit(x, typeof next === "function" ? next(x.now) : next)), []);
  const give = (ids: Map<string, string>) => (list: Box[]) => list.map((b) => (ids.has(b.key) ? { ...b, id: ids.get(b.key) } : b));
  return {
    boxes: h.now,
    set,
    undo: () => setH(undo),
    redo: () => setH(redo),
    canUndo: h.past.length > 0,
    canRedo: h.future.length > 0,
    adopt: (ids) => setH((x) => ({ past: x.past.map(give(ids)), now: give(ids)(x.now), future: x.future.map(give(ids)) })),
    reset: (boxes) => setH(history(boxes)),
  };
}

const ZOOMS = [0.75, 1, 1.25, 1.5];

const rectOf = (b: Rect): Rect => ({ x: b.x, y: b.y, width: b.width, height: b.height });
const whole4 = (r: Rect): Rect => ({ x: whole(r.x), y: whole(r.y), width: whole(r.width), height: whole(r.height) });

// ───────────────────────── the editor ─────────────────────────

export interface Role {
  role: string;
  /** What to call them: their name, else the role. */
  label: string;
}

export function Editor({ ed, roles, pages, onPdf, document: path }: { ed: Boxes; roles: Role[]; pages: Size[]; onPdf?: () => void; document?: string | null }) {
  const { boxes } = ed;
  const pdf = usePdf(path ? () => fetch(path, { credentials: "same-origin" }).then((r) => (r.ok ? r.arrayBuffer() : Promise.reject(new Error(r.statusText)))) : null, path ?? "");
  const names = roles.map((r) => r.role);
  const [selected, setSelected] = useState<string | null>(null);
  const [active, setActive] = useState(roles[0]?.role ?? "");
  const [zoom, setZoom] = useState(1);
  const [drag, setDrag] = useState<{ key: string; rect: Rect } | null>(null);
  const [ghost, setGhost] = useState<{ type: FieldType; x: number; y: number } | null>(null);
  const [clip, setClip] = useState<Box | null>(null);
  // What was just done, for people who do not see the sheets.
  const [said, say] = useState("");
  const root = useRef<HTMLDivElement>(null);
  const sheets = useRef(new Map<number, HTMLDivElement>());
  const role = names.some((r) => r === active) ? active : (names[0] ?? "");
  const sel = boxes.find((b) => b.key === selected) ?? null;
  const missing = lacksSignature(boxes, names);

  const update = (key: string, change: Partial<Box>) => ed.set((prev) => prev.map((b) => (b.key === key ? { ...b, ...change } : b)));
  const focusBox = (key: string) =>
    requestAnimationFrame(() => {
      const el = root.current?.querySelector<HTMLElement>(`[data-box="${key}"]`);
      el?.focus({ preventScroll: true });
      el?.scrollIntoView({ block: "nearest", behavior: window.matchMedia("(prefers-reduced-motion: reduce)").matches ? "auto" : "smooth" });
    });

  /** The page whose sheet is nearest the middle of the screen. */
  const pageInView = () => {
    const mid = window.innerHeight / 2;
    let best = 1;
    let gap = Infinity;
    for (const [n, el] of sheets.current) {
      const r = el.getBoundingClientRect();
      const d = r.top <= mid && r.bottom >= mid ? 0 : Math.min(Math.abs(r.top - mid), Math.abs(r.bottom - mid));
      if (d < gap) [best, gap] = [n, d];
    }
    return best;
  };

  const add = (type: FieldType, where?: { page: number; at: { x: number; y: number } }) => {
    const page = where?.page ?? pageInView();
    const size = pages[page - 1];
    if (!size || !role) return;
    const b = addBox(type, role, page, size, boxes, where?.at);
    ed.set((prev) => [...prev, b]);
    setSelected(b.key);
    focusBox(b.key);
    say(`Added: ${describe(b)}. Arrow keys move it.`);
  };
  const remove = (key: string) => {
    const gone = boxes.find((b) => b.key === key);
    ed.set((prev) => prev.filter((b) => b.key !== key));
    setSelected(null);
    if (gone) say(`Deleted: ${describe(gone)}.`);
  };
  const everyPage = (b: Box) => {
    const copies = spread(b, pages, boxes);
    if (copies.length) ed.set((prev) => [...prev, ...copies]);
    say(copies.length ? `Copied to ${copies.length} more ${copies.length === 1 ? "page" : "pages"}.` : "It is already on every page.");
  };
  const duplicate = (b: Box) => {
    const c = copyOf(b, pages[b.page - 1] ?? { width: 612, height: 792 });
    ed.set((prev) => [...prev, c]);
    setSelected(c.key);
    focusBox(c.key);
    say(`Copied: ${describe(c)}.`);
  };

  /** Picks a box up (or one of its edges) and carries it with the pointer. */
  const begin = (e: ReactPointerEvent, b: Box, mode: "move" | Handle) => {
    if (e.pointerType === "mouse" && e.button !== 0) return;
    e.stopPropagation();
    setSelected(b.key);
    const sheet = sheets.current.get(b.page);
    const size = pages[b.page - 1];
    if (!sheet || !size) return;
    const shown = sheet.getBoundingClientRect();
    const from = rectOf(b);
    const start = { x: e.clientX, y: e.clientY };
    let last = from;
    let moved = false;
    const tick = (ev: PointerEvent) => {
      if (!moved && Math.hypot(ev.clientX - start.x, ev.clientY - start.y) < 3) return;
      moved = true;
      const dx = toPoints(ev.clientX - start.x, shown.width, size.width);
      const dy = toPoints(ev.clientY - start.y, shown.width, size.width);
      last = whole4(mode === "move" ? move(from, dx, dy, size) : resize(from, mode, dx, dy, size));
      setDrag({ key: b.key, rect: last });
    };
    const end = () => {
      window.removeEventListener("pointermove", tick);
      window.removeEventListener("pointerup", end);
      window.removeEventListener("pointercancel", end);
      setDrag(null);
      if (moved) update(b.key, last);
    };
    window.addEventListener("pointermove", tick);
    window.addEventListener("pointerup", end);
    window.addEventListener("pointercancel", end);
  };

  /** A box dragged from the palette: drops where the pointer lets go, if that is a page. */
  const carry = (type: FieldType, e: ReactPointerEvent<HTMLButtonElement>) => {
    if (e.pointerType === "mouse" && e.button !== 0) return;
    const start = { x: e.clientX, y: e.clientY };
    let dragging = false;
    const tick = (ev: PointerEvent) => {
      if (!dragging && Math.hypot(ev.clientX - start.x, ev.clientY - start.y) < 6) return;
      dragging = true;
      setGhost({ type, x: ev.clientX, y: ev.clientY });
    };
    const end = (ev: PointerEvent) => {
      window.removeEventListener("pointermove", tick);
      window.removeEventListener("pointerup", end);
      window.removeEventListener("pointercancel", end);
      setGhost(null);
      if (ev.type !== "pointerup") return;
      if (!dragging) return add(type);
      const sheet = document.elementsFromPoint(ev.clientX, ev.clientY).find((el): el is HTMLElement => el instanceof HTMLElement && el.dataset.sheet !== undefined);
      const page = Number(sheet?.dataset.sheet);
      const size = pages[page - 1];
      if (!sheet || !size) return;
      const shown = sheet.getBoundingClientRect();
      add(type, { page, at: { x: toPoints(ev.clientX - shown.left, shown.width, size.width), y: toPoints(ev.clientY - shown.top, shown.width, size.width) } });
    };
    window.addEventListener("pointermove", tick);
    window.addEventListener("pointerup", end);
    window.addEventListener("pointercancel", end);
  };

  const onKey = (e: ReactKeyboardEvent) => {
    const el = e.target as HTMLElement;
    if (el.closest("input, select, textarea, [contenteditable]")) return;
    const mod = e.metaKey || e.ctrlKey;
    const k = e.key.toLowerCase();
    if (mod && (k === "z" || k === "y")) {
      e.preventDefault();
      const again = k === "y" || e.shiftKey;
      say(again ? "Redone." : "Undone.");
      return again ? ed.redo() : ed.undo();
    }
    if (mod && k === "v" && clip) {
      e.preventDefault();
      const page = sel?.page ?? pageInView();
      const c = copyOf({ ...clip, page }, pages[page - 1] ?? { width: 612, height: 792 });
      ed.set((prev) => [...prev, c]);
      setSelected(c.key);
      return focusBox(c.key);
    }
    if (e.key === "Escape") return setSelected(null);
    // The rest acts on the box that has the focus.
    if (!sel || !el.closest("[data-box]")) return;
    if (mod && k === "d") {
      e.preventDefault();
      return duplicate(sel);
    }
    if (mod && k === "c") {
      e.preventDefault();
      say("Copied. Paste it with Ctrl or ⌘ V.");
      return setClip(sel);
    }
    if (e.key === "Delete" || e.key === "Backspace") {
      e.preventDefault();
      return remove(sel.key);
    }
    const a = arrow(e);
    if (!a) return;
    e.preventDefault();
    const size = pages[sel.page - 1];
    if (size) update(sel.key, a.size ? grow(rectOf(sel), a.dx, a.dy, size) : move(rectOf(sel), a.dx, a.dy, size));
  };

  if (!pages.length)
    return <Alert tone="error">The pages of this document couldn't be read, so boxes can't be placed. Go back and check that the PDF opens.</Alert>;
  if (!roles.length)
    return (
      <Alert tone="warning">
        There is nobody to place boxes for yet. Add the people who sign first; each gets a colour, and their boxes are placed here.
      </Alert>
    );

  return (
    <div className="sg-editor" ref={root} onKeyDown={onKey} style={{ "--zoom": zoom } as CSSProperties}>
      <div className="sg-edit-bar" role="toolbar" aria-label="Page tools">
        <Button size="sm" onClick={() => (say("Undone."), ed.undo())} disabled={!ed.canUndo} aria-label="Undo">
          <Undo2 size={15} aria-hidden="true" /> Undo
        </Button>
        <Button size="sm" onClick={() => (say("Redone."), ed.redo())} disabled={!ed.canRedo} aria-label="Redo">
          <Redo2 size={15} aria-hidden="true" /> Redo
        </Button>
        <span className="sg-edit-sep" />
        <Button size="sm" aria-label="Zoom out" disabled={zoom === ZOOMS[0]} onClick={() => setZoom(ZOOMS[Math.max(0, ZOOMS.indexOf(zoom) - 1)])}>
          <ZoomOut size={15} aria-hidden="true" />
        </Button>
        <span className="faint" aria-live="polite" style={{ minWidth: "3ch", textAlign: "center" }}>
          {Math.round(zoom * 100)}%
        </span>
        <Button size="sm" aria-label="Zoom in" disabled={zoom === ZOOMS[ZOOMS.length - 1]} onClick={() => setZoom(ZOOMS[Math.min(ZOOMS.length - 1, ZOOMS.indexOf(zoom) + 1)])}>
          <ZoomIn size={15} aria-hidden="true" />
        </Button>
        <span className="sg-edit-sep" />
        <label className="sg-goto">
          <span className="faint">Go to page</span>
          <select
            className="select"
            value=""
            aria-label="Go to page"
            onChange={(e) => {
              document.getElementById(`page-${e.target.value}`)?.scrollIntoView({ block: "start" });
              e.target.value = "";
            }}
          >
            <option value="" disabled>
              {pages.length} {pages.length === 1 ? "page" : "pages"}
            </option>
            {pages.map((_, i) => (
              <option key={i} value={i + 1}>
                Page {i + 1}
                {boxes.some((b) => b.page === i + 1) ? ` (${boxes.filter((b) => b.page === i + 1).length})` : ""}
              </option>
            ))}
          </select>
        </label>
        <span className="spacer" />
        {onPdf && (
          <Button size="sm" onClick={onPdf}>
            Download the PDF
          </Button>
        )}
      </div>

      <div className="sg-edit-body">
        <div className={pages.length > 40 ? "sg-stage sg-lazy" : "sg-stage"}>
          {!pdf && <p className="sg-note">The sheets are the size of your pages, not pictures of them. Keep the PDF open beside this screen to see where things go on each page.</p>}
          {pages.map((size, i) => {
            const n = i + 1;
            const here = boxes.filter((b) => b.page === n);
            return (
              <Sheet
                key={n}
                number={n}
                count={pages.length}
                size={size}
                picture={pdf && <PdfPage pdf={pdf} number={n} />}
                note={here.length ? `${here.length} ${here.length === 1 ? "box" : "boxes"}` : undefined}
                inner={(el) => {
                  if (el) sheets.current.set(n, el);
                  else sheets.current.delete(n);
                }}
              >
                {here.map((b) => {
                  const live = drag?.key === b.key ? drag.rect : b;
                  const picked = b.key === selected;
                  return (
                    <div
                      key={b.key}
                      data-box={b.key}
                      role="button"
                      tabIndex={0}
                      aria-label={describe(b)}
                      aria-pressed={picked}
                      className={`sg-edit sg-c${colourOf(b.role, names)}`}
                      data-selected={picked || undefined}
                      data-kind={b.field_type}
                      data-moving={drag?.key === b.key || undefined}
                      style={boxStyle(live, size)}
                      onPointerDown={(e) => begin(e, b, "move")}
                      onFocus={() => setSelected(b.key)}
                    >
                      <span className="sg-edit-label">{b.label.trim() || kindOf(b.field_type).label}</span>
                      {!b.required && b.field_type !== "Date Signed" && b.field_type !== "Name" && <span className="sg-edit-opt">optional</span>}
                      {picked && HANDLES.map((h) => <span key={h} className="sg-handle" data-h={h} aria-hidden="true" onPointerDown={(e) => begin(e, b, h)} />)}
                    </div>
                  );
                })}
              </Sheet>
            );
          })}
        </div>

        <aside className="sg-edit-side" aria-label="Boxes">
          <section className="sg-side-block" aria-labelledby="ed-who">
            <h3 id="ed-who">Who signs here?</h3>
            <ul className="sg-who">
              {roles.map((r) => {
                const count = boxes.filter((b) => b.role.trim().toLowerCase() === r.role.trim().toLowerCase()).length;
                const lacks = missing.some((m) => m.trim().toLowerCase() === r.role.trim().toLowerCase());
                return (
                  <li key={r.role}>
                    <button type="button" className={`sg-who-pick sg-c${colourOf(r.role, names)}`} aria-pressed={r.role === role} onClick={() => setActive(r.role)}>
                      <span className="sg-dot" aria-hidden="true" />
                      <span className="sg-who-name">
                        <strong>{r.label}</strong>
                        {r.label !== r.role && <small>{r.role}</small>}
                      </span>
                      <span className="faint">{count}</span>
                      {lacks && (
                        <span className="sg-lacks" title="Needs a required Signature box before the request can be sent">
                          <TriangleAlert size={14} aria-hidden="true" />
                          <span className="sr-only">has no signature box yet</span>
                        </span>
                      )}
                    </button>
                  </li>
                );
              })}
            </ul>
          </section>

          <section className="sg-side-block" aria-labelledby="ed-add">
            <h3 id="ed-add">Add a box</h3>
            <div className={`sg-palette sg-c${colourOf(role, names)}`}>
              {KINDS.map((k) => (
                <button
                  key={k.type}
                  type="button"
                  className="sg-pal"
                  title={k.hint}
                  onPointerDown={(e) => carry(k.type, e)}
                  // A keyboard press is a click with no pointer: it adds the box to the page in view.
                  onClick={(e) => e.detail === 0 && add(k.type)}
                >
                  <span className="sg-dot" aria-hidden="true" />
                  {k.label}
                </button>
              ))}
            </div>
            <p className="faint" style={{ fontSize: 12.5 }}>
              Click to add one to the page in view, or drag it onto a page. For {roles.find((r) => r.role === role)?.label ?? role}.
            </p>
          </section>

          {sel ? (
            <Inspector
              key={sel.key}
              box={sel}
              roles={roles}
              pages={pages}
              onChange={(change) => update(sel.key, change)}
              onDuplicate={() => duplicate(sel)}
              onEveryPage={pages.length > 1 ? () => everyPage(sel) : undefined}
              onRemove={() => remove(sel.key)}
              onDone={() => setSelected(null)}
            />
          ) : (
            <section className="sg-side-block">
              <h3>Keyboard</h3>
              <p className="faint" style={{ fontSize: 12.5, lineHeight: 1.5 }}>
                Tab to a box. Arrow keys move it a point (Shift: ten); Alt with the arrows changes its size. Delete removes it, Ctrl or ⌘ with D copies it in place, C and V copy and paste, Z undoes.
              </p>
            </section>
          )}
        </aside>
      </div>
      <div className="sr-only" role="status" aria-live="polite">
        {said}
      </div>
      {ghost && (
        <div className={`sg-ghost sg-c${colourOf(role, names)}`} style={{ left: ghost.x, top: ghost.y }} aria-hidden="true">
          {kindOf(ghost.type).label}
        </div>
      )}
    </div>
  );
}

// ───────────────────────── the selected box ─────────────────────────

/** A number that can be cleared and retyped: it follows the box while it is a number, and shows
 *  the box's again when the field is left. */
function Num({ id, label, value, max, onValue }: { id: string; label: string; value: number; max: number; onValue: (v: number) => void }) {
  const [typed, setTyped] = useState<string | null>(null);
  return (
    <Field label={label} htmlFor={id}>
      <input
        id={id}
        className="input num"
        type="number"
        min={0}
        max={max}
        step={1}
        value={typed ?? String(value)}
        onChange={(e) => {
          setTyped(e.target.value);
          const v = Number(e.target.value);
          if (e.target.value !== "" && Number.isFinite(v)) onValue(v);
        }}
        onBlur={() => setTyped(null)}
      />
    </Field>
  );
}

function Inspector({ box: b, roles, pages, onChange, onDuplicate, onEveryPage, onRemove, onDone }: { box: Box; roles: Role[]; pages: Size[]; onChange: (c: Partial<Box>) => void; onDuplicate: () => void; onEveryPage?: () => void; onRemove: () => void; onDone: () => void }) {
  const size = pages[b.page - 1] ?? { width: 612, height: 792 };
  const auto = b.field_type === "Date Signed" || b.field_type === "Name";
  // Numbers are kept on the page as they are typed.
  const set = (field: keyof Rect, v: number) => onChange(fit({ ...rectOf(b), [field]: v }, size));
  const num = (field: keyof Rect, label: string, max: number) => <Num key={field} id={`ins-${field}`} label={label} value={whole(b[field])} max={Math.round(max)} onValue={(v) => set(field, v)} />;
  return (
    <section className="sg-side-block sg-inspector" aria-labelledby="ins-head">
      <h3 id="ins-head">This box</h3>
      <div className="stack" style={{ gap: 10 }}>
        <Field label="For" htmlFor="ins-role">
          <select id="ins-role" className="select" value={b.role} onChange={(e) => onChange({ role: e.target.value })}>
            {roles.map((r) => (
              <option key={r.role} value={r.role}>
                {r.label}
              </option>
            ))}
          </select>
        </Field>
        <Field label="Kind" htmlFor="ins-kind">
          <select id="ins-kind" className="select" value={b.field_type} onChange={(e) => onChange({ field_type: e.target.value as FieldType })}>
            {KINDS.map((k) => (
              <option key={k.type} value={k.type}>
                {k.label}
              </option>
            ))}
          </select>
        </Field>
        {!auto && (
          <Field label="Label" htmlFor="ins-label" hint="What the signer sees, such as “Company name”.">
            <input id="ins-label" className="input" value={b.label} maxLength={100} onChange={(e) => onChange({ label: e.target.value })} />
          </Field>
        )}
        {!auto && (
          <label className="check-field">
            <input type="checkbox" checked={b.required} onChange={(e) => onChange({ required: e.target.checked })} />
            <span>Required</span>
          </label>
        )}
        <div className="form-grid" style={{ "--cols": 2, gap: "8px 12px" } as CSSProperties}>
          <Field label="Page" htmlFor="ins-page">
            <input
              id="ins-page"
              className="input num"
              type="number"
              min={1}
              max={pages.length}
              value={b.page}
              onChange={(e) => {
                const page = Math.min(Math.max(Math.round(Number(e.target.value)), 1), pages.length);
                if (Number.isFinite(page) && e.target.value !== "") onChange({ page, ...fit(rectOf(b), pages[page - 1] ?? size) });
              }}
            />
          </Field>
          <span />
          {num("x", "From left", size.width)}
          {num("y", "From top", size.height)}
          {num("width", "Width", size.width)}
          {num("height", "Height", size.height)}
        </div>
        <p className="faint" style={{ fontSize: 12 }}>
          In points (72 to the inch) from the top left of page {b.page}.
        </p>
        <div className="row">
          <Button size="sm" onClick={onDuplicate}>
            <Copy size={14} aria-hidden="true" /> Copy
          </Button>
          {onEveryPage && (
            <Button size="sm" title="Puts the same box in the same place on every other page" onClick={onEveryPage}>
              On every page
            </Button>
          )}
          <Button size="sm" variant="danger" onClick={onRemove}>
            <Trash size={14} aria-hidden="true" /> Delete
          </Button>
          <span className="spacer" />
          <Button size="sm" onClick={onDone}>
            Done
          </Button>
        </div>
      </div>
    </section>
  );
}

