import { describe, expect, it } from "vitest";
import { columnsOf, parseRecipients } from "./bulk";

describe("a bulk send's recipients", () => {
  it("reads a name and an address for each role, a line each", () => {
    const r = parseRecipients("Asha Rao, asha@x.test, Dev, dev@x.test\n\nBo\tbo@x.test\tCy\tcy@x.test", ["Client", "Witness"]);
    expect(r.problems).toEqual([]);
    expect(r.rows.map((x) => JSON.parse(x.signers))).toEqual([
      { Client: { name: "Asha Rao", email: "asha@x.test" }, Witness: { name: "Dev", email: "dev@x.test" } },
      { Client: { name: "Bo", email: "bo@x.test" }, Witness: { name: "Cy", email: "cy@x.test" } },
    ]);
  });

  it("skips a header line and names the line that is wrong", () => {
    const r = parseRecipients("Client name, Client email\nAsha, asha@x.test\nBob, not-an-address", ["Client"]);
    expect(r.rows).toHaveLength(1);
    expect(r.problems).toEqual(["Line 3: a name and a valid address are needed for Client."]);
  });

  it("wants someone, and says how the columns run", () => {
    expect(parseRecipients("  \n", ["Client"]).problems).toEqual(["Add at least one recipient."]);
    expect(columnsOf(["Client"])).toBe("Client name, Client email");
  });
});
