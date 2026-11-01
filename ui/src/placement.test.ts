import { describe, expect, it } from "vitest";
import { addBox, arrow, type Box, colourOf, commit, copyOf, describe as say, fit, grow, history, lacksSignature, move, redo, resize, sameBoxes, spread, toBoxes, toPoints, toRows, undo } from "./placement";

const page = { width: 612, height: 792 };
const box = (over: Partial<Box> = {}): Box => ({ key: "k", role: "Client", field_type: "Signature", label: "", page: 1, x: 100, y: 100, width: 160, height: 44, required: true, ...over });

describe("a box on its page", () => {
  it("stays inside the page and no smaller than the server allows", () => {
    expect(fit({ x: -5, y: 900, width: 100, height: 50 }, page)).toEqual({ x: 0, y: 742, width: 100, height: 50 });
    expect(fit({ x: 10, y: 10, width: 1, height: 2 }, page)).toEqual({ x: 10, y: 10, width: 4, height: 4 });
    expect(fit({ x: 0, y: 0, width: 1000, height: 1000 }, page)).toEqual({ x: 0, y: 0, width: 612, height: 792 });
  });

  it("moves, and stops at the edges", () => {
    expect(move({ x: 10, y: 10, width: 100, height: 40 }, 5, 5, page)).toEqual({ x: 15, y: 15, width: 100, height: 40 });
    expect(move({ x: 10, y: 10, width: 100, height: 40 }, -50, 1000, page)).toEqual({ x: 0, y: 752, width: 100, height: 40 });
  });

  const r = { x: 100, y: 100, width: 50, height: 20 };
  it("grows from the edge or corner it is held by, the other side staying put", () => {
    expect(resize(r, "e", 30, 0, page)).toEqual({ x: 100, y: 100, width: 80, height: 20 });
    expect(resize(r, "w", 20, 0, page)).toEqual({ x: 120, y: 100, width: 30, height: 20 });
    expect(resize(r, "n", 0, -30, page)).toEqual({ x: 100, y: 70, width: 50, height: 50 });
    expect(resize(r, "se", 10, 5, page)).toEqual({ x: 100, y: 100, width: 60, height: 25 });
    expect(resize(r, "nw", -10, -10, page)).toEqual({ x: 90, y: 90, width: 60, height: 30 });
  });

  it("does not turn inside out: past the minimum the far side holds", () => {
    expect(resize(r, "e", -100, 0, page)).toEqual({ x: 100, y: 100, width: 4, height: 20 });
    expect(resize(r, "w", 60, 0, page)).toEqual({ x: 146, y: 100, width: 4, height: 20 });
    expect(resize(r, "n", 0, 90, page)).toEqual({ x: 100, y: 116, width: 50, height: 4 });
  });

  it("does not leave the page when held by a corner", () => {
    expect(resize(r, "nw", -120, 0, page)).toEqual({ x: 0, y: 100, width: 150, height: 20 });
    expect(resize({ x: 600, y: 780, width: 10, height: 10 }, "se", 50, 50, page)).toEqual({ x: 600, y: 780, width: 12, height: 12 });
  });

  it("changes size from the keyboard without shifting", () => {
    expect(grow({ x: 600, y: 100, width: 10, height: 10 }, 20, 0, page)).toEqual({ x: 600, y: 100, width: 12, height: 10 });
    expect(grow(r, -10, 5, page)).toEqual({ x: 100, y: 100, width: 40, height: 25 });
  });

  it("reads the arrow keys: a point, ten with Shift, size with Alt", () => {
    const key = (k: string, shiftKey = false, altKey = false) => arrow({ key: k, shiftKey, altKey });
    expect(key("ArrowLeft")).toEqual({ dx: -1, dy: 0, size: false });
    expect(key("ArrowDown", true)).toEqual({ dx: 0, dy: 10, size: false });
    expect(key("ArrowRight", true, true)).toEqual({ dx: 10, dy: 0, size: true });
    expect(key("Enter")).toBeNull();
    expect(key("a")).toBeNull();
  });

  it("turns screen pixels into points", () => {
    expect(toPoints(50, 500, 612)).toBeCloseTo(61.2);
    expect(toPoints(50, 0, 612)).toBe(0);
  });
});

describe("a new box", () => {
  it("lands centred where it was dropped, on the page", () => {
    const b = addBox("Signature", "Client", 2, page, [], { x: 300, y: 400 });
    expect([b.page, b.x, b.y, b.width, b.height, b.role, b.required]).toEqual([2, 220, 378, 160, 44, "Client", true]);
    const corner = addBox("Signature", "Client", 1, page, [], { x: 0, y: 0 });
    expect([corner.x, corner.y]).toEqual([0, 0]);
  });

  it("is put in a free place when there is no drop, below what is already there", () => {
    const first = addBox("Signature", "Client", 1, page, []);
    expect([first.x, first.y]).toEqual([72, 317]);
    const second = addBox("Signature", "Company", 1, page, [first]);
    expect([second.x, second.y]).toEqual([72, 369]);
    const other = addBox("Signature", "Company", 2, page, [first]);
    expect([other.x, other.y]).toEqual([72, 317]);
  });

  it("is given up on the page when it is full, not looped on", () => {
    const tiny = { width: 200, height: 60 };
    const a = addBox("Signature", "A", 1, tiny, []);
    const b = addBox("Signature", "A", 1, tiny, [a]);
    expect(b.y + b.height).toBeLessThanOrEqual(60);
  });

  it("is a copy a little to one side, with no saved id", () => {
    const c = copyOf(box({ id: "abc", x: 590, y: 20 }), page);
    expect(c.id).toBeUndefined();
    expect(c.key).not.toBe("k");
    expect([c.x, c.y]).toEqual([452, 32]);
  });
});

describe("a box on every page", () => {
  const sizes = [page, page, { width: 200, height: 300 }];
  it("is copied to each other page, in the same place or the nearest that fits", () => {
    const b = box({ field_type: "Initials", x: 500, y: 700, width: 56, height: 32 });
    const copies = spread(b, sizes, [b]);
    expect(copies.map((c) => [c.page, c.x, c.y, c.width, c.height])).toEqual([
      [2, 500, 700, 56, 32],
      [3, 144, 268, 56, 32],
    ]);
    expect(copies.every((c) => c.id === undefined && c.key !== b.key && c.role === "Client" && c.field_type === "Initials")).toBe(true);
  });

  it("leaves pages that already have one like it, and boxes of other kinds alone", () => {
    const b = box({ field_type: "Initials", x: 500, y: 700 });
    const there = box({ key: "z", field_type: "Initials", page: 2, x: 500, y: 700 });
    const other = box({ key: "y", field_type: "Text", page: 3, x: 500, y: 700 });
    expect(spread(b, sizes, [b, there, other]).map((c) => c.page)).toEqual([3]);
    expect(spread(b, [page], [b])).toEqual([]);
  });
});

describe("undo", () => {
  it("walks back and forth, and a new change drops what was undone", () => {
    let h = history("a");
    h = commit(h, "b");
    h = commit(h, "c");
    expect(h.now).toBe("c");
    h = undo(h);
    expect(h.now).toBe("b");
    h = undo(h);
    expect([h.now, undo(h) === h]).toEqual(["a", true]);
    h = redo(h);
    expect(h.now).toBe("b");
    h = commit(h, "d");
    expect([h.now, redo(h) === h, h.past]).toEqual(["d", true, ["a", "b"]]);
  });

  it("does not remember a change that changed nothing, nor more than a hundred", () => {
    const h = history(0);
    expect(commit(h, 0)).toBe(h);
    let long = h;
    for (let i = 1; i <= 150; i++) long = commit(long, i);
    expect(long.past).toHaveLength(100);
  });
});

describe("rows and colours", () => {
  it("round trips what the API gives, with whole points and decimals as strings", () => {
    const rows = [{ id: "a", role: "Client", field_type: "Text" as const, label: " Name ", page: 2, x: "72.40", y: "100", width: "160.00", height: "22", required: false }];
    const boxes = toBoxes(rows);
    expect(boxes[0]).toMatchObject({ key: "a", id: "a", x: 72.4, width: 160, required: false, label: " Name " });
    expect(toRows(boxes)).toEqual([{ id: "a", role: "Client", field_type: "Text", label: "Name", page: 2, x: "72", y: "100", width: "160", height: "22", required: false }]);
    expect(toRows([box()])[0]).not.toHaveProperty("id");
    expect(toBoxes([{ role: "A", field_type: "Name", page: 1, x: 1, y: 2, width: 3, height: 4 }])[0].required).toBe(true);
  });

  it("tells whether boxes changed as far as saving goes", () => {
    expect(sameBoxes([box()], [box({ key: "other", label: "  " })])).toBe(true);
    expect(sameBoxes([box()], [box({ x: 101 })])).toBe(false);
    expect(sameBoxes([box({ x: 100.2 })], [box({ x: 99.8 })])).toBe(true);
  });

  it("gives each signer a colour by their place", () => {
    const roles = ["Client", "Company"];
    expect([colourOf("Client", roles), colourOf(" company ", roles), colourOf("Nobody", roles)]).toEqual([0, 1, 0]);
    expect(colourOf("n9", ["a", "b", "c", "d", "e", "f", "g", "h", "n9"])).toBe(0);
  });

  it("names the signers who have no required signature box", () => {
    const boxes = [box(), box({ role: "Company", required: false }), box({ role: "Witness", field_type: "Initials" })];
    expect(lacksSignature(boxes, ["client", "Company", "Witness"])).toEqual(["Company", "Witness"]);
    expect(lacksSignature(boxes, [])).toEqual([]);
  });

  it("says a box in words", () => {
    expect(say(box({ field_type: "Text", label: "Company name", role: "Company", page: 3 }))).toBe("Text “Company name” for Company, page 3");
    expect(say(box())).toBe("Signature for Client, page 1");
  });
});
