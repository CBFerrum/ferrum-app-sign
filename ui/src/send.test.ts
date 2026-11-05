import { describe, expect, it } from "vitest";
import type { SignRequest } from "./model";
import type { Box } from "./placement";
import { addSigner, bodyOf, boxCounts, changed, draftOf, editSigner, looksLikeEmail, moveSigner, nextRole, peopleOf, refusal, removeSigner, sendProblems, sizeText, templateBody, titleOf, turns } from "./send";

const request = (over: Partial<SignRequest> = {}): SignRequest => ({
  id: "r1",
  version: 3,
  title: "Master Services Agreement",
  status: "Draft",
  signing_mode: "Sequential",
  signers: [
    { id: "s1", role: "Client", name: "Jane Doe", email: "jane@client.test", signing_order: 1 },
    { id: "s2", role: "Company", name: "Ravi Kumar", email: "ravi@acme.test", signing_order: 2, access_code_hash: "$argon2id$x" },
  ],
  fields: [
    { id: "f1", role: "Client", field_type: "Signature", page: 1, x: "72.00", y: "600.00", width: "160.00", height: "44.00", required: true },
    { id: "f2", role: "Company", field_type: "Signature", page: 2, x: "72", y: "600", width: "160", height: "44", required: true },
  ],
  ...over,
});

describe("preparing a request", () => {
  it("reads a saved draft, remembering who has an access code but not the code", () => {
    const d = draftOf(request());
    expect(d.signers.map((s) => [s.role, s.hasCode, s.accessCode])).toEqual([
      ["Client", false, ""],
      ["Company", true, ""],
    ]);
    expect(d.boxes).toHaveLength(2);
    expect(draftOf(request({ expires_on: "2026-12-01", message: "Please sign." })).expires_on).toBe("2026-12-01");
  });

  it("saves the whole draft, with a code only when one was typed", () => {
    const d = editSigner(draftOf(request()), "s1", { accessCode: "7731" });
    const body = bodyOf(d);
    expect(body.signers[0]).toEqual({ id: "s1", role: "Client", name: "Jane Doe", email: "jane@client.test", signing_order: 1, access_code: "7731" });
    expect(body.signers[1]).not.toHaveProperty("access_code");
    expect(body.fields[0]).toMatchObject({ id: "f1", x: "72", y: "600" });
    expect(body).toMatchObject({ title: "Master Services Agreement", message: null, expires_on: null, signing_mode: "Sequential" });
  });

  it("knows whether anything changed", () => {
    const a = draftOf(request());
    expect(changed(a, draftOf(request()))).toBe(false);
    expect(changed(a, { ...a, title: " Master Services Agreement " })).toBe(false);
    expect(changed(a, { ...a, title: "Other" })).toBe(true);
    expect(changed(a, editSigner(a, "s1", { email: "j@client.test" }))).toBe(true);
    expect(changed(a, editSigner(a, "s1", { accessCode: "1234" }))).toBe(true);
    expect(changed(a, { ...a, boxes: a.boxes.slice(1) })).toBe(true);
  });

  it("names new signers so that no two share a role", () => {
    expect(nextRole([])).toBe("Signer 1");
    const d = draftOf(request());
    expect(nextRole(d.signers)).toBe("Signer 3");
    expect(nextRole([...d.signers, { ...d.signers[0], role: "signer 3" }])).toBe("Signer 4");
  });

  it("adds a signer to sign after the others, or with them in parallel", () => {
    const d = addSigner(draftOf(request()));
    expect(d.signers[2]).toMatchObject({ role: "Signer 3", signing_order: 3 });
    expect(addSigner({ ...draftOf(request()), signing_mode: "Parallel" }).signers[2].signing_order).toBe(1);
  });

  it("takes a signer's boxes along when they leave, or are renamed", () => {
    const d = draftOf(request());
    const gone = removeSigner(d, "s2");
    expect(gone.signers.map((s) => s.role)).toEqual(["Client"]);
    expect(gone.boxes.map((b) => b.role)).toEqual(["Client"]);
    const renamed = editSigner(d, "s1", { role: "Buyer" });
    expect(renamed.boxes.map((b) => b.role)).toEqual(["Buyer", "Company"]);
    expect(editSigner(d, "s1", { name: "J" }).boxes).toBe(d.boxes);
    expect(removeSigner(d, "nobody")).toBe(d);
  });

  it("moves a signer and swaps their turns, so the order still reads", () => {
    const d = draftOf(request());
    const up = moveSigner(d, "s2", -1);
    expect(up.signers.map((s) => [s.role, s.signing_order])).toEqual([
      ["Company", 1],
      ["Client", 2],
    ]);
    expect(moveSigner(d, "s1", -1)).toBe(d);
    expect(moveSigner(d, "s2", 1)).toBe(d);
  });

  it("groups signers by turn, those with the same number signing together", () => {
    const d = draftOf(request());
    const together = editSigner(d, "s2", { signing_order: 1 });
    expect(turns(d.signers).map((t) => t.map((s) => s.role))).toEqual([["Client"], ["Company"]]);
    expect(turns(together.signers).map((t) => t.map((s) => s.role))).toEqual([["Client", "Company"]]);
  });

  it("checks addresses roughly: the server has the last word", () => {
    expect(["a@b.co", " a@b.co ", "a.b+c@d-e.org"].every(looksLikeEmail)).toBe(true);
    expect(["a@b", "a b@c.d", "@b.c", "ab.c", ""].some(looksLikeEmail)).toBe(false);
  });
});

describe("what stops a request going out", () => {
  const today = "2026-11-01";
  it("lets a finished draft go", () => {
    expect(sendProblems(draftOf(request()), today)).toEqual([]);
  });

  it("says what is missing about each person", () => {
    const d = draftOf(request());
    const bad = editSigner(editSigner(d, "s1", { name: "", email: "jane@" }), "s2", { email: "" });
    const texts = sendProblems(bad, today).map((p) => p.text);
    expect(texts).toEqual(["Client needs a name.", "“jane@” doesn't look like an email address.", "Ravi Kumar needs an email address."]);
  });

  it("asks for a signature box for everyone, by name", () => {
    const d = draftOf(request());
    const lacking = { ...d, boxes: d.boxes.filter((b: Box) => b.role === "Client") };
    expect(sendProblems(lacking, today)).toEqual([{ step: "fields", signer: "s2", text: "Ravi Kumar has no signature box. Place a required Signature box for them." }]);
    const optional = { ...d, boxes: d.boxes.map((b: Box) => (b.role === "Client" ? { ...b, required: false } : b)) };
    expect(sendProblems(optional, today)[0].signer).toBe("s1");
  });

  it("needs a title, someone to sign, and a deadline that has not passed", () => {
    expect(sendProblems({ ...draftOf(request({ signers: [], fields: [] })), title: " " }, today).map((p) => p.text)).toEqual(["Give the request a title.", "Add at least one person to sign."]);
    expect(sendProblems({ ...draftOf(request()), expires_on: "2026-10-31" }, today)).toEqual([{ step: "details", text: "The deadline is in the past." }]);
    expect(sendProblems({ ...draftOf(request()), expires_on: "2026-11-01" }, today)).toEqual([]);
  });

  it("does not let two people share a role", () => {
    const d = editSigner(draftOf(request()), "s2", { role: "client" });
    expect(sendProblems(d, today).some((p) => p.text.includes("roles must differ"))).toBe(true);
    expect(sendProblems(editSigner(draftOf(request()), "s2", { role: " " }), today).some((p) => p.text.endsWith("needs a role."))).toBe(true);
  });

  it("counts a signer's boxes in words", () => {
    const d = draftOf(request());
    expect(boxCounts(d.boxes, "Client")).toBe("1 signature");
    expect(boxCounts(d.boxes, "Nobody")).toBe("no boxes");
    const many: Box[] = [
      ...d.boxes,
      { ...d.boxes[0], key: "a", field_type: "Initials" },
      { ...d.boxes[0], key: "b", field_type: "Initials" },
      { ...d.boxes[0], key: "c", field_type: "Date Signed" },
      { ...d.boxes[0], key: "d", field_type: "Checkbox" },
      { ...d.boxes[0], key: "e", field_type: "Checkbox" },
    ];
    expect(boxCounts(many, "Client")).toBe("1 signature, 2 initials, 1 automatic, 2 checkboxes");
  });
});

describe("starting from a template", () => {
  const roles = [
    { role: "Client", signing_order: 1 },
    { role: "Company", signing_order: 2, default_name: "Ravi Kumar", default_email: "ravi@acme.test" },
  ];

  it("offers each role as a person, with the defaults the template holds", () => {
    expect(peopleOf({ roles })).toEqual([
      { role: "Client", name: "", email: "", code: "" },
      { role: "Company", name: "Ravi Kumar", email: "ravi@acme.test", code: "" },
    ]);
    expect(peopleOf({})).toEqual([]);
  });

  it("sends only what was filled in", () => {
    const who = peopleOf({ roles }).map((p, i) => (i === 0 ? { ...p, name: " Jane Doe ", email: "jane@client.test ", code: "4821" } : p));
    expect(templateBody(" ", "", "", who)).toEqual({
      signers: [
        { role: "Client", name: "Jane Doe", email: "jane@client.test", access_code: "4821" },
        { role: "Company", name: "Ravi Kumar", email: "ravi@acme.test" },
      ],
    });
    expect(templateBody("NDA", "Please sign", "2026-12-01", [])).toEqual({ title: "NDA", message: "Please sign", expires_on: "2026-12-01", signers: [] });
  });
});

describe("the file to be signed", () => {
  it("makes a title from the file's name", () => {
    expect(["Master_Agreement.pdf", "NDA  v2.PDF", "contract", "  .pdf"].map(titleOf)).toEqual(["Master Agreement", "NDA v2", "contract", ""]);
  });

  it("refuses what the server would", () => {
    const pdf = { name: "a.pdf", type: "application/pdf", size: 1000 };
    expect(refusal(pdf)).toBeNull();
    expect(refusal({ ...pdf, name: "scan", type: "application/pdf" })).toBeNull();
    expect(refusal({ ...pdf, name: "a.pdf", type: "" })).toBeNull();
    expect(refusal({ ...pdf, name: "a.docx", type: "application/msword" })).toBe("Only PDF files can be signed here.");
    expect(refusal({ ...pdf, size: 26 * 1024 * 1024 })).toBe("That file is larger than 25 MB.");
    expect(refusal({ ...pdf, size: 25 * 1024 * 1024 })).toBeNull();
    expect(refusal({ ...pdf, size: 0 })).toBe("That file is empty.");
  });

  it("says a file's size so a small one is not 0.00 MB", () => {
    expect([sizeText(1), sizeText(776), sizeText(1023)]).toEqual(["1 byte", "776 bytes", "1023 bytes"]);
    expect([sizeText(1024), sizeText(48_300), sizeText(999 * 1024)]).toEqual(["1 KB", "47 KB", "999 KB"]);
    expect([sizeText(1000 * 1024), sizeText(2.4 * 1024 * 1024), sizeText(25 * 1024 * 1024)]).toEqual(["1.0 MB", "2.4 MB", "25.0 MB"]);
  });
});
