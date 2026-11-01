// Getting a request ready: who signs and in what order, where each of them signs, and a last look
// before it goes. The draft is the server's, saved against the version on screen; leaving with
// changes asks first. What stops a request being sent is said in words about people, and the server
// has the last word when it is sent.

import { useQuery, useQueryClient } from "@tanstack/react-query";
import { type CSSProperties, useCallback, useEffect, useRef, useState } from "react";
import { Link, Navigate, useNavigate, useParams } from "react-router";
import { ArrowDown, ArrowUp, Check, Info, Plus, Send, Trash } from "lucide-react";
import { api, ApiError, docPath, download, errorMessage, type FieldError, todayIso } from "@ferrum/client";
import { Alert, Badge, Button, confirmDialog, ErrorState, Field, PageLoading, toast, useLiveEntity, useShellTitle, useUnsavedGuard } from "@ferrum/kit";
import { refresh, requestQuery, sendRequest } from "./data";
import { Editor, useBoxes } from "./Editor";
import { pageSizes, type SignRequest } from "./model";
import { colourOf } from "./placement";
import { addSigner, bodyOf, boxCounts, changed, type Draft, draftOf, editSigner, looksLikeEmail, moveSigner, type Problem, removeSigner, sendProblems, type SignerDraft, turns } from "./send";
import { short } from "./track";

type StepId = "signers" | "fields" | "review";
const STEPS: { id: StepId; label: string }[] = [
  { id: "signers", label: "Signers" },
  { id: "fields", label: "Where to sign" },
  { id: "review", label: "Review" },
];

export function PrepareRoute() {
  const { id = "", step = "signers" } = useParams();
  const q = useQuery(requestQuery(id));
  useShellTitle(q.data ? `${q.data.title}: prepare` : "Prepare a request");
  if (q.error) return <ErrorState error={q.error} onRetry={() => void q.refetch()} />;
  if (!q.data) return <PageLoading />;
  // Only a draft can be prepared: a sent request is followed, not edited.
  if (q.data.status !== "Draft") return <Navigate to={`/request/${id}`} replace />;
  const at = STEPS.some((s) => s.id === step) ? (step as StepId) : "signers";
  return <Prepare key={id} request={q.data} step={at} />;
}

function Prepare({ request, step }: { request: SignRequest; step: StepId }) {
  const qc = useQueryClient();
  const navigate = useNavigate();
  const id = request.id;
  const base = useRef(request);
  const ed = useBoxes(draftOf(request).boxes);
  const [form, setForm] = useState(() => {
    const { boxes: _boxes, ...rest } = draftOf(request);
    return rest;
  });
  const draft: Draft = { ...form, boxes: ed.boxes };
  const dirty = changed(draft, draftOf(base.current));
  const [saving, setSaving] = useState(false);
  const [sending, setSending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [rows, setRows] = useState<FieldError[]>([]);
  const [stale, setStale] = useState(false);
  const { allowLeave, dialog } = useUnsavedGuard(dirty);
  const today = todayIso();

  /** Edits the draft: its form, and its boxes with their undo. */
  const apply = (change: (d: Draft) => Draft) => {
    const next = change(draft);
    const { boxes, ...rest } = next;
    setForm(rest);
    if (boxes !== ed.boxes) ed.set(boxes);
  };

  // A change made somewhere else (or by this very save) is read again.
  useLiveEntity("SignRequest", useCallback((e) => void (e.id === id && qc.invalidateQueries({ queryKey: requestQuery(id).queryKey })), [qc, id]));

  // Someone else saved this request meanwhile: take it if nothing here is unsaved, else say so.
  useEffect(() => {
    if (request.version === base.current.version) return;
    if (dirty) return setStale(true);
    adopt(request, draft);
  }, [request.version]); // only a new version of the record is news

  /** Takes what the server holds as the draft, keeping the keys the screen already uses. */
  function adopt(saved: SignRequest, from: Draft) {
    const fresh = draftOf(saved);
    const { boxes: _boxes, ...rest } = fresh;
    setForm({ ...rest, signers: fresh.signers.map((s, i) => ({ ...s, key: from.signers[i]?.key ?? s.key })) });
    const ids = new Map<string, string>();
    saved.fields.forEach((f, i) => from.boxes[i] && f.id && ids.set(from.boxes[i].key, f.id));
    if (saved.fields.length === from.boxes.length) ed.adopt(ids);
    else ed.reset(fresh.boxes);
    base.current = saved;
    setStale(false);
  }

  const save = async (): Promise<boolean> => {
    if (!dirty) return true;
    setSaving(true);
    setError(null);
    setRows([]);
    try {
      const saved = await api.patch<SignRequest>(docPath("SignRequest", id), bodyOf(draft), { ifMatch: base.current.version });
      adopt(saved, draft);
      qc.setQueryData(requestQuery(id).queryKey, saved);
      refresh(qc);
      return true;
    } catch (e) {
      setError(errorMessage(e, false));
      setRows(e instanceof ApiError ? e.fields : []);
      if (e instanceof ApiError && e.status === 409) setStale(true);
      return false;
    } finally {
      setSaving(false);
    }
  };

  const go = async (to: StepId) => {
    if (!(await save())) return;
    allowLeave();
    navigate(`/request/${id}/prepare/${to}`);
  };
  const reload = async () => {
    const fresh = await qc.fetchQuery({ ...requestQuery(id), staleTime: 0 });
    adopt(fresh, draft);
    setError(null);
  };
  const send = async () => {
    if (!(await save())) return;
    setSending(true);
    try {
      const out = await sendRequest(id);
      refresh(qc, id);
      allowLeave();
      navigate(`/request/${id}`, { replace: true, state: { sent: out.signers } });
    } catch (e) {
      setError(errorMessage(e));
      setRows(e instanceof ApiError ? e.fields : []);
      setSending(false);
    }
  };

  const problems = sendProblems(draft, today);
  const pages = pageSizes(request);
  const at = STEPS.findIndex((s) => s.id === step);
  const roles = draft.signers.filter((s) => s.role.trim()).map((s) => ({ role: s.role, label: s.name.trim() || s.role }));

  return (
    <div className="sg-app sg-prepare">
      <div className="crumbs">
        <Link to="/">Signatures</Link>
      </div>
      <header className="sg-prep-head">
        <div>
          <h1 className="page-title">{draft.title.trim() || "Untitled request"}</h1>
          <p className="page-sub">
            {[request.document_name, request.page_count ? `${request.page_count} ${request.page_count === 1 ? "page" : "pages"}` : null].filter(Boolean).join(" · ")}
            {" · "}
            <span aria-live="polite">{saving ? "Saving…" : dirty ? "Unsaved changes" : "Saved"}</span>
          </p>
        </div>
        <nav aria-label="Steps" className="sg-steps-nav">
          <ol>
            <li data-state="done">
              <span className="sg-step-dot" aria-hidden="true">
                <Check size={12} />
              </span>
              Document
            </li>
            {STEPS.map((s, i) => (
              <li key={s.id} data-state={i < at ? "done" : i === at ? "now" : undefined}>
                <button type="button" aria-current={i === at ? "step" : undefined} disabled={saving || sending} onClick={() => void go(s.id)}>
                  <span className="sg-step-dot" aria-hidden="true">
                    {i < at ? <Check size={12} /> : i + 2}
                  </span>
                  {s.label}
                </button>
              </li>
            ))}
          </ol>
        </nav>
      </header>

      {stale && (
        <Alert
          tone="warning"
          action={
            <Button size="sm" onClick={() => void reload()}>
              Load their version
            </Button>
          }
        >
          This request was changed somewhere else. {dirty ? "Your unsaved changes would be lost by loading it; saving now is refused." : ""}
        </Alert>
      )}
      {error && <Alert tone="error">{error}</Alert>}

      {step === "signers" && <SignersStep draft={draft} apply={apply} rows={rows} problems={problems} />}
      {step === "fields" && (
        <Editor
          ed={ed}
          roles={roles}
          pages={pages}
          document={request.document}
          onPdf={() => download(request.document ?? "", request.document_name ?? "document.pdf").catch((e) => toast(errorMessage(e), "error"))}
        />
      )}
      {step === "review" && <Review request={request} draft={draft} problems={problems} go={(s) => void go(s)} />}

      <footer className="sg-prep-foot">
        <Button disabled={at <= 0 || saving || sending} onClick={() => void go(STEPS[at - 1].id)}>
          Back
        </Button>
        <span className="spacer" />
        <Button loading={saving && !sending} disabled={!dirty || sending} onClick={() => void save()}>
          Save draft
        </Button>
        {step !== "review" ? (
          <Button variant="primary" disabled={saving} onClick={() => void go(STEPS[at + 1].id)}>
            Continue
          </Button>
        ) : (
          <Button variant="primary" loading={sending} disabled={problems.length > 0 || saving} onClick={() => void send()}>
            <Send size={15} aria-hidden="true" /> Send for signature
          </Button>
        )}
      </footer>
      {dialog}
    </div>
  );
}

// ───────────────────────── who signs ─────────────────────────

const rowMessage = (rows: FieldError[], table: string, i: number, field: string) => rows.find((r) => r.row?.[0] === table && r.row[1] === i && r.field === field)?.message;

export function SignersStep({ draft, apply, rows, problems }: { draft: Draft; apply: (c: (d: Draft) => Draft) => void; rows: FieldError[]; problems: Problem[] }) {
  const inTurn = draft.signing_mode === "Sequential";
  const remove = async (s: SignerDraft) => {
    const boxes = draft.boxes.filter((b) => b.role.trim().toLowerCase() === s.role.trim().toLowerCase()).length;
    if (boxes && !(await confirmDialog({ title: `Remove ${s.name || s.role}?`, message: `Their ${boxes} ${boxes === 1 ? "box goes" : "boxes go"} with them.`, confirmLabel: "Remove", danger: true }))) return;
    apply((d) => removeSigner(d, s.key));
  };
  const shown = (step: "signers" | "details") => problems.filter((p) => p.step === step);
  return (
    <div className="sg-prep-grid">
      <section className="stack" aria-labelledby="who-head">
        <h2 id="who-head" className="sg-h2">
          Who signs
        </h2>
        <div className="sg-segmented" role="group" aria-label="Signing order">
          {(["Sequential", "Parallel"] as const).map((m) => (
            <button key={m} type="button" aria-pressed={draft.signing_mode === m} onClick={() => apply((d) => ({ ...d, signing_mode: m }))}>
              {m === "Sequential" ? "One after another" : "All at once"}
            </button>
          ))}
        </div>
        <p className="faint" style={{ fontSize: 12.5 }}>
          {inTurn ? "Each person gets their link when the one before has signed. Give two people the same number to let them sign together." : "Everyone gets their link now and signs when they like."}
        </p>
        {draft.signers.length === 0 && (
          <Alert tone="info">
            Add the people who sign. Each gets a personal link by email, with no account needed; you place where they sign on the next step.
          </Alert>
        )}
        <ol className="sg-signers">
          {draft.signers.map((s, i) => (
            <li key={s.key} className="sg-signer">
              <div className="sg-signer-head">
                <span className={`sg-dot sg-c${colourOf(s.role, draft.signers.map((x) => x.role))}`} aria-hidden="true" />
                <strong>{s.name.trim() || s.role.trim() || `Signer ${i + 1}`}</strong>
                {inTurn && <Badge tone="neutral">{`Turn ${s.signing_order}`}</Badge>}
                <span className="spacer" />
                <button type="button" className="icon-btn" aria-label={`Move ${s.name || s.role} up`} disabled={i === 0} onClick={() => apply((d) => moveSigner(d, s.key, -1))}>
                  <ArrowUp size={15} aria-hidden="true" />
                </button>
                <button type="button" className="icon-btn" aria-label={`Move ${s.name || s.role} down`} disabled={i === draft.signers.length - 1} onClick={() => apply((d) => moveSigner(d, s.key, 1))}>
                  <ArrowDown size={15} aria-hidden="true" />
                </button>
                <button type="button" className="icon-btn" aria-label={`Remove ${s.name || s.role}`} onClick={() => void remove(s)}>
                  <Trash size={15} aria-hidden="true" />
                </button>
              </div>
              <div className="form-grid" style={{ "--cols": inTurn ? 3 : 2 } as CSSProperties}>
                <Field label="Name" htmlFor={`s-name-${i}`} required error={rowMessage(rows, "signers", i, "name")}>
                  <input id={`s-name-${i}`} className="input" value={s.name} autoComplete="off" onChange={(e) => apply((d) => editSigner(d, s.key, { name: e.target.value }))} />
                </Field>
                <Field label="Email" htmlFor={`s-email-${i}`} required error={rowMessage(rows, "signers", i, "email") ?? (s.email.trim() && !looksLikeEmail(s.email) ? "This doesn't look like an email address." : undefined)}>
                  <input id={`s-email-${i}`} className="input" type="email" value={s.email} autoComplete="off" onChange={(e) => apply((d) => editSigner(d, s.key, { email: e.target.value }))} />
                </Field>
                {inTurn && (
                  <Field label="Turn" htmlFor={`s-turn-${i}`} hint="Same number: together.">
                    <input
                      id={`s-turn-${i}`}
                      className="input num"
                      type="number"
                      min={1}
                      max={50}
                      value={s.signing_order}
                      onChange={(e) => apply((d) => editSigner(d, s.key, { signing_order: Math.min(50, Math.max(1, Math.round(Number(e.target.value)) || 1)) }))}
                    />
                  </Field>
                )}
                <Field label="Role" htmlFor={`s-role-${i}`} required hint="What they are in this document, such as Client. Their boxes go with it." error={rowMessage(rows, "signers", i, "role")}>
                  <input id={`s-role-${i}`} className="input" value={s.role} maxLength={60} autoComplete="off" onChange={(e) => apply((d) => editSigner(d, s.key, { role: e.target.value }))} />
                </Field>
                <Field
                  label="Access code"
                  htmlFor={`s-code-${i}`}
                  hint={s.hasCode ? "A code is set. Type a new one to replace it." : "Optional. Tell them separately; it is never emailed."}
                  error={rowMessage(rows, "signers", i, "access_code")}
                >
                  <input id={`s-code-${i}`} className="input" value={s.accessCode} autoComplete="off" minLength={4} maxLength={64} onChange={(e) => apply((d) => editSigner(d, s.key, { accessCode: e.target.value }))} />
                </Field>
              </div>
              {shown("signers")
                .filter((p) => p.signer === s.key)
                .map((p) => (
                  <p key={p.text} className="sg-hint-warn">
                    {p.text}
                  </p>
                ))}
            </li>
          ))}
        </ol>
        <div>
          <Button onClick={() => apply((d) => addSigner(d))}>
            <Plus size={15} aria-hidden="true" /> Add a signer
          </Button>
        </div>
      </section>

      <section className="stack" aria-labelledby="about-head">
        <h2 id="about-head" className="sg-h2">
          About this request
        </h2>
        <Field label="Title" htmlFor="d-title" required hint="Signers see this in their email and on their page.">
          <input id="d-title" className="input" value={draft.title} maxLength={200} onChange={(e) => apply((d) => ({ ...d, title: e.target.value }))} />
        </Field>
        <Field label="Message to the signers" htmlFor="d-message" hint="Optional.">
          <textarea id="d-message" className="textarea" rows={4} maxLength={5000} value={draft.message} onChange={(e) => apply((d) => ({ ...d, message: e.target.value }))} />
        </Field>
        <Field label="Last day to sign" htmlFor="d-expires" hint="Empty: the workspace's usual time after sending. Links stop working at the end of that day.">
          <input id="d-expires" className="input" type="date" min={todayIso()} value={draft.expires_on} onChange={(e) => apply((d) => ({ ...d, expires_on: e.target.value }))} />
        </Field>
        {shown("details").map((p) => (
          <p key={p.text} className="sg-hint-warn">
            {p.text}
          </p>
        ))}
      </section>
    </div>
  );
}

// ───────────────────────── the last look ─────────────────────────

export function Review({ request, draft, problems, go }: { request: SignRequest; draft: Draft; problems: Problem[]; go: (s: StepId) => void }) {
  const groups = turns(draft.signers);
  const parallel = draft.signing_mode === "Parallel";
  const first = parallel ? draft.signers : (groups[0] ?? []);
  const names = (list: SignerDraft[]) => list.map((s) => s.name.trim() || s.role).join(" and ");
  return (
    <div className="sg-prep-grid">
      <section className="stack" aria-labelledby="rev-head">
        <h2 id="rev-head" className="sg-h2">
          {problems.length ? "Before it can go" : "Ready to send"}
        </h2>
        {problems.length > 0 && (
          <Alert tone="warning">
            <ul className="sg-problems">
              {problems.map((p) => (
                <li key={p.text}>
                  {p.text}{" "}
                  <button type="button" className="link-btn" onClick={() => go(p.step === "fields" ? "fields" : "signers")}>
                    Fix it
                  </button>
                </li>
              ))}
            </ul>
          </Alert>
        )}
        <dl className="sg-summary">
          <dt>Document</dt>
          <dd>
            {request.document_name}
            <span className="faint">
              {" "}
              · {request.page_count} {request.page_count === 1 ? "page" : "pages"} · fingerprint <span className="mono" title={request.document_sha256 ?? ""}>{short(request.document_sha256)}</span>
            </span>
          </dd>
          <dt>Title</dt>
          <dd>{draft.title || <span className="faint">None</span>}</dd>
          <dt>Last day</dt>
          <dd>{draft.expires_on || <span className="faint">The workspace's usual time after sending</span>}</dd>
          {draft.message.trim() && (
            <>
              <dt>Message</dt>
              <dd className="sg-pre">{draft.message}</dd>
            </>
          )}
        </dl>
        <h3 className="sg-h3">Signers</h3>
        <ol className="sg-sum-signers">
          {draft.signers.map((s) => (
            <li key={s.key}>
              <span className={`sg-dot sg-c${colourOf(s.role, draft.signers.map((x) => x.role))}`} aria-hidden="true" />
              <div>
                <strong>{s.name || s.role}</strong> <span className="faint">{s.email}</span>
                <div className="faint" style={{ fontSize: 12.5 }}>
                  {s.role} · {boxCounts(draft.boxes, s.role)}
                  {!parallel && ` · turn ${s.signing_order}`}
                  {(s.hasCode || s.accessCode) && " · access code"}
                </div>
              </div>
            </li>
          ))}
        </ol>
      </section>

      <section className="stack" aria-labelledby="next-head">
        <h2 id="next-head" className="sg-h2">
          What happens next
        </h2>
        <ul className="sg-next">
          <li>{first.length ? `${names(first)} ${first.length > 1 ? "get" : "gets"} a personal link to sign${parallel ? "" : " first"}.` : "Nobody has been added yet."}</li>
          {!parallel && groups.length > 1 && <li>The next {groups.length > 2 ? "people follow" : "person follows"} when the ones before have signed.</li>}
          <li>If your workspace can't send email, you are given each link to pass on.</li>
          <li>You see who has opened and signed, can send reminders, and download the sealed copy when everyone has signed.</li>
        </ul>
        <Alert tone="info">
          <div className="sg-kind-note">
            <Info size={16} aria-hidden="true" />
            <div>
              <strong>What kind of signature this is.</strong> Sign makes an electronic signature with an audit trail: it records who agreed, when and from which browser, and seals the PDF with a page that lists those steps and fingerprints. It does not add a PAdES, advanced or qualified
              signature, and signers are identified by their personal link (and access code), not by an identity check. If you need a qualified signature, use a trust service provider.
            </div>
          </div>
        </Alert>
      </section>
    </div>
  );
}
