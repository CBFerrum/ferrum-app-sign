// One request, followed: where it stands and who is holding it up, a reminder in a click, the signed
// copy once everyone has signed, everything that happened (the audit trail), and a check that the
// files and the trail are as they were. It stays current as signers act.

import { useQuery, useQueryClient } from "@tanstack/react-query";
import { type FormEvent, useCallback, useEffect, useState } from "react";
import { Link, Navigate, useLocation, useNavigate, useParams } from "react-router";
import { Ban, Bell, Check, Clock, Copy, Download, Ellipsis, Eye, FileCheck, Info, Mail, PenLine, X } from "lucide-react";
import { api, ApiError, docPath, download, errorMessage, relativeTime, shell } from "@ferrum/client";
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
  if (await copyText(text)) toast(`${what} copied`);
  else toast("Select it and copy by hand", "error");
}

export function RequestView({ request: r }: { request: SignRequest }) {
  const qc = useQueryClient();
  const navigate = useNavigate();
  const here = useLocation();
  const sent = (here.state as { sent?: LinkRow[] } | null)?.sent;
  const [links, setLinks] = useState<{ title: string; list: LinkRow[] } | null>(sent?.length ? { title: "Request sent", list: sent } : null);
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
        <h1 className="page-title">{r.title}</h1>
        <p className="page-sub">
          {[r.number, r.document_name, r.page_count ? `${r.page_count} ${r.page_count === 1 ? "page" : "pages"}` : null].filter(Boolean).join(" · ")}
        </p>
      </div>
      <div className="page-actions">
        <Badge tone={toneOf(r.status)}>{r.status}</Badge>
        {left && <Badge tone={left.level === "late" ? "red" : left.level === "soon" ? "amber" : "neutral"}>{left.text}</Badge>}
        {open && (
          <Button variant="primary" loading={busy === "remind"} onClick={() => void remind()}>
            <Bell size={15} aria-hidden="true" /> Send a reminder
          </Button>
        )}
        {r.status === "Completed" && r.sealed_document && (
          <Button variant="primary" loading={busy === "download"} onClick={() => void get(r.sealed_document, `${r.title} - signed.pdf`)}>
            <Download size={15} aria-hidden="true" /> Download the signed copy
          </Button>
        )}
        <DropdownMenu>
          <DropdownMenuTrigger asChild>
            <button type="button" className="icon-btn" aria-label="More actions">
              <Ellipsis size={17} aria-hidden="true" />
            </button>
          </DropdownMenuTrigger>
          <DropdownMenuContent align="end">
            {open && <DropdownMenuItem onSelect={() => void again()}>Send the links again</DropdownMenuItem>}
            <DropdownMenuItem onSelect={() => void check()}>Check integrity</DropdownMenuItem>
            <DropdownMenuItem onSelect={() => void get(r.document, r.document_name ?? "document.pdf")}>Download the original</DropdownMenuItem>
            {r.sealed_document && r.status !== "Completed" && <DropdownMenuItem onSelect={() => void get(r.sealed_document, `${r.title} - signed.pdf`)}>Download the signed copy</DropdownMenuItem>}
            {open && (
              <>
                <DropdownMenuSeparator />
                <DropdownMenuItem onSelect={() => setVoiding(true)}>Withdraw this request…</DropdownMenuItem>
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
        <Link to="/">Signatures</Link>
      </div>
      {header}
      {r.status === "Completed" && (
        <Alert tone="success">
          Everyone signed{r.completed_at ? ` on ${when(r.completed_at)}` : ""}. The signed copy carries an audit page listing each step and the fingerprints below.
        </Alert>
      )}
      {open && everyoneSigned && (
        <Alert tone="warning">
          Everyone has signed. The sealed copy is being prepared{r.seal_error ? `, and the last try failed: ${r.seal_error}` : ""}. This is retried every minute; there is nothing to do.
        </Alert>
      )}
      {r.status === "Declined" && declined && (
        <Alert tone="error">
          <strong>{declined.name || declined.role} declined.</strong> {declined.decline_reason ? `“${declined.decline_reason}”` : ""} The request is closed for everyone.
        </Alert>
      )}
      {r.status === "Voided" && <Alert tone="info">You withdrew this request{r.void_reason ? `: “${r.void_reason}”` : ""}. Its links no longer work.</Alert>}
      {r.status === "Expired" && <Alert tone="warning">The last day to sign passed on {when(r.expires_at)}. The links no longer work.</Alert>}

      <div className="sg-req-body">
        <div className="sg-req-main">
          <section aria-labelledby="signers-head">
            <h2 id="signers-head" className="sg-h2">
              Signers <span className="faint">{s.text}</span>
            </h2>
            <Signers request={r} onAgain={(x) => void again(x)} onCorrect={setCorrecting} />
          </section>
          <section aria-labelledby="trail-head">
            <h2 id="trail-head" className="sg-h2">
              What happened
            </h2>
            <Trail events={events.data} error={events.error} onRetry={() => void events.refetch()} />
          </section>
        </div>
        <aside className="sg-req-side" aria-label="Details">
          <section className="sg-side-card" aria-labelledby="doc-head">
            <h3 id="doc-head">Document</h3>
            <p className="sg-wrap">{r.document_name}</p>
            <div className="sg-fp">
              <span className="faint">Fingerprint (SHA-256)</span>
              <span className="mono" title={r.document_sha256 ?? ""}>
                {short(r.document_sha256)}
              </span>
              {r.document_sha256 && (
                <button type="button" className="icon-btn icon-btn-sm" aria-label="Copy the original's fingerprint" onClick={() => void copy(r.document_sha256 ?? "", "Fingerprint")}>
                  <Copy size={13} aria-hidden="true" />
                </button>
              )}
            </div>
            {r.sealed_sha256 && (
              <div className="sg-fp">
                <span className="faint">Signed copy</span>
                <span className="mono" title={r.sealed_sha256}>
                  {short(r.sealed_sha256)}
                </span>
                <button type="button" className="icon-btn icon-btn-sm" aria-label="Copy the signed copy's fingerprint" onClick={() => void copy(r.sealed_sha256 ?? "", "Fingerprint")}>
                  <Copy size={13} aria-hidden="true" />
                </button>
              </div>
            )}
          </section>

          <section className="sg-side-card" aria-labelledby="int-head">
            <h3 id="int-head">Integrity</h3>
            <p className="faint" style={{ fontSize: 12.5 }}>
              Checks that the stored files and the trail are as they were when sent and signed.
            </p>
            <Button loading={busy === "verify"} onClick={() => void check()}>
              <FileCheck size={15} aria-hidden="true" /> {verify ? "Check again" : "Check integrity"}
            </Button>
            {verify && <Integrity result={verify.result} at={verify.at} />}
          </section>

          <section className="sg-side-card" aria-labelledby="det-head">
            <h3 id="det-head">Details</h3>
            <dl className="sg-facts">
              <dt>Sent by</dt>
              <dd>{r.sender_email || "—"}</dd>
              <dt>Order</dt>
              <dd>{r.signing_mode === "Parallel" ? "All at once" : "One after another"}</dd>
              {r.sent_at && (
                <>
                  <dt>Sent</dt>
                  <dd>{when(r.sent_at)}</dd>
                </>
              )}
              {r.expires_at && (
                <>
                  <dt>{open ? "Last day" : "Was due"}</dt>
                  <dd>{when(r.expires_at)}</dd>
                </>
              )}
              {r.reference_entity && r.reference_id && (
                <>
                  <dt>About</dt>
                  <dd>
                    <button type="button" className="link-btn" onClick={() => shell().navigate({ entity: r.reference_entity ?? "", id: r.reference_id ?? "" })}>
                      {r.reference_entity}
                    </button>
                  </dd>
                </>
              )}
              {r.template && (
                <>
                  <dt>Template</dt>
                  <dd>{r._titles?.template ?? "Template"}</dd>
                </>
              )}
            </dl>
            {r.message && <p className="sg-message sg-pre">{r.message}</p>}
          </section>

          <p className="sg-kind">
            <Info size={14} aria-hidden="true" />
            <span>
              An electronic signature with an audit trail, not a certificate-based one.{" "}
              <button type="button" className="link-btn" onClick={() => setAbout(true)}>
                What this means
              </button>
            </span>
          </p>
        </aside>
      </div>

      <LinksDialog open={links !== null} title={links?.title ?? ""} links={links?.list ?? []} onClose={() => setLinks(null)} />
      {voiding && <VoidDialog request={r} onClose={() => setVoiding(false)} />}
      {correcting && <CorrectDialog request={r} signer={correcting} onClose={() => setCorrecting(null)} />}
      <Dialog open={about} onClose={() => setAbout(false)} title="What kind of signature this is" footer={<Button variant="primary" onClick={() => setAbout(false)}>Got it</Button>}>
        <div className="stack sg-about" style={{ gap: 10 }}>
          <p>
            Sign makes an <strong>electronic signature with an audit trail</strong>. Each signer's agreement is recorded with the time and their browser, the finished PDF is sealed with a page listing the steps, and the
            fingerprints (SHA-256) of the files and a chain of the trail's entries let anyone see whether something changed afterwards.
          </p>
          <p>
            It is <strong>not</strong> a PAdES signature, and not an advanced or qualified electronic signature: the PDF carries no certificate that a PDF reader can validate, and signers are identified by their personal link and
            access code, not by an identity check. "Check integrity" compares fingerprints; it is not a certificate check.
          </p>
          <p>If a law or the other party requires a qualified signature, use a trust service provider instead.</p>
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
                <span className="faint">{p.email}</span>
                <Badge tone={p.status === "Signed" ? "green" : p.status === "Declined" ? "red" : p.status === "Viewed" ? "blue" : "neutral"}>{p.status === "Sent" ? "Link sent" : p.status === "Viewed" ? "Opened" : p.status}</Badge>
              </div>
              <div className="faint">
                {p.role}
                {r.signing_mode !== "Parallel" && r.signers.length > 1 ? ` · turn ${p.signing_order}` : ""}
                {p.access_code_hash ? " · access code" : ""}
              </div>
              <div className="sg-person-line">{signerLine(p, when)}</div>
              {note && <div className="faint">{note}</div>}
              {p.status === "Declined" && p.decline_reason && <div className="sg-person-reason">“{p.decline_reason}”</div>}
            </div>
            {open && p.status !== "Signed" && p.status !== "Declined" && (
              <DropdownMenu>
                <DropdownMenuTrigger asChild>
                  <button type="button" className="icon-btn" aria-label={`Actions for ${p.name || p.role}`}>
                    <Ellipsis size={16} aria-hidden="true" />
                  </button>
                </DropdownMenuTrigger>
                <DropdownMenuContent align="end">
                  {state === "now" && <DropdownMenuItem onSelect={() => onAgain(p)}>Send their link again</DropdownMenuItem>}
                  <DropdownMenuItem onSelect={() => onCorrect(p)}>Correct their name or email…</DropdownMenuItem>
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
  if (error) return <ErrorState error={error} onRetry={onRetry} title="The trail couldn't be read" />;
  if (!events) return <Spinner label="Loading the trail" />;
  if (!events.length) return <p className="muted">Nothing has happened yet. Each step is recorded here once the request is sent.</p>;
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
                <strong>{t.title}</strong>
                <time dateTime={e.occurred_at} className="faint" title={e.occurred_at}>
                  {when(e.occurred_at)}
                </time>
              </div>
              {t.detail && <p className="sg-trail-detail">{t.detail}</p>}
              <div className="faint sg-trail-meta">
                No. {e.seq}
                {e.actor && e.actor !== "system" && e.actor !== e.signer_email ? ` · by ${e.actor}` : e.actor === "system" ? " · automatic" : ""}
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
        {result.valid ? "Everything is as it was" : "Something doesn't match"}
        <span className="faint"> · checked {relativeTime(new Date(at).toISOString())}</span>
      </p>
      <ul className="sg-checks">
        {checks(result).map((c) => (
          <li key={c.label} data-ok={c.ok || undefined}>
            {c.ok ? <Check size={14} aria-hidden="true" /> : <X size={14} aria-hidden="true" />}
            <div>
              <strong>{c.label}</strong>
              <div className="faint">{c.detail}</div>
            </div>
          </li>
        ))}
      </ul>
      {result.audit_trail.head && (
        <div className="sg-fp">
          <span className="faint">Trail ends at</span>
          <span className="mono" title={result.audit_trail.head}>
            {short(result.audit_trail.head)}
          </span>
        </div>
      )}
      <p className="faint" style={{ fontSize: 12 }}>
        This compares fingerprints. The signed PDF carries no certificate signature to validate.
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
      toast("Request withdrawn");
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
      title="Withdraw this request"
      footer={
        <>
          <Button disabled={busy} onClick={onClose}>
            Keep it
          </Button>
          <Button type="submit" form="void-form" variant="danger" loading={busy}>
            Withdraw
          </Button>
        </>
      }
    >
      <form id="void-form" className="stack" onSubmit={submit}>
        <p>
          Nobody can sign it afterwards, and anyone who already has a link is told if the workspace sends email. {r.signers.some((s) => s.status === "Signed") ? "What has been signed so far stays in the trail." : ""} You can't undo this.
        </p>
        {error && <Alert tone="error">{error}</Alert>}
        <Field label="Reason" htmlFor="void-reason" hint="Optional. It is kept in the trail.">
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
      toast(email.trim() !== (signer.email ?? "") ? "Corrected. The old link no longer works." : "Corrected");
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
      title={`Correct ${signer.name || signer.role}`}
      footer={
        <>
          <Button disabled={busy} onClick={onClose}>
            Cancel
          </Button>
          <Button type="submit" form="correct-form" variant="primary" loading={busy} disabled={!name.trim() || !email.trim()}>
            Save
          </Button>
        </>
      }
    >
      <form id="correct-form" className="stack" onSubmit={submit}>
        <p className="muted">
          Changing the email retires their old link. If it is their turn, the new link is emailed, or, when the workspace can't send email, put in your notifications; "Send their link again" shows it here.
        </p>
        {error && <Alert tone="error">{error}</Alert>}
        <Field label="Name" htmlFor="c-name" required>
          <input id="c-name" className="input" value={name} onChange={(e) => setName(e.target.value)} autoComplete="off" />
        </Field>
        <Field label="Email" htmlFor="c-email" required>
          <input id="c-email" className="input" type="email" value={email} onChange={(e) => setEmail(e.target.value)} autoComplete="off" />
        </Field>
      </form>
    </Dialog>
  );
}
