import { describe, expect, it } from "vitest";
import { exportFrame, hasInk, inkBounds, pathLength, type Pt } from "./ink";

// A flourish across the middle of a 400 × 200 canvas.
const stroke: Pt[] = [
  [0.25, 0.5],
  [0.5, 0.25],
  [0.75, 0.5],
];

describe("a drawn signature", () => {
  it("measures how far the pen travelled, in pixels", () => {
    expect(pathLength([[[0, 0], [0.5, 0]]], 400, 200)).toBe(200);
    expect(pathLength([[[0.1, 0.1]]], 400, 200)).toBe(0);
    expect(Math.round(pathLength([stroke], 400, 200))).toBe(Math.round(2 * Math.hypot(100, 50)));
  });

  it("takes a touch for no signature", () => {
    expect(hasInk([], 400, 200)).toBe(false);
    expect(hasInk([[[0.5, 0.5]]], 400, 200)).toBe(false);
    expect(hasInk([[[0.5, 0.5], [0.52, 0.5]]], 400, 200)).toBe(false);
    expect(hasInk([stroke], 400, 200)).toBe(true);
  });

  it("finds the ink, and a pen's width around it", () => {
    expect(inkBounds([], 400, 200, 3)).toBeNull();
    expect(inkBounds([stroke], 400, 200, 4)).toEqual({ x: 98, y: 48, width: 204, height: 54 });
  });

  it("cuts the picture to the ink with a margin, at twice the size, and shrinks a wide one", () => {
    const b = inkBounds([stroke], 400, 200, 4)!;
    expect(exportFrame(b, 6, 800)).toEqual({ box: { x: 92, y: 42, width: 216, height: 66 }, width: 432, height: 132 });
    const wide = exportFrame({ x: 0, y: 0, width: 1600, height: 400 }, 0, 800);
    expect([wide.width, wide.height]).toEqual([800, 200]);
    const mid = exportFrame({ x: 0, y: 0, width: 600, height: 150 }, 0, 800);
    expect([mid.width, mid.height]).toEqual([800, 200]);
  });

  it("keeps a tiny mark at the size the server accepts, centred", () => {
    const tiny = exportFrame({ x: 100, y: 100, width: 4, height: 4 }, 2, 800);
    expect(tiny.width).toBeGreaterThanOrEqual(24);
    expect(tiny.height).toBeGreaterThanOrEqual(12);
    expect(tiny.box.x + tiny.box.width / 2).toBe(102);
    expect(tiny.box.y + tiny.box.height / 2).toBe(102);
  });
});
