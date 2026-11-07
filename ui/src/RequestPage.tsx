// One request, followed: where it stands and who is holding it up, a reminder in a click, the signed
// copy once everyone has signed, everything that happened (the audit trail), and a check that the
// files and the trail are as they were. It stays current as signers act.

import { useQuery, useQueryClient } from "@tanstack/react-query";
import { type FormEvent, useCallback, useEffect, useState } from "react";
import { Link, Navigate, useLocation, useNavigate, useParams } from "react-router";
import { Ban, Bell, Check, Clock, Copy, Download, Ellipsis, Eye, Hash, Info, Mail, PenLine, X } from "lucide-react";
import { api, ApiError, docPath, download, errorMessage, relativeTime, shell, t as tr, isolate } from "@ferrum/client";
import { Alert, Badge, Button, Dialog, ErrorState, Field, PageLoading, Spinner, toast, useLiveEntity, useShellTitle } from "@ferrum/kit";
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuSeparator, DropdownMenuTrigger } from "@ferrum/kit/primitives";
import { copyText } from "./clipboard";
import { act, eventsQuery, refresh, requestQuery } from "./data";
import { LinksDialog } from "./Links";
import type { LinkRow, SignEvent, Signer, SignRequest } from "./model";
import { checks, isOpen, reminders, short, signerLine, standing, stepState, toneOf, told, turnOf, type Verify } from "./track";
import { moment, timeLeft } from "./time";

export function RequestRoute() {
  const { id = "" } = useParams();
  const qc = useQueryClient();
  const q = useQuery(requestQuery(id));
  useLiveEntity("SignRequest", useCallback((e) => void (e.id === id && refresh(qc, id)), [qc, id]));
  useLiveEntity("SignEvent", useCallback(() => void qc.invalidateQueries({ queryKey: ["sign", "events", id] }), [qc, id]));
  useShellTitle(q.data?.title);
  if (q.error) return <ErrorState error={q.error} onRetry={() => void q.refetch()} />;
  if (!q.data) return <PageLoading />;
  if (q.data.status === "Draft") return <Navigate to={`/request/${id}/prepare`} replace />;
  return <RequestView key={id} request={q.data} />;
}

const when = (iso: string | null | undefined) => (iso ? moment(iso) : "");

async function copy(text: string, what: string) {
  if (await copyText(text)) toast(tr("{what} copied", { what }));
  else toast(tr("Select it and copy by hand"), "error");
}

export function RequestView({ request: r }: { request: SignRequest }) {
  const qc = useQueryClient();
  const navigate = useNavigate();
  const here = useLocation();
  const sent = (here.state as { sent?: LinkRow[] } | null)?.sent;
  const [links, setLinks] = useState<{ title: string; list: LinkRow[] } | null>(sent?.length ? { title: tr("Request sent"), list: sent } : null);
  const [busy, setBusy] = useState<string | null>(null);
  const [voiding, setVoiding] = useState(false);
  const [correcting, setCorrecting] = useState<Signer | null>(null);
  const [about, setAbout] = useState(false);
  const events = useQuery(eventsQuery(r.id));
  const [verify, setVerify] = useState<{ at: number; result: Verify } | null>(null);
  const open = isOpen(r.status);
  const s = standing(r);
  const left = open ? timeLeft(r.expires_at) : null;
  const everyoneSigned = r.signers.length > 0 && r.signers.every((x) => x.status === "Signed");
  const declined = r.signers.find((x) => x.status === "Declined");

  // After a send the links are read once: the state is cleared so a reload does not show them again.
  useEffect(() => {
    if (sent) navigate(".", { replace: true, state: null });
  }, [sent, navigate]);

  const run = async (name: string, f: () => Promise<void>) => {
    setBusy(name);
    try {
      await f();
    } catch (e) {
      toast(errorMessage(e), "error");
    } finally {
      setBusy(null);
    }
  };
  const handBack = (title: string, list: LinkRow[]) => {
    refresh(qc, r.id);
    if (list.some((l) => !l.emailed)) setLinks({ title, list });
    else toast(`${title}: ${list.map((l) => l.name || l.email).join(", ")}`);
  };
  const remind = () => run("remind", async () => handBack("Reminder sent", (await act<{ reminded: LinkRow[] }>(r.id, "remind")).reminded));
  const again = (signer?: Signer) => run("resend", async () => handBack("Link sent again", (await act<{ resent: LinkRow[] }>(r.id, "resend", signer ? { signer: signer.id } : {})).resent));
  const check = () => run("verify", async () => setVerify({ at: Date.now(), result: await act<Verify>(r.id, "verify") }));
  const get = (path: string | null | undefined, name: string) => run("download", () => download(path ?? "", name));

  const header = (
    <header className="sg-req-head">
      <div className="sg-req-title">
        <h1 className="page-title">{isolate(r.title)}</h1>
        <p className="page-sub">
          {isolate([r.number, r.document_name, r.page_count ? `${r.page_count} ${r.page_count === 1 ? "page" : "pages"}` : null].filter(Boolean).join(" · "))}
        </p>
      </div>
      <div className="page-actions">
        <Badge tone={toneOf(r.status)}>{isolate(r.status)}</Badge>
        {left && <Badge tone={left.level === "late" ? "red" : left.level === "soon" ? "amber" : "neutral"}>{isolate(left.text)}</Badge>}
        {open && (
          <Button variant="primary" loading={busy === "remind"} onClick={() => void remind()}>
            <Bell size={15} aria-hidden="true" /> {tr("Send a reminder")}
          </Button>
        )}
        {r.status === "Completed" && r.sealed_document && (
          <Button variant="primary" loading={busy === "download"} onClick={() => void get(r.sealed_document, `${r.title} - signed.pdf`)}>
            <Download size={15} aria-hidden="true" /> {tr("Download the signed copy")}
          </Button>
        )}
        <DropdownMenu>
          <DropdownMenuTrigger asChild>
            <button type="button" className="icon-btn" aria-label={tr("More actions")}>
              <Ellipsis size={17} aria-hidden="true" />
            </button>
          </DropdownMenuTrigger>
          <DropdownMenuContent align="end">
            {open && <DropdownMenuItem onSelect={() => void again()}>{tr("Send the links again")}</DropdownMenuItem>}
            <DropdownMenuItem onSelect={() => void check()}>{tr("Check integrity")}</DropdownMenuItem>
            <DropdownMenuItem onSelect={() => void get(r.document, r.document_name ?? "document.pdf")}>{tr("Download the original")}</DropdownMenuItem>
            {r.sealed_document && r.status !== "Completed" && <DropdownMenuItem onSelect={() => void get(r.sealed_document, `${r.title} - signed.pdf`)}>{tr("Download the signed copy")}</DropdownMenuItem>}
            {open && (
              <>
                <DropdownMenuSeparator />
                <DropdownMenuItem onSelect={() => setVoiding(true)}>{tr("Withdraw this request…")}</DropdownMenuItem>
              </>
            )}
          </DropdownMenuContent>
        </DropdownMenu>
      </div>
    </header>
  );

  return (
    <div className="sg-app sg-request">
      <div className="crumbs">
        <Link to="/">{tr("Signatures")}</Link>
      </div>
      {header}
      {r.status === "Completed" && (
        <Alert tone="success">
          {tr("Everyone signed")}{r.completed_at ? tr(" on {value}", { value: when(r.completed_at) }) : ""}{tr(". The signed copy carries an audit page listing each step and the fingerprints below.")}
        </Alert>
      )}
      {open && everyoneSigned && (
        <Alert tone="warning">
          {tr("Everyone has signed. The sealed copy is being prepared")}{r.seal_error ? tr(", and the last try failed: {seal_error}", { seal_error: r.seal_error }) : ""}{tr(". This is retried every minute; there is nothing to do.")}
        </Alert>
      )}
      {r.status === "Declined" && declined && (
        <Alert tone="error">
          <strong>{declined.name || declined.role} {tr("declined.")}</strong> {declined.decline_reason ? `“${declined.decline_reason}”` : ""} {tr("The request is closed for everyone.")}
        </Alert>
      )}
      {r.status === "Voided" && <Alert tone="info">{tr("You withdrew this request")}{r.void_reason ? `: “${r.void_reason}”` : ""}{tr(". Its links no longer work.")}</Alert>}
      {r.status === "Expired" && <Alert tone="warning">{tr("The last day to sign passed on")} {isolate(when(r.expires_at))}{tr(". The links no longer work.")}</Alert>}

      <div className="sg-req-body">
        <div className="sg-req-main">
          <section aria-labelledby="signers-head">
            <h2 id="signers-head" className="sg-h2">
              {tr("Signers")} <span className="faint">{isolate(s.text)}</span>
            </h2>
            <Signers request={r} onAgain={(x) => void again(x)} onCorrect={setCorrecting} />
          </section>
          <section aria-labelledby="trail-head">
            <h2 id="trail-head" className="sg-h2">
              {tr("What happened")}
            </h2>
            <Trail events={events.data} error={events.error} onRetry={() => void events.refetch()} />
          </section>
        </div>
        <aside className="sg-req-side" aria-label={tr("Details")}>
          <section className="sg-side-card" aria-labelledby="doc-head">
            <h3 id="doc-head">{tr("Document")}</h3>
            <p className="sg-wrap">{isolate(r.document_name ?? "")}</p>
            <div className="sg-fp">
              <span className="faint">{tr("Fingerprint (SHA-256)")}</span>
              <span className="mono" title={r.document_sha256 ?? ""}>
                {isolate(short(r.document_sha256))}
              </span>
              {r.document_sha256 && (
                <button type="button" className="icon-btn icon-btn-sm" aria-label={tr("Copy the original's fingerprint")} onClick={() => void copy(r.document_sha256 ?? "", "Fingerprint")}>
                  <Copy size={13} aria-hidden="true" />
                </button>
              )}
            </div>
            {r.sealed_sha256 && (
              <div className="sg-fp">
                <span className="faint">{tr("Signed copy")}</span>
                <span className="mono" title={r.sealed_sha256}>
                  {isolate(short(r.sealed_sha256))}
                </span>
                <button type="button" className="icon-btn icon-btn-sm" aria-label={tr("Copy the signed copy's fingerprint")} onClick={() => void copy(r.sealed_sha256 ?? "", "Fingerprint")}>
                  <Copy size={13} aria-hidden="true" />
                </button>
              </div>
            )}
          </section>

          <section className="sg-side-card" aria-labelledby="int-head">
            <h3 id="int-head">{tr("Integrity")}</h3>
            <p className="faint" style={{ fontSize: 12.5 }}>
              {tr("Checks that the stored files and the trail are as they were when sent and signed.")}
            </p>
            <Button loading={busy === "verify"} onClick={() => void check()}>
              <Hash size={15} aria-hidden="true" /> {verify ? tr("Check again") : tr("Check integrity")}
            </Button>
            {verify && <Integrity result={verify.result} at={verify.at} />}
          </section>

          <section className="sg-side-card" aria-labelledby="det-head">
            <h3 id="det-head">{tr("Details")}</h3>
            <dl className="sg-facts">
              <dt>{tr("Sent by")}</dt>
              <dd>{r.sender_email || "—"}</dd>
              <dt>{tr("Order")}</dt>
              <dd>{r.signing_mode === "Parallel" ? tr("All at once") : tr("One after another")}</dd>
              {r.sent_at && (
                <>
                  <dt>{tr("Sent")}</dt>
                  <dd>{isolate(when(r.sent_at))}</dd>
                </>
              )}
              {r.expires_at && (
                <>
                  <dt>{open ? tr("Last day") : tr("Was due")}</dt>
                  <dd>{isolate(when(r.expires_at))}</dd>
                </>
              )}
              {r.reference_entity && r.reference_id && (
                <>
                  <dt>{tr("About")}</dt>
                  <dd>
                    <button type="button" className="link-btn" onClick={() => shell().navigate({ entity: r.reference_entity ?? "", id: r.reference_id ?? "" })}>
                      {isolate(r.reference_entity)}
                    </button>
                  </dd>
                </>
              )}
              {r.template && (
                <>
                  <dt>{tr("Template")}</dt>
                  <dd>{r._titles?.template ?? tr("Template")}</dd>
                </>
              )}
            </dl>
            {r.message && <p className="sg-message sg-pre">{isolate(r.message)}</p>}
          </section>

          <p className="sg-kind">
            <Info size={14} aria-hidden="true" />
            <span>
              {tr("An electronic signature with an audit trail, not a certificate-based one.")}{isolate(" ")}
              <button type="button" className="link-btn" onClick={() => setAbout(true)}>
                {tr("What this means")}
              </button>
            </span>
          </p>
        </aside>
      </div>

      <LinksDialog open={links !== null} title={links?.title ?? ""} links={links?.list ?? []} onClose={() => setLinks(null)} />
      {voiding && <VoidDialog request={r} onClose={() => setVoiding(false)} />}
      {correcting && <CorrectDialog request={r} signer={correcting} onClose={() => setCorrecting(null)} />}
      <Dialog open={about} onClose={() => setAbout(false)} title={tr("What kind of signature this is")} footer={<Button variant="primary" onClick={() => setAbout(false)}>{tr("Got it")}</Button>}>
        <div className="stack sg-about" style={{ gap: 10 }}>
          <p>
            {tr("Sign makes an")} <strong>{tr("electronic signature with an audit trail")}</strong>{tr(". Each signer's agreement is recorded with the time and their browser, the finished PDF is sealed with a page listing the steps, and the fingerprints (SHA-256) of the files and a chain of the trail's entries let anyone see whether something changed afterwards.")}
          </p>
          <p>
            {tr("It is")} <strong>{tr("not")}</strong> {tr("a PAdES signature, and not an advanced or qualified electronic signature: the PDF carries no certificate that a PDF reader can validate, and signers are identified by their personal link and access code, not by an identity check. \"Check integrity\" compares fingerprints; it is not a certificate check.")}
          </p>
          <p>{tr("If a law or the other party requires a qualified signature, use a trust service provider instead.")}</p>
        </div>
      </Dialog>
    </div>
  );
}

// ───────────────────────── the signers ─────────────────────────

const ICON = { done: Check, now: Clock, later: Clock, declined: X } as const;

function Signers({ request: r, onAgain, onCorrect }: { request: SignRequest; onAgain: (s: Signer) => void; onCorrect: (s: Signer) => void }) {
  const turn = turnOf(r);
  const open = isOpen(r.status);
  const sorted = [...r.signers].sort((a, b) => (r.signing_mode === "Parallel" ? 0 : a.signing_order - b.signing_order));
  return (
    <ol className="sg-people">
      {sorted.map((p) => {
        const state = stepState(p, turn);
        const Icon = ICON[state];
        const note = reminders(p, relativeTime);
        return (
          <li key={p.id ?? p.role} className="sg-person-row" data-state={state}>
            <span className="sg-person-mark" aria-hidden="true">
              <Icon size={14} />
            </span>
            <div className="sg-person-body">
              <div className="sg-person-name">
                <strong>{p.name || p.role}</strong>
                <span className="faint">{isolate(p.email ?? "")}</span>
                <Badge tone={p.status === "Signed" ? "green" : p.status === "Declined" ? "red" : p.status === "Viewed" ? "blue" : "neutral"}>{p.status === "Sent" ? tr("Link sent") : p.status === "Viewed" ? tr("Opened") : p.status}</Badge>
              </div>
              <div className="faint">
                {isolate(p.role)}
                {r.signing_mode !== "Parallel" && r.signers.length > 1 ? tr(" · turn {signing_order}", { signing_order: p.signing_order }) : ""}
                {p.access_code_hash ? tr(" · access code") : ""}
              </div>
              <div className="sg-person-line">{isolate(signerLine(p, when))}</div>
              {note && <div className="faint">{isolate(note)}</div>}
              {p.status === "Declined" && p.decline_reason && <div className="sg-person-reason">“{isolate(p.decline_reason)}”</div>}
            </div>
            {open && p.status !== "Signed" && p.status !== "Declined" && (
              <DropdownMenu>
                <DropdownMenuTrigger asChild>
                  <button type="button" className="icon-btn" aria-label={tr("Actions for {value}", { value: p.name || p.role })}>
                    <Ellipsis size={16} aria-hidden="true" />
                  </button>
                </DropdownMenuTrigger>
                <DropdownMenuContent align="end">
                  {state === "now" && <DropdownMenuItem onSelect={() => onAgain(p)}>{tr("Send their link again")}</DropdownMenuItem>}
                  <DropdownMenuItem onSelect={() => onCorrect(p)}>{tr("Correct their name or email…")}</DropdownMenuItem>
                </DropdownMenuContent>
              </DropdownMenu>
            )}
          </li>
        );
      })}
    </ol>
  );
}

// ───────────────────────── the trail ─────────────────────────

const TRAIL_ICON: Record<string, typeof Check> = { Sent: Mail, Viewed: Eye, Signed: PenLine, Declined: Ban, Reminded: Bell, Resent: Mail, Completed: Check };

function Trail({ events, error, onRetry }: { events: SignEvent[] | undefined; error: unknown; onRetry: () => void }) {
  if (error) return <ErrorState error={error} onRetry={onRetry} title={tr("The trail couldn't be read")} />;
  if (!events) return <Spinner label={tr("Loading the trail")} />;
  if (!events.length) return <p className="muted">{tr("Nothing has happened yet. Each step is recorded here once the request is sent.")}</p>;
  return (
    <ol className="sg-trail">
      {[...events].reverse().map((e) => {
        const t = told(e);
        const Icon = TRAIL_ICON[e.event] ?? Clock;
        return (
          <li key={e.id} data-tone={t.tone}>
            <span className="sg-trail-mark" aria-hidden="true">
              <Icon size={13} />
            </span>
            <div className="sg-trail-body">
              <div className="sg-trail-line">
                <strong>{isolate(t.title)}</strong>
                <time dateTime={e.occurred_at} className="faint" title={e.occurred_at}>
                  {isolate(when(e.occurred_at))}
                </time>
              </div>
              {t.detail && <p className="sg-trail-detail">{isolate(t.detail)}</p>}
              <div className="faint sg-trail-meta">
                {tr("No. {seq}", { seq: e.seq })}
                {e.actor && e.actor !== "system" && e.actor !== e.signer_email ? tr(" · by {actor}", { actor: e.actor }) : e.actor === "system" ? tr(" · automatic") : ""}
              </div>
            </div>
          </li>
        );
      })}
    </ol>
  );
}

// ───────────────────────── a check of integrity ─────────────────────────

function Integrity({ result, at }: { result: Verify; at: number }) {
  return (
    <div className="stack" style={{ gap: 8 }} role="status">
      <p className="sg-verdict" data-ok={result.valid || undefined}>
        {result.valid ? <Check size={15} aria-hidden="true" /> : <X size={15} aria-hidden="true" />}
        {result.valid ? tr("Everything is as it was") : tr("Something doesn't match")}
        <span className="faint"> {tr("· checked")} {isolate(relativeTime(new Date(at).toISOString()))}</span>
      </p>
      <ul className="sg-checks">
        {checks(result).map((c) => (
          <li key={c.label} data-ok={c.ok || undefined}>
            {c.ok ? <Check size={14} aria-hidden="true" /> : <X size={14} aria-hidden="true" />}
            <div>
              <strong>{isolate(c.label)}</strong>
              <div className="faint">{isolate(c.detail)}</div>
              {c.recorded && (
                <details className="sg-hashes">
                  <summary>{tr("Fingerprints (SHA-256)")}</summary>
                  <dl>
                    <dt>{tr("Recorded")}</dt>
                    <dd className="mono">{isolate(c.recorded)}</dd>
                    <dt>{tr("Found now")}</dt>
                    <dd className="mono">{c.found ?? tr("not readable")}</dd>
                  </dl>
                </details>
              )}
            </div>
          </li>
        ))}
      </ul>
      {result.audit_trail.head && (
        <div className="sg-fp">
          <span className="faint">{tr("Trail ends at")}</span>
          <span className="mono" title={result.audit_trail.head}>
            {isolate(short(result.audit_trail.head))}
          </span>
        </div>
      )}
      <p className="faint" style={{ fontSize: 12 }}>
        {tr("This compares fingerprints. The signed PDF carries no certificate signature to validate.")}
      </p>
    </div>
  );
}

// ───────────────────────── withdrawing, correcting ─────────────────────────

function VoidDialog({ request: r, onClose }: { request: SignRequest; onClose: () => void }) {
  const qc = useQueryClient();
  const [reason, setReason] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const submit = async (e: FormEvent) => {
    e.preventDefault();
    setBusy(true);
    try {
      await act(r.id, "void", reason.trim() ? { reason: reason.trim() } : {});
      refresh(qc, r.id);
      toast(tr("Request withdrawn"));
      onClose();
    } catch (err) {
      setError(errorMessage(err));
      setBusy(false);
    }
  };
  return (
    <Dialog
      open
      onClose={() => !busy && onClose()}
      title={tr("Withdraw this request")}
      footer={
        <>
          <Button disabled={busy} onClick={onClose}>
            {tr("Keep it")}
          </Button>
          <Button type="submit" form="void-form" variant="danger" loading={busy}>
            {tr("Withdraw")}
          </Button>
        </>
      }
    >
      <form id="void-form" className="stack" onSubmit={submit}>
        <p>
          {tr("Nobody can sign it afterwards, and anyone who already has a link is told if the workspace sends email.")} {r.signers.some((s) => s.status === "Signed") ? tr("What has been signed so far stays in the trail.") : ""} {tr("You can't undo this.")}
        </p>
        {error && <Alert tone="error">{isolate(error)}</Alert>}
        <Field label={tr("Reason")} htmlFor="void-reason" hint={tr("Optional. It is kept in the trail.")}>
          <textarea id="void-reason" className="textarea" rows={3} maxLength={1000} value={reason} onChange={(e) => setReason(e.target.value)} />
        </Field>
      </form>
    </Dialog>
  );
}

/** A signer who has not signed: their name or email, fixed. The old link stops working. */
function CorrectDialog({ request: r, signer, onClose }: { request: SignRequest; signer: Signer; onClose: () => void }) {
  const qc = useQueryClient();
  const [name, setName] = useState(signer.name ?? "");
  const [email, setEmail] = useState(signer.email ?? "");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const submit = async (e: FormEvent) => {
    e.preventDefault();
    setBusy(true);
    try {
      // Every signer goes back, so none is dropped; only this one's details are given.
      const rows = r.signers.map((s) => (s.id === signer.id ? { id: s.id, name: name.trim(), email: email.trim() } : { id: s.id }));
      const saved = await api.patch<SignRequest>(docPath("SignRequest", r.id), { signers: rows }, { ifMatch: r.version });
      qc.setQueryData(requestQuery(r.id).queryKey, saved);
      refresh(qc, r.id);
      toast(email.trim() !== (signer.email ?? "") ? tr("Corrected. The old link no longer works.") : tr("Corrected"));
      onClose();
    } catch (err) {
      setError(err instanceof ApiError && err.status === 409 ? "This request was changed meanwhile. Close this and try again." : errorMessage(err, false));
      setBusy(false);
    }
  };
  return (
    <Dialog
      open
      onClose={() => !busy && onClose()}
      title={tr("Correct {value}", { value: signer.name || signer.role })}
      footer={
        <>
          <Button disabled={busy} onClick={onClose}>
            {tr("Cancel")}
          </Button>
          <Button type="submit" form="correct-form" variant="primary" loading={busy} disabled={!name.trim() || !email.trim()}>
            {tr("Save")}
          </Button>
        </>
      }
    >
      <form id="correct-form" className="stack" onSubmit={submit}>
        <p className="muted">
          {tr("Changing the email retires their old link. If it is their turn, the new link is emailed, or, when the workspace can't send email, put in your notifications; \"Send their link again\" shows it here.")}
        </p>
        {error && <Alert tone="error">{isolate(error)}</Alert>}
        <Field label={tr("Name")} htmlFor="c-name" required>
          <input id="c-name" className="input" value={name} onChange={(e) => setName(e.target.value)} autoComplete="off" />
        </Field>
        <Field label={tr("Email")} htmlFor="c-email" required>
          <input id="c-email" className="input" type="email" value={email} onChange={(e) => setEmail(e.target.value)} autoComplete="off" />
        </Field>
      </form>
    </Dialog>
  );
}
