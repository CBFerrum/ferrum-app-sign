import { describe, expect, it } from "vitest";
import { plural, timeLeft } from "./time";

describe("time left", () => {
  const now = Date.UTC(2026, 10, 1, 12, 0, 0);
  const at = (ms: number) => new Date(now + ms).toISOString();
  const H = 3_600_000;

  it("counts down in the unit that matters", () => {
    expect(timeLeft(at(10 * 24 * H), now)).toEqual({ text: "10 days left", level: "ok" });
    expect(timeLeft(at(3 * 24 * H), now)).toEqual({ text: "3 days left", level: "ok" });
    expect(timeLeft(at(2 * 24 * H + H), now)).toEqual({ text: "2 days left", level: "soon" });
    expect(timeLeft(at(25 * H), now)).toEqual({ text: "1 day left", level: "soon" });
    expect(timeLeft(at(5 * H + 1), now)).toEqual({ text: "5 hours left", level: "soon" });
    expect(timeLeft(at(H), now)).toEqual({ text: "1 hour left", level: "soon" });
    expect(timeLeft(at(20 * 60_000), now)).toEqual({ text: "Under an hour left", level: "soon" });
  });

  it("says when it has passed, and says nothing without a deadline", () => {
    expect(timeLeft(at(-1000), now)).toEqual({ text: "Expired", level: "late" });
    expect(timeLeft(at(0), now)).toEqual({ text: "Expired", level: "late" });
    expect(timeLeft(null, now)).toBeNull();
    expect(timeLeft("soon", now)).toBeNull();
  });

  it("pluralises", () => {
    expect([plural(0, "reminder"), plural(1, "reminder"), plural(2, "person", "people")]).toEqual(["0 reminders", "1 reminder", "2 people"]);
  });
});
