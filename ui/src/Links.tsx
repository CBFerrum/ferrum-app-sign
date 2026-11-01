// Signing links handed back by an action, for the sender to pass on. With Mail they are emailed and
// there is nothing to copy; without it the server returns each link once, here.

import { useState } from "react";
import { Check, Copy } from "lucide-react";
import { Badge, Button, Dialog, toast } from "@ferrum/kit";
import { copyText } from "./clipboard";
import { absolute, type LinkRow } from "./model";

function Row({ link }: { link: LinkRow }) {
  const [done, setDone] = useState(false);
  const url = link.url ? absolute(link.url) : "";
  return (
    <li className="sg-link">
      <div className="sg-link-who">
        <strong>{link.name || link.role}</strong>
        <span className="faint">{link.email}</span>
      </div>
      {link.emailed ? (
        <Badge tone="green">Emailed</Badge>
      ) : (
        <div className="sg-link-url">
          <input className="input mono" readOnly value={url} aria-label={`Signing link for ${link.name || link.role}`} onFocus={(e) => e.currentTarget.select()} />
          <Button
            onClick={async () => {
              if (await copyText(url)) {
                setDone(true);
                toast("Link copied");
                setTimeout(() => setDone(false), 2500);
              } else toast("Select the link and copy it by hand", "error");
            }}
          >
            {done ? <Check size={15} aria-hidden="true" /> : <Copy size={15} aria-hidden="true" />}
            {done ? "Copied" : "Copy"}
          </Button>
        </div>
      )}
    </li>
  );
}

/** What was sent where: emailed links need nothing more; the others are shown to pass on. */
export function LinksDialog({ open, title, links, onClose }: { open: boolean; title: string; links: LinkRow[]; onClose: () => void }) {
  const manual = links.some((l) => !l.emailed);
  return (
    <Dialog open={open} onClose={onClose} title={title} wide footer={<Button variant="primary" onClick={onClose}>Done</Button>}>
      <div className="stack" style={{ gap: 12 }}>
        <p className="muted">
          {manual
            ? "This workspace has no email set up for Sign, so nothing was emailed. Send each person their own link: it works for them only, once."
            : "Each person was emailed their link. It works for them only, once."}
        </p>
        <ul className="sg-links">
          {links.map((l) => (
            <Row key={`${l.role}-${l.email}`} link={l} />
          ))}
        </ul>
      </div>
    </Dialog>
  );
}
