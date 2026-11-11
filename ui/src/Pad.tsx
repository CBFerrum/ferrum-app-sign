// Drawing or typing a signature (or initials): one pad, two ways in. Drawing is a canvas for a
// finger, a pen or a mouse; typing is for anyone who cannot or would rather not draw (and the only
// way with a keyboard alone). Either gives a mark: a picture cut to the ink, or the text.

import { useCallback, useEffect, useMemo, useRef, useState, type PointerEvent as ReactPointerEvent } from "react";
import { Button, Tabs } from "@ferrum/kit";
import { exportFrame, hasInk, inkBounds, type Pt } from "./ink";
import type { Mark } from "./signing";
import { t as tr, isolate } from "@ferrum/client";

/** The pen's width in CSS pixels, the margin kept round the ink and the widest picture sent. */
const PEN = 2.6;
const MARGIN = 6;
const MAX_WIDTH = 800;

function strokesOn(g: CanvasRenderingContext2D, strokes: Pt[][], w: number, h: number, ink: string) {
  g.lineWidth = PEN;
  g.lineCap = "round";
  g.lineJoin = "round";
  g.strokeStyle = ink;
  g.fillStyle = ink;
  for (const s of strokes) {
    if (s.length === 1) {
      g.beginPath();
      g.arc(s[0][0] * w, s[0][1] * h, PEN / 2, 0, Math.PI * 2);
      g.fill();
      continue;
    }
    // Curves through the midpoints of the samples, so a quick stroke is smooth rather than angular.
    g.beginPath();
    g.moveTo(s[0][0] * w, s[0][1] * h);
    for (let i = 1; i < s.length - 1; i++) g.quadraticCurveTo(s[i][0] * w, s[i][1] * h, ((s[i][0] + s[i + 1][0]) / 2) * w, ((s[i][1] + s[i + 1][1]) / 2) * h);
    const last = s[s.length - 1];
    g.lineTo(last[0] * w, last[1] * h);
    g.stroke();
  }
}

function Drawing({ height, hint, onChange }: { height: string; hint: string; onChange: (png: string | null) => void }) {
  const canvas = useRef<HTMLCanvasElement>(null);
  const strokes = useRef<Pt[][]>([]);
  const live = useRef<Pt[] | null>(null);
  const [drawn, setDrawn] = useState(0);

  const ink = () => (canvas.current ? getComputedStyle(canvas.current).color : "currentColor");

  const paint = useCallback(() => {
    const c = canvas.current;
    const g = c?.getContext("2d");
    // Hidden (the other tab is showing) it has no size: it is drawn again when it is back.
    if (!c || !g || !c.clientWidth) return;
    g.setTransform(1, 0, 0, 1, 0, 0);
    g.clearRect(0, 0, c.width, c.height);
    const dpr = c.width / c.clientWidth;
    g.setTransform(dpr, 0, 0, dpr, 0, 0);
    strokesOn(g, strokes.current, c.clientWidth, c.clientHeight, ink());
  }, []);

  // The canvas keeps its picture sharp at the screen's density and follows its box when that changes.
  useEffect(() => {
    const c = canvas.current;
    if (!c) return;
    const fit = () => {
      const dpr = Math.min(window.devicePixelRatio || 1, 3);
      c.width = Math.max(1, Math.round(c.clientWidth * dpr));
      c.height = Math.max(1, Math.round(c.clientHeight * dpr));
      paint();
    };
    fit();
    const watch = new ResizeObserver(fit);
    watch.observe(c);
    return () => watch.disconnect();
  }, [paint]);

  const publish = () => {
    const c = canvas.current;
    setDrawn(strokes.current.length);
    if (!c || !hasInk(strokes.current, c.clientWidth, c.clientHeight)) return onChange(null);
    const [w, h] = [c.clientWidth, c.clientHeight];
    const bounds = inkBounds(strokes.current, w, h, PEN);
    if (!bounds) return onChange(null);
    const f = exportFrame(bounds, MARGIN, MAX_WIDTH);
    const out = document.createElement("canvas");
    out.width = f.width;
    out.height = f.height;
    const g = out.getContext("2d");
    if (!g) return onChange(null);
    g.scale(f.width / f.box.width, f.height / f.box.height);
    g.translate(-f.box.x, -f.box.y);
    strokesOn(g, strokes.current, w, h, ink());
    onChange(out.toDataURL("image/png"));
  };

  const at = (e: { clientX: number; clientY: number }): Pt => {
    const r = canvas.current!.getBoundingClientRect();
    return [(e.clientX - r.left) / r.width, (e.clientY - r.top) / r.height];
  };
  const down = (e: ReactPointerEvent<HTMLCanvasElement>) => {
    if (e.pointerType === "mouse" && e.button !== 0) return;
    e.currentTarget.setPointerCapture(e.pointerId);
    live.current = [at(e)];
    strokes.current.push(live.current);
    setDrawn(strokes.current.length);
    paint();
  };
  const move = (e: ReactPointerEvent<HTMLCanvasElement>) => {
    if (!live.current) return;
    // A fast pen reports many positions between two events: all of them make the line smooth.
    for (const p of e.nativeEvent.getCoalescedEvents?.() ?? [e.nativeEvent]) live.current.push(at(p));
    paint();
  };
  const up = () => {
    if (!live.current) return;
    live.current = null;
    publish();
  };
  const undo = () => {
    strokes.current.pop();
    paint();
    publish();
  };
  const clear = () => {
    strokes.current = [];
    paint();
    publish();
  };

  return (
    <div className="stack" style={{ gap: 8 }}>
      <div className="sg-paper sg-canvas" style={{ height }}>
        <canvas
          ref={canvas}
          aria-label={tr("Drawing area for your signature. To type it instead, use the Type tab.")}
          onPointerDown={down}
          onPointerMove={move}
          onPointerUp={up}
          onPointerCancel={up}
        />
        {drawn === 0 && <span className="sg-canvas-hint">{isolate(hint)}</span>}
      </div>
      <div className="row">
        <Button size="sm" onClick={undo} disabled={drawn === 0}>
          {tr("Undo")}
        </Button>
        <Button size="sm" onClick={clear} disabled={drawn === 0}>
          {tr("Clear")}
        </Button>
        <span className="faint" style={{ fontSize: "var(--text-xs)" }}>
          {tr("Use your finger, a pen or the mouse.")}
        </span>
      </div>
    </div>
  );
}

const hasLetter = (t: string) => /\p{L}/u.test(t);

/** Draw or type a mark. `onChange` gets the mark as it stands, or null while there is none. */
export function MarkPad({ kind, typed: start, onChange }: { kind: "signature" | "initials"; typed: string; onChange: (m: Mark | null) => void }) {
  const [mode, setMode] = useState<"draw" | "type">("draw");
  const [png, setPng] = useState<string | null>(null);
  const [text, setText] = useState(start);
  const mark = useMemo<Mark | null>(() => {
    if (mode === "draw") return png ? { image: png } : null;
    const t = text.trim();
    return t && hasLetter(t) && t.length <= 100 ? { text: t } : null;
  }, [mode, png, text]);
  useEffect(() => onChange(mark), [mark, onChange]);

  const what = kind === "signature" ? "signature" : "initials";
  return (
    <div className="stack" style={{ gap: 12 }}>
      <Tabs
        id="mark"
        tabs={[
          { id: "draw", label: tr("Draw") },
          { id: "type", label: tr("Type") },
        ]}
        value={mode}
        onChange={(m) => setMode(m as "draw" | "type")}
      />
      {/* Both stay mounted: what was drawn is still there after a look at the other tab. */}
      <div role="tabpanel" id="mark-panel-draw" aria-labelledby="mark-tab-draw" hidden={mode !== "draw"}>
        <Drawing height={kind === "signature" ? "11rem" : "7.5rem"} hint={tr("Draw your {what} here", { what })} onChange={setPng} />
      </div>
      <div role="tabpanel" id="mark-panel-type" aria-labelledby="mark-tab-type" hidden={mode !== "type"}>
        <div className="stack" style={{ gap: 8 }}>
          <label className="field-label" htmlFor="mark-text">
            {(kind === "signature" ? tr("Type your full name") : tr("Type your initials"))}
          </label>
          <input id="mark-text" className="input" value={text} maxLength={kind === "signature" ? 100 : 10} autoComplete="off" onChange={(e) => setText(e.target.value)} />
          <div className="sg-paper sg-typed" aria-hidden="true">
            {text.trim() || <span className="sg-canvas-hint">{tr("Your {what} appears here", { what })}</span>}
          </div>
          <p className="faint" style={{ fontSize: "var(--text-xs)" }}>
            {tr("A typed {what} is stamped on the document in italic type, as shown.", { what })}
          </p>
        </div>
      </div>
    </div>
  );
}

/** A mark as it shows on a box or in a summary. */
export function MarkView({ mark, className }: { mark: Mark; className?: string }) {
  return "image" in mark ? <img className={`sg-mark ${className ?? ""}`} src={mark.image} alt="" /> : <span className={`sg-mark sg-mark-text ${className ?? ""}`}>{isolate(mark.text)}</span>;
}
