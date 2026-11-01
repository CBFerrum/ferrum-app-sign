import { describe, expect, it } from "vitest";
import {
  actionLabel,
  boxState,
  classify,
  closedCopy,
  complete,
  emptyEntry,
  initialsOf,
  locate,
  nextTask,
  pagesText,
  pagesWithFields,
  problemsOf,
  progress,
  signBody,
  type SigningField,
  tasksOf,
} from "./signing";

const box = (id: string, field_type: SigningField["field_type"], page: number, y: number, x = 72, extra: Partial<SigningField> = {}): SigningField => ({
  id,
  field_type,
  label: null,
  page,
  x,
  y,
  width: 120,
  height: 30,
  required: true,
  ...extra,
});

// A contract: initials on both pages, a text box and a checkbox on page 1, signatures at the end and
// a date and a name beside the last one.
const fields: SigningField[] = [
  box("sig2", "Signature", 2, 600),
  box("ini2", "Initials", 2, 700, 400),
  box("txt", "Text", 1, 200, 72, { label: "Company name" }),
  box("ini1", "Initials", 1, 700, 400),
  box("chk", "Checkbox", 1, 400, 72, { label: "I accept the terms" }),
  box("date", "Date Signed", 2, 604, 300),
  box("name", "Name", 2, 560),
  box("sig1", "Signature", 1, 620),
  box("opt", "Text", 1, 300, 72, { label: "Nickname", required: false }),
];

describe("what a link answers", () => {
  it("finds the workspace and the token in the page's address", () => {
    expect(locate("/sign/s/acme/AbC_123-xyz")).toEqual({ tenant: "acme", token: "AbC_123-xyz" });
    expect(locate("/sign/s/acme/AbC/")).toEqual({ tenant: "acme", token: "AbC" });
    expect(locate("/sign/s/acme")).toBeNull();
    expect(locate("/sign/new")).toBeNull();
    expect(locate("/sign/s/%E0%A4%A/x")).toBeNull();
  });

  it("opens when the server shows the request", () => {
    expect(classify(200, { request: { title: "NDA" } }, false).kind).toBe("ready");
  });

  it("asks for the access code, and says so when it was wrong", () => {
    expect(classify(401, { error: "unauthorized" }, false)).toEqual({ kind: "code" });
    expect(classify(403, { message: "wrong access code" }, true)).toEqual({ kind: "code", problem: "That code isn't right. Check it and try again." });
  });

  it("says when it is not yet the signer's turn, with or without a code", () => {
    const body = { message: "it is not your turn to sign yet; you will be sent the link again when it is" };
    for (const sent of [false, true]) {
      const r = classify(403, body, sent);
      expect(r.kind).toBe("waiting");
      expect(r.kind === "waiting" && r.message).toMatch(/^It is not your turn/);
    }
  });

  it("keeps a lockout apart from a wrong code", () => {
    const r = classify(429, { message: "too many wrong access codes; try again after 2026-11-01 10:15:00 UTC" }, true);
    expect(r).toEqual({ kind: "locked", message: "Too many wrong access codes; try again after 2026-11-01 10:15:00 UTC" });
  });

  it("treats forged, retired and unknown links alike", () => {
    expect(classify(404, { error: "not_found" }, false)).toEqual({ kind: "invalid" });
  });

  it("tells why a link is gone, signer first", () => {
    const gone = (signer_status: string | null, request_status: string | null) => {
      const r = classify(410, { error: "gone", message: "x", signer_status, request_status }, false);
      return r.kind === "closed" ? r.why : r.kind;
    };
    expect(gone("Signed", "In Progress")).toBe("signed");
    expect(gone("Signed", "Completed")).toBe("signed");
    expect(gone("Declined", "Declined")).toBe("declined");
    expect(gone("Viewed", "Declined")).toBe("request_declined");
    expect(gone("Sent", "Voided")).toBe("voided");
    expect(gone("Sent", "Expired")).toBe("expired");
    expect(gone("Sent", "Completed")).toBe("completed");
    // An expiry the server saw before maintenance marked the request.
    expect(gone("Sent", "Sent")).toBe("expired");
  });

  it("falls back to the server's words for anything else", () => {
    expect(classify(503, { message: "the document cannot be read right now" }, false)).toEqual({ kind: "failed", message: "The document cannot be read right now" });
    expect(classify(500, null, false)).toEqual({ kind: "failed", message: "Something went wrong (500)." });
  });

  it("has words for every way a link can end", () => {
    for (const why of ["signed", "declined", "completed", "request_declined", "voided", "expired"] as const) {
      const c = closedCopy(why, null);
      expect(c.title.length).toBeGreaterThan(5);
      expect(c.text.length).toBeGreaterThan(10);
    }
    expect(closedCopy("signed", "Completed").text).toMatch(/Everyone has now signed/);
    expect(closedCopy("signed", "In Progress").text).toMatch(/still have to sign/);
  });
});

describe("what the signer has to do", () => {
  const tasks = tasksOf(fields);

  it("makes one task of all signatures and one of all initials, in reading order", () => {
    expect(tasks.map((t) => [t.kind, t.label])).toEqual([
      ["text", "Company name"],
      ["text", "Nickname"],
      ["checkbox", "I accept the terms"],
      ["signature", "Your signature"],
      ["initials", "Your initials"],
    ]);
    expect(tasks.find((t) => t.kind === "signature")?.fields.map((f) => f.id)).toEqual(["sig1", "sig2"]);
    expect(tasks.find((t) => t.kind === "initials")?.fields.map((f) => f.id)).toEqual(["ini1", "ini2"]);
  });

  it("leaves the name and the date to the server", () => {
    expect(tasks.flatMap((t) => t.fields).some((f) => f.field_type === "Name" || f.field_type === "Date Signed")).toBe(false);
  });

  it("counts only what is required", () => {
    const e = emptyEntry();
    expect(progress(tasks, e)).toEqual({ done: 0, total: 4 });
    e.values.txt = "Acme";
    e.values.opt = "Bob";
    expect(progress(tasks, e)).toEqual({ done: 1, total: 4 });
    e.values.chk = true;
    e.signature = { text: "Jane Doe" };
    e.initials = { text: "JD" };
    expect(progress(tasks, e)).toEqual({ done: 4, total: 4 });
    expect(complete(tasks, e)).toBe(true);
  });

  it("does not count a blank text or an unticked box as done", () => {
    const e = emptyEntry();
    e.values.txt = "   ";
    e.values.chk = false;
    expect(progress(tasks, e).done).toBe(0);
  });

  it("goes to the next thing to do, skipping optional boxes, and round again", () => {
    const e = emptyEntry();
    expect(nextTask(tasks, e)?.key).toBe("txt");
    e.values.txt = "Acme";
    expect(nextTask(tasks, e, "txt")?.key).toBe("chk");
    e.values.chk = true;
    expect(nextTask(tasks, e, "chk")?.key).toBe("signature");
    e.signature = { text: "Jane" };
    e.initials = { text: "J" };
    expect(nextTask(tasks, e, "initials")).toBeNull();
    // Coming back to an earlier box that is still open.
    const f = emptyEntry();
    f.values.txt = "Acme";
    f.values.chk = true;
    f.signature = { text: "Jane" };
    expect(nextTask(tasks, f, "signature")?.key).toBe("initials");
    f.initials = { text: "J" };
    f.values.chk = false;
    expect(nextTask(tasks, f, "initials")?.key).toBe("chk");
  });

  it("names the next step on the primary button", () => {
    const e = emptyEntry();
    expect(actionLabel(nextTask(tasks, e))).toBe("Fill in “Company name”");
    expect(actionLabel(tasks.find((t) => t.kind === "checkbox") ?? null)).toBe("Tick “I accept the terms”");
    expect(actionLabel(tasks.find((t) => t.kind === "signature") ?? null)).toBe("Add your signature");
    expect(actionLabel(tasks.find((t) => t.kind === "initials") ?? null)).toBe("Add your initials");
    expect(actionLabel(null)).toBe("Review and sign");
  });

  it("shows the pages that have something to do, and says which", () => {
    expect(pagesWithFields(fields)).toEqual([1, 2]);
    expect(pagesWithFields([box("a", "Text", 7, 1), box("b", "Text", 3, 1), box("c", "Text", 3, 9)])).toEqual([3, 7]);
    expect([[], [4], [2, 4], [1, 2, 5]].map(pagesText)).toEqual(["", "page 4", "pages 2 and 4", "pages 1, 2 and 5"]);
  });

  it("gives each box a state", () => {
    const e = emptyEntry();
    e.signature = { image: "data:image/png;base64,AAAA" };
    const state = (id: string) => boxState(fields.find((f) => f.id === id)!, tasks, e, "txt");
    expect(state("sig1")).toBe("done");
    expect(state("sig2")).toBe("done");
    expect(state("txt")).toBe("current");
    expect(state("chk")).toBe("todo");
    expect(state("opt")).toBe("optional");
    expect(state("date")).toBe("auto");
    expect(state("name")).toBe("auto");
  });
});

describe("signing", () => {
  it("sends the marks, the text written and every checkbox", () => {
    const e = emptyEntry();
    e.signature = { image: "data:image/png;base64,AAAA" };
    e.initials = { text: "JD" };
    e.values = { txt: "  Acme  ", chk: true, opt: "" };
    expect(signBody(fields, e)).toEqual({
      consent: true,
      signature: { image: "data:image/png;base64,AAAA" },
      initials: { text: "JD" },
      values: { txt: "Acme", chk: true },
    });
    e.values.chk = false;
    expect(signBody(fields, e).values).toEqual({ txt: "Acme", chk: false });
  });

  it("sends no mark for a box the signer does not have", () => {
    const only = [box("t", "Text", 1, 10)];
    const e = emptyEntry();
    e.signature = { text: "Jane" };
    e.values = { t: "x" };
    expect(signBody(only, e)).toEqual({ consent: true, values: { t: "x" } });
  });

  it("puts the server's complaints where they belong", () => {
    const p = problemsOf(
      [
        { field: "values.txt", message: "Company name: is required" },
        { field: "values.sig2", message: "Your signature: nope" },
        { field: "signature", message: "draw or type your signature" },
        { field: "consent", message: "confirm that you agree to sign electronically" },
        { field: "values.unknown", message: "is not one of your text or checkbox fields" },
      ],
      fields,
    );
    expect(p).toEqual({
      txt: "Company name: is required",
      signature: "Your signature: nope Draw or type your signature",
      consent: "Confirm that you agree to sign electronically",
      "": "Is not one of your text or checkbox fields",
    });
  });

  it("makes initials the way the server does", () => {
    expect(initialsOf("jane van doe")).toBe("JVD");
    expect(initialsOf("  Ana   María Rao Singh Extra ")).toBe("AMRS");
    expect(initialsOf("Ångström Øre")).toBe("ÅØ");
    expect(initialsOf("")).toBe("");
    expect(initialsOf("12 34")).toBe("");
  });
});
