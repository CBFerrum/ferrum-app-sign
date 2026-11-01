import { describe, expect, it } from "vitest";
import { placement } from "./geometry";

describe("a box on its sheet", () => {
  const letter = { width: 612, height: 792 };

  it("is placed by shares of the page, so it follows the sheet's size", () => {
    const p = placement({ x: 72, y: 396, width: 153, height: 79.2 }, letter);
    expect([p.left, p.top, p.width, p.height]).toEqual(["11.765%", "50.000%", "25.000%", "10.000%"]);
  });

  it("scales its text with the page but not past what reads well", () => {
    expect(placement({ x: 0, y: 0, width: 50, height: 10 }, letter)["--fs"]).toBe("0.980cqw");
    expect(placement({ x: 0, y: 0, width: 50, height: 100 }, letter)["--fs"]).toBe("2.288cqw");
  });

  it("is the same box on a landscape page", () => {
    const p = placement({ x: 100, y: 50, width: 200, height: 40 }, { width: 800, height: 400 });
    expect([p.left, p.top, p.width, p.height]).toEqual(["12.500%", "12.500%", "25.000%", "10.000%"]);
  });
});
