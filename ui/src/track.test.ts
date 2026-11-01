import { describe, expect, it } from "vitest";
import type { SignEvent, Signer, SignRequest } from "./model";
import { checks, isOpen, reminders, short, signerLine, standing, stepState, tabOf, told, toneOf, turnOf, type Verify } from "./track";

const signer = (over: Partial<Signer>): Signer => ({ id: over.role, role: "A", signing_order: 1, status: "Waiting", ...over });
const when = (iso: string | null | undefined) => `on ${iso}`;

describe("the lists", () => {
  it("puts a request on the list its status belongs to", () => {
    expect(["Draft", "Sent", "In Progress", "Completed", "Declined", "Expired", "Voided"].map(tabOf)).toEqual(["drafts", "progress", "progress", "completed", "closed", "closed", "closed"]);
    expect([isOpen("Sent"), isOpen("In Progress"), isOpen("Draft"), isOpen("Completed")]).toEqual([true, true, false, false]);
  });

  it("dresses a status the way it reads", () => {
    expect(["Draft", "Sent", "Completed", "Declined", "Expired", "Voided", "Other"].map(toneOf)).toEqual(["neutral", "blue", "green", "red", "amber", "neutral", "neutral"]);
  });
});

describe("whose turn it is", () => {
  const ours = (mode: SignRequest["signing_mode"], signers: Signer[]) => ({ status: "In Progress" as const, signing_mode: mode, signers });
  const three = [
    signer({ role: "A", signing_order: 1, status: "Signed" }),
    signer({ role: "B", signing_order: 2, status: "Viewed" }),
    signer({ role: "C", signing_order: 2, status: "Sent" }),
    signer({ role: "D", signing_order: 3, status: "Waiting" }),
  ];

  it("is those with the lowest turn still to sign, together when the numbers match", () => {
    expect(turnOf(ours("Sequential", three)).map((s) => s.role)).toEqual(["B", "C"]);
  });

  it("is everyone still to sign when they sign in parallel", () => {
    expect(turnOf(ours("Parallel", three)).map((s) => s.role)).toEqual(["B", "C", "D"]);
  });

  it("is nobody when all have signed", () => {
    expect(turnOf(ours("Sequential", [signer({ status: "Signed" })]))).toEqual([]);
  });

  it("marks each step done, now or later", () => {
    const turn = turnOf(ours("Sequential", three));
    expect(three.map((s) => stepState(s, turn))).toEqual(["done", "now", "now", "later"]);
    expect(stepState(signer({ role: "E", status: "Declined" }), turn)).toBe("declined");
  });
});

describe("where a request stands", () => {
  const base = { signing_mode: "Sequential" as const };
  it("says who it is waiting for", () => {
    const signers = [signer({ role: "A", name: "Jane Doe", signing_order: 1, status: "Signed" }), signer({ role: "B", name: "Ravi Kumar", signing_order: 2, status: "Viewed" })];
    expect(standing({ ...base, status: "In Progress", signers })).toMatchObject({ signed: 1, total: 2, text: "Waiting for Ravi Kumar" });
    const both = [signer({ role: "A", name: "Jane", status: "Sent" }), signer({ role: "B", name: "Ravi", status: "Sent" }), signer({ role: "C", name: "Bo", status: "Sent" })];
    expect(standing({ signing_mode: "Parallel", status: "Sent", signers: both }).text).toBe("Waiting for Jane and 2 others");
    expect(standing({ signing_mode: "Parallel", status: "Sent", signers: both.slice(0, 2) }).text).toBe("Waiting for Jane and 1 other");
  });

  it("names the email when there is no name", () => {
    const s = [signer({ role: "A", email: "a@x.test", status: "Sent" })];
    expect(standing({ ...base, status: "Sent", signers: s }).text).toBe("Waiting for a@x.test");
  });

  it("says how it ended", () => {
    const signers = [signer({ role: "A", name: "Jane", status: "Signed" }), signer({ role: "B", name: "Ravi", status: "Declined" })];
    expect(standing({ ...base, status: "Draft", signers: [] }).text).toBe("Not sent yet");
    expect(standing({ ...base, status: "Completed", signers: [signer({ status: "Signed" })] }).text).toBe("Signed");
    expect(standing({ ...base, status: "Completed", signers: [signer({ role: "A", status: "Signed" }), signer({ role: "B", status: "Signed" })] }).text).toBe("Signed by all 2");
    expect(standing({ ...base, status: "Declined", signers }).text).toBe("Ravi declined");
    expect(standing({ ...base, status: "Expired", signers }).text).toBe("Expired with 1 of 2 signed");
    expect(standing({ ...base, status: "Voided", signers }).text).toBe("Withdrawn by the sender");
  });

  it("falls back to a count when nobody has their link yet", () => {
    expect(standing({ ...base, status: "Sent", signers: [signer({ status: "Waiting" })] }).text).toBe("0 of 1 signed");
  });

  it("describes each signer and their reminders", () => {
    expect(signerLine(signer({ status: "Signed", signed_at: "T1" }), when)).toBe("Signed on T1");
    expect(signerLine(signer({ status: "Declined", declined_at: "T2" }), when)).toBe("Declined on T2");
    expect(signerLine(signer({ status: "Viewed", viewed_at: "T3" }), when)).toBe("Opened on T3, not signed yet");
    expect(signerLine(signer({ status: "Sent", sent_at: "T4" }), when)).toBe("Link sent on T4, not opened yet");
    expect(signerLine(signer({ status: "Waiting" }), when)).toBe("Waiting for their turn");
    expect(reminders(signer({}), (i) => i)).toBe("");
    expect(reminders(signer({ reminders_sent: 1 }), (i) => i)).toBe("Reminded once");
    expect(reminders(signer({ reminders_sent: 3, last_reminded_at: "yesterday" }), (i) => i)).toBe("Reminded 3 times, last yesterday");
  });
});

describe("the trail in words", () => {
  const event = (over: Partial<SignEvent>): SignEvent => ({ id: "e", seq: 1, event: "Sent", occurred_at: "T", ...over });
  it("tells each kind of event about the person it concerns", () => {
    expect(told(event({ event: "Sent", signer_name: "Jane", details: "Emailed the signing link" }))).toEqual({ title: "Sent to Jane", detail: "Emailed the signing link", tone: "plain" });
    expect(told(event({ event: "Viewed", signer_email: "j@x.test" })).title).toBe("j@x.test opened the document");
    expect(told(event({ event: "Signed", role: "Client", details: "signature drawn" }))).toEqual({ title: "Client signed", detail: "Signature drawn", tone: "good" });
    expect(told(event({ event: "Declined", signer_name: "Ravi", details: "Wrong amount" }))).toEqual({ title: "Ravi declined", detail: "“Wrong amount”", tone: "bad" });
    expect(told(event({ event: "Reminded", signer_name: "Ravi" })).title).toBe("Reminder for Ravi");
    expect(told(event({ event: "Resent", signer_name: "Ravi" })).title).toBe("Link sent again to Ravi");
    expect(told(event({ event: "Corrected", signer_name: "Ravi", details: "email: a → b" })).detail).toBe("email: a → b");
    expect(told(event({ event: "Access Denied", signer_name: "Ravi" })).tone).toBe("bad");
    expect(told(event({ event: "Voided", details: "Wrong version" }))).toEqual({ title: "Withdrawn by the sender", detail: "“Wrong version”", tone: "bad" });
    expect(told(event({ event: "Expired" })).tone).toBe("bad");
    expect(told(event({ event: "Completed" })).tone).toBe("good");
    expect(told(event({ event: "Something new" })).title).toBe("Something new");
    expect(told(event({ event: "Viewed" })).title).toBe("the signer opened the document");
  });
});

describe("what a check found", () => {
  const ok: Verify = { valid: true, original: { ok: true }, sealed: { ok: true, readable: true }, audit_trail: { ok: true, events: 6 } };
  it("reads a sound request line by line", () => {
    const lines = checks(ok);
    expect(lines.map((l) => [l.label, l.ok])).toEqual([
      ["The original document", true],
      ["The signed copy", true],
      ["The audit trail", true],
    ]);
    expect(lines[2].detail).toBe("6 events, each tied to the one before it.");
  });

  it("leaves out the signed copy until there is one", () => {
    expect(checks({ ...ok, sealed: null }).map((l) => l.label)).toEqual(["The original document", "The audit trail"]);
  });

  it("says where it went wrong", () => {
    const bad: Verify = {
      valid: false,
      original: { ok: false },
      sealed: { ok: false, readable: false },
      audit_trail: { ok: false, events: 4, problem: "event 3 was changed" },
    };
    const lines = checks(bad);
    expect(lines.every((l) => !l.ok)).toBe(true);
    expect(lines[1].detail).toBe("It can't be read as a PDF.");
    expect(lines[2].detail).toBe("event 3 was changed");
    expect(checks({ ...bad, original: { ok: false, error: "the stored file is missing" } })[0].detail).toBe("the stored file is missing");
  });

  it("shortens a fingerprint to read", () => {
    expect(short("a".repeat(24) + "b".repeat(40))).toBe("aaaaaaaa…bbbbbb");
    expect(short(null)).toBe("");
  });
});
