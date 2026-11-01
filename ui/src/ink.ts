// A signature drawn on a canvas: strokes are kept as fractions of the canvas, so they survive a
// resize (turning a phone) and are cut to the ink when sent, which makes the stamped signature fill
// its box instead of floating in a blank rectangle. Pure, so it is tested without a canvas.

/** A point as fractions of the canvas width and height. */
export type Pt = [number, number];

export interface Box {
  x: number;
  y: number;
  width: number;
  height: number;
}

/** How long the pen has travelled, in pixels. */
export function pathLength(strokes: Pt[][], w: number, h: number): number {
  let total = 0;
  for (const s of strokes) for (let i = 1; i < s.length; i++) total += Math.hypot((s[i][0] - s[i - 1][0]) * w, (s[i][1] - s[i - 1][1]) * h);
  return total;
}

/** Enough ink to be a signature rather than a touch: the server refuses an image with next to none. */
export const hasInk = (strokes: Pt[][], w: number, h: number) => pathLength(strokes, w, h) >= 24;

/** The ink's bounds in pixels, grown by half the pen's width; null when nothing is drawn. */
export function inkBounds(strokes: Pt[][], w: number, h: number, pen: number): Box | null {
  let [x0, y0, x1, y1] = [Infinity, Infinity, -Infinity, -Infinity];
  for (const s of strokes)
    for (const [fx, fy] of s) {
      x0 = Math.min(x0, fx * w);
      y0 = Math.min(y0, fy * h);
      x1 = Math.max(x1, fx * w);
      y1 = Math.max(y1, fy * h);
    }
  if (!Number.isFinite(x0)) return null;
  const r = pen / 2;
  return { x: x0 - r, y: y0 - r, width: x1 - x0 + pen, height: y1 - y0 + pen };
}

/** The picture that is sent: the ink's bounds with a margin, drawn at twice the size it was
 *  (sharp when a viewer zooms in) but at most `maxWidth` wide, and never below the server's
 *  smallest (20 × 10 pixels). `box` is where it comes from on the canvas. */
export function exportFrame(bounds: Box, margin: number, maxWidth: number): { box: Box; width: number; height: number } {
  const box = { x: bounds.x - margin, y: bounds.y - margin, width: Math.max(bounds.width + 2 * margin, 24), height: Math.max(bounds.height + 2 * margin, 12) };
  // A short mark is centred in the minimum size instead of pushed to a corner.
  box.x -= Math.max(0, box.width - (bounds.width + 2 * margin)) / 2;
  box.y -= Math.max(0, box.height - (bounds.height + 2 * margin)) / 2;
  const scale = Math.min(2, maxWidth / box.width);
  return { box, width: Math.max(24, Math.round(box.width * scale)), height: Math.max(12, Math.round(box.height * scale)) };
}
