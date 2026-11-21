// Bulk send, apart from drawing it: the recipients' list, a line each, read into the rows the server takes. The server checks
// everything again when sending starts (the roles, the addresses, what the caller may use); this tells the sender sooner.

import { t as tr } from "@ferrum/client";
import { looksLikeEmail } from "./send";

export interface RecipientRow {
  signers: string;
  reference_entity?: string;
  reference_id?: string;
}

/** One line per recipient, for each role in turn a name and an address, separated by commas or tabs (a spreadsheet's rows
 *  paste as they are). Blank lines are skipped, and so is a first line that names the columns. */
export function parseRecipients(text: string, roles: string[]): { rows: RecipientRow[]; problems: string[] } {
  const rows: RecipientRow[] = [];
  const problems: string[] = [];
  const lines = text.split(/\r?\n/).filter((l) => l.trim());
  const header = lines[0]?.split(/[,\t]/).some((c) => /@/.test(c)) === false && lines.length > 1;
  lines.slice(header ? 1 : 0).forEach((line, i) => {
    const cells = line.split(/[,\t]/).map((c) => c.trim());
    const who: Record<string, { name: string; email: string }> = {};
    roles.forEach((role, r) => {
      who[role] = { name: cells[r * 2] ?? "", email: cells[r * 2 + 1] ?? "" };
    });
    const bad = roles.find((role) => !who[role].name || !looksLikeEmail(who[role].email));
    if (bad) problems.push(tr("Line {n}: a name and a valid address are needed for {role}.", { n: i + 1 + (header ? 1 : 0), role: bad }));
    else rows.push({ signers: JSON.stringify(who) });
  });
  if (!rows.length && !problems.length) problems.push(tr("Add at least one recipient."));
  if (rows.length > 1000) problems.push(tr("At most 1000 recipients in one bulk send."));
  return { rows, problems };
}

/** The columns a list is read in, for the sender: `Client name, Client email, …`. */
export const columnsOf = (roles: string[]) => roles.flatMap((r) => [tr("{role} name", { role: r }), tr("{role} email", { role: r })]).join(", ");
