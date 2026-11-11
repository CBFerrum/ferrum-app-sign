// A signer's page (Ferrum docs/plan/apps-ui.md §7.3, `shell.public`): what anyone with their link
// opens, with no account. The pages show where the signer's boxes are; one button walks them through
// what is left and a last step asks for their agreement. Everything comes from Sign's public
// endpoints, which hold the rules (who may sign now, what a box accepts, when a link is spent); this
// shows what they say. It works on a phone: one column, a bar for the next step at the thumb, and
// every box a button.

import { type ReactNode, useCallback, useEffect, useMemo, useRef, useState } from "react";
import { Ban, Check, CircleAlert, Clock, Copy, Download, Ellipsis, FileText, Info, Link2Off, PenLine, Signature } from "lucide-react";
import { bootstrap, saveBlob, t as tr, isolate } from "@ferrum/client";
import { Alert, Badge, Button, Dialog, Field, Menu, MenuItem, Spinner, toast, useShellTitle, placed } from "@ferrum/kit";
import { copyText } from "./clipboard";
import { MarkPad, MarkView } from "./Pad";
import { PdfPage, usePdf } from "./PdfPage";
import { boxStyle, Sheet } from "./Sheet";
import {
  actionLabel,
  boxState,
  classify,
  closedCopy,
  complete,
  emptyEntry,
  type Entry,
  type FieldType,
  initialsOf,
  isDone,
  locate,
  type Mark,
  nextTask,
  pagesText,
  pagesWithFields,
  problemsOf,
  progress,
  type Reached,
  sentence,
  signBody,
  type SigningField,
  type SigningView,
  type Task,
  tasksOf,
} from "./signing";
import { deadline, timeLeft } from "./time";
import { short } from "./track";

const enc = encodeURIComponent;
const offline = () => tr("We couldn't reach the server. Check your connection and try again.");

const LABEL: Record<FieldType, string> = {
  get Signature() { return tr("Sign here"); },
  get Initials() { return tr("Initial here"); },
  get "Date Signed"() { return tr("Date signed"); },
  get Name() { return tr("Name"); },
  get Text() { return tr("Text"); },
  get Checkbox() { return tr("Tick"); },
};

const reduced = () => window.matchMedia("(prefers-reduced-motion: reduce)").matches;

type Phase = Reached | { kind: "loading" } | { kind: "signed"; requestStatus: string | null; title: string; number: string | null; at: Date } | { kind: "declined" };

// ───────────────────────── the page ─────────────────────────

export function SigningPage() {
  const link = useMemo(() => locate(window.location.pathname), []);
  const code = useRef("");
  const [phase, setPhase] = useState<Phase>(link ? { kind: "loading" } : { kind: "invalid" });
  const base = link ? `/api/site/${enc(link.tenant)}/sign/signing/${enc(link.token)}` : "";
  useShellTitle(phase.kind === "ready" ? phase.view.request.title : tr("Sign a document"));

  // No session goes along: the link is the signer's whole credential.
  const call = useCallback(
    (path: string, init: RequestInit = {}) =>
      fetch(base + path, {
        ...init,
        cache: "no-store",
        credentials: "omit",
        headers: { ...(init.headers as Record<string, string> | undefined), ...(code.current ? { "X-Access-Code": code.current } : {}) },
      }),
    [base],
  );

  const open = useCallback(async () => {
    if (!link) return;
    setPhase({ kind: "loading" });
    try {
      const r = await call("");
      setPhase(classify(r.status, await r.json().catch(() => null), code.current !== ""));
    } catch {
      setPhase({ kind: "failed", message: offline() });
    }
  }, [call, link]);

  useEffect(() => {
    void open();
  }, [open]);

  switch (phase.kind) {
    case "loading":
      return (
        <Frame>
          <div className="sg-notice" role="status">
            <Spinner />
            <p className="muted">{tr("Opening your document…")}</p>
          </div>
        </Frame>
      );
    case "code":
      return (
        <Frame>
          <CodeForm
            problem={phase.problem}
            onSubmit={(c) => {
              code.current = c;
              void open();
            }}
          />
        </Frame>
      );
    case "locked":
      return (
        <Frame>
          <Notice icon={<Clock size={22} />} title={tr("Too many wrong codes")} text={phase.message}>
            <Button onClick={() => void open()}>{tr("Try again")}</Button>
          </Notice>
        </Frame>
      );
    case "waiting":
      return (
        <Frame>
          <Notice icon={<Clock size={22} />} title={tr("It isn't your turn yet")} text={tr("This document is signed one person at a time, and someone before you hasn't signed yet. You'll be sent this link again when it is your turn, and it will work then.")}>
            <Button onClick={() => void open()}>{tr("Check again")}</Button>
          </Notice>
        </Frame>
      );
    case "invalid":
      return (
        <Frame>
          <Notice
            icon={<Link2Off size={22} />}
            tone="warn"
            title={tr("This link doesn't work")}
            text={tr("It may have been mistyped, or replaced by a newer one. Use the most recent link you were sent, or ask the sender for a new one.")}
          />
        </Frame>
      );
    case "closed": {
      const c = closedCopy(phase.why, phase.requestStatus);
      return (
        <Frame>
          <Notice icon={phase.why === "signed" || phase.why === "completed" ? <Check size={22} /> : <Ban size={22} />} tone={phase.why === "signed" || phase.why === "completed" ? "good" : "warn"} title={c.title} text={c.text} />
        </Frame>
      );
    }
    case "failed":
      return (
        <Frame>
          <Notice icon={<CircleAlert size={22} />} tone="warn" title={tr("This page couldn't open")} text={phase.message}>
            <Button onClick={() => void open()}>{tr("Try again")}</Button>
          </Notice>
        </Frame>
      );
    case "signed":
      return (
        <Frame>
          <Notice
            icon={<Check size={22} />}
            tone="good"
            title={tr("You've signed")}
            text={
              phase.requestStatus === "Completed"
                ? tr("Everyone has now signed, so the document is sealed and the sender has the finished copy.")
                : tr("Your signature is recorded. The others still have to sign; the sender is told when everyone has.")
            }
          >
            <dl className="sg-facts sg-receipt">
              <dt>{tr("Document")}</dt>
              <dd>{isolate(phase.title)}</dd>
              {phase.number && (
                <>
                  <dt>{tr("Reference")}</dt>
                  <dd>{isolate(phase.number)}</dd>
                </>
              )}
              <dt>{tr("Signed")}</dt>
              <dd>{isolate(phase.at.toLocaleString(undefined, { dateStyle: "long", timeStyle: "short" }))}</dd>
            </dl>
            <p className="muted" style={{ maxWidth: "34rem" }}>
              {tr("Once everyone has signed, the sealed copy goes to the sender, and to you by email if the sender's workspace sends email. This link has now been used and can't be used again. You can close this page.")}
            </p>
          </Notice>
        </Frame>
      );
    case "declined":
      return (
        <Frame>
          <Notice icon={<Ban size={22} />} tone="warn" title={tr("You declined to sign")} text={tr("The sender has been told, and this request is closed for everyone. If you change your mind, ask them to send a new one.")} />
        </Frame>
      );
    case "ready":
      return (
        <Ready
          view={phase.view}
          base={base}
          hasCode={code.current !== ""}
          call={call}
          onChange={(r) => setPhase(r)}
          onSigned={(requestStatus) => setPhase({ kind: "signed", requestStatus, title: phase.view.request.title, number: phase.view.request.number ?? null, at: new Date() })}
          onDeclined={() => setPhase({ kind: "declined" })}
          reopen={() => void open()}
        />
      );
  }
}

// ───────────────────────── the page's frame ─────────────────────────

function Frame({ children }: { children: ReactNode }) {
  const workspace = bootstrap()?.workspace.name;
  return (
    <div className="sg-sign">
      <header className="sg-top sg-top-plain">
        <span className="sg-brand">
          <Signature size={18} aria-hidden="true" />
          {workspace ? tr("{workspace} · Sign", { workspace }) : tr("Sign")}
        </span>
      </header>
      <main className="sg-center" id="main">
        {children}
      </main>
    </div>
  );
}

function Notice({ icon, title, text, tone, children }: { icon: ReactNode; title: string; text?: string; tone?: "good" | "warn"; children?: ReactNode }) {
  // The page has changed under the reader: its new heading takes the focus.
  const heading = useRef<HTMLHeadingElement>(null);
  useEffect(() => heading.current?.focus({ preventScroll: true }), []);
  return (
    <div className="sg-notice">
      <span className="sg-notice-icon" data-tone={tone} aria-hidden="true">
        {icon}
      </span>
      <h1 ref={heading} tabIndex={-1}>
        {isolate(title)}
      </h1>
      {text && <p className="muted">{isolate(text)}</p>}
      {children}
    </div>
  );
}

function CodeForm({ problem, onSubmit }: { problem?: string; onSubmit: (code: string) => void }) {
  const [value, setValue] = useState("");
  return (
    <form
      className="sg-notice sg-code"
      onSubmit={(e) => {
        e.preventDefault();
        if (value.trim()) onSubmit(value.trim());
      }}
    >
      <h1>{tr("Enter your access code")}</h1>
      <p className="muted">{tr("The sender gave you a code for this document, separately from the link. Enter it to open the document.")}</p>
      <Field label={tr("Access code")} htmlFor="sg-code" error={problem}>
        <input
          id="sg-code"
          className="input"
          autoFocus
          autoComplete="one-time-code"
          spellCheck={false}
          value={value}
          aria-invalid={problem ? true : undefined}
          aria-describedby={problem ? "sg-code-error" : undefined}
          onChange={(e) => setValue(e.target.value)}
        />
      </Field>
      <Button type="submit" variant="primary" disabled={!value.trim()}>
        {tr("Continue")}
      </Button>
    </form>
  );
}

// ───────────────────────── signing ─────────────────────────

type Call = (path: string, init?: RequestInit) => Promise<Response>;

type Dialogs = null | { kind: "mark"; which: "signature" | "initials" } | { kind: "text"; key: string } | { kind: "review" } | { kind: "decline" } | { kind: "about"; back: Dialogs };

export function Ready({
  view,
  base,
  hasCode,
  call,
  onChange,
  onSigned,
  onDeclined,
  reopen,
}: {
  view: SigningView;
  base: string;
  hasCode: boolean;
  call: Call;
  onChange: (r: Reached) => void;
  onSigned: (requestStatus: string | null) => void;
  onDeclined: () => void;
  reopen: () => void;
}) {
  const { request, signer, fields } = view;
  const name = signer.name ?? "";
  const tasks = useMemo(() => tasksOf(fields), [fields]);
  const pages = useMemo(() => pagesWithFields(fields), [fields]);
  const pdf = usePdf(() => call("/document").then((r) => (r.ok ? r.arrayBuffer() : Promise.reject(new Error(r.statusText)))), base);
  const [entry, setEntry] = useState<Entry>(emptyEntry);
  const [current, setCurrent] = useState<string | null>(() => nextTask(tasks, emptyEntry())?.key ?? null);
  const [dialog, setDialog] = useState<Dialogs>(null);
  const [pending, setPending] = useState<Mark | null>(null);
  const [text, setText] = useState("");
  const [agreed, setAgreed] = useState(false);
  const [problems, setProblems] = useState<Record<string, string>>({});
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const done = progress(tasks, entry);
  const everything = complete(tasks, entry);
  const touched = !!entry.signature || !!entry.initials || Object.keys(entry.values).length > 0;

  // A half-finished signature is lost on a reload: say so before the page goes.
  useEffect(() => {
    if (!touched) return;
    const warn = (e: BeforeUnloadEvent) => e.preventDefault();
    window.addEventListener("beforeunload", warn);
    return () => window.removeEventListener("beforeunload", warn);
  }, [touched]);

  const reveal = (t: Task | null | undefined) => {
    const el = t ? document.getElementById(`box-${t.fields[0].id}`) : null;
    el?.scrollIntoView({ block: "center", behavior: reduced() ? "auto" : "smooth" });
  };

  // The task to do now: the one in hand while it is still open, else the next required one.
  const cur = tasks.find((t) => t.key === current);
  const target = cur && cur.required && !isDone(cur, entry) ? cur : nextTask(tasks, entry, current ?? undefined);

  /** Saves a change, then moves on to what is next. */
  const commit = (change: (e: Entry) => Entry, from: string) => {
    const e = change(entry);
    const nx = nextTask(tasks, e, from);
    setEntry(e);
    setProblems(({ [from]: _gone, ...rest }) => rest);
    setCurrent(nx?.key ?? null);
    setDialog(null);
    requestAnimationFrame(() => reveal(nx ?? tasks.find((t) => t.key === from)));
  };

  const activate = (t: Task) => {
    setCurrent(t.key);
    reveal(t);
    if (t.kind === "signature" || t.kind === "initials") {
      setPending(null);
      setDialog({ kind: "mark", which: t.kind });
    } else if (t.kind === "text") {
      setText(typeof entry.values[t.key] === "string" ? (entry.values[t.key] as string) : "");
      setDialog({ kind: "text", key: t.key });
    } else {
      commit((e) => ({ ...e, values: { ...e.values, [t.key]: e.values[t.key] !== true } }), t.key);
    }
  };

  const primary = () => {
    if (target) activate(target);
    else setDialog({ kind: "review" });
  };

  const submit = async () => {
    setBusy(true);
    setError(null);
    try {
      const r = await call("/sign", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(signBody(fields, entry)) });
      const body = await r.json().catch(() => null);
      if (r.ok) return onSigned(typeof body?.request_status === "string" ? body.request_status : null);
      if (r.status === 422) {
        const found = problemsOf(Array.isArray(body?.fields) ? body.fields : [], fields);
        setProblems(found);
        setError(Object.keys(found).length ? null : sentence(body?.message ?? "") || tr("Something in your answers wasn't accepted."));
        return;
      }
      // Spent or withdrawn, or signed already in another tab: the page says so. Anything else
      // (a busy server, a code that stopped working) leaves what was filled in where it is.
      if (r.status === 404 || r.status === 410) return onChange(classify(r.status, body, hasCode));
      if (r.status === 409) return reopen();
      setError(sentence(body?.message ?? "") || tr("Something went wrong ({status}).", { status: r.status }));
    } catch {
      setError(offline());
    } finally {
      setBusy(false);
    }
  };

  const decline = async (reason: string): Promise<string | null> => {
    try {
      const r = await call("/decline", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ reason }) });
      const body = await r.json().catch(() => null);
      if (r.ok) {
        onDeclined();
        return null;
      }
      if (r.status === 404 || r.status === 410) {
        onChange(classify(r.status, body, hasCode));
        return null;
      }
      return sentence(body?.fields?.[0]?.message ?? body?.message ?? "") || tr("Something went wrong ({status}).", { status: r.status });
    } catch {
      return offline();
    }
  };

  const left = timeLeft(request.expires_at);
  const sender = [request.sender.name, request.sender.organization].filter(Boolean).join(" · ");
  const first = (name || "").split(/\s+/)[0];
  const inOrder = request.signing_mode !== "Parallel";
  const label = actionLabel(target);
  const markTask = dialog?.kind === "mark" ? dialog.which : null;
  const textTask = dialog?.kind === "text" ? tasks.find((t) => t.key === dialog.key) : undefined;

  return (
    <div className="sg-sign">
      <header className="sg-top">
        <div className="sg-top-main">
          <h1 className="sg-title">{isolate(request.title)}</h1>
          <p className="sg-from">
            {sender ? tr("From {sender}", { sender }) : tr("Sent to you for signature")}
            {request.number ? ` · ${request.number}` : ""}
          </p>
        </div>
        <div className="sg-top-side">
          <div className="sg-count" aria-live="polite">
            <strong>
              {tr("{done} of {total}", { done: done.done, total: done.total })}
            </strong>{isolate(" ")}
            {tr("done")}
          </div>
          <div className="sg-meter" role="progressbar" aria-label={tr("Steps done")} aria-valuemin={0} aria-valuemax={done.total} aria-valuenow={done.done}>
            <span style={{ width: `${done.total ? (done.done / done.total) * 100 : 0}%` }} />
          </div>
        </div>
        <Menu ariaLabel={tr("More")} label={<Ellipsis size={18} aria-hidden="true" />} buttonClass="icon-btn" align="end">
          {(close) => (
            <>
              <MenuItem
                onSelect={() => {
                  close();
                  setDialog({ kind: "about", back: null });
                }}
              >
                {tr("About this signature")}
              </MenuItem>
              <MenuItem
                danger
                onSelect={() => {
                  close();
                  setDialog({ kind: "decline" });
                }}
              >
                {tr("Decline to sign…")}
              </MenuItem>
            </>
          )}
        </Menu>
      </header>

      <main className="sg-layout" id="main">
        <div className="sg-pages">
          <div className="sg-intro">
            <p>
              {first ? tr("Hello {first}. ", { first }) : ""}
              <strong>{pages.length ? tr("Your boxes are on {pages}.", { pages: pagesText(pages) }) : tr("There is nothing to fill in on the pages.")}</strong> {tr("Read the whole document, then fill them in.")}
            </p>
            <DocumentLink base={base} hasCode={hasCode} call={call} name={request.document_name} pages={request.pages.length} />
            {!hasCode && !pdf && <Preview src={`${base}/document`} />}
          </div>
          {(pdf ? request.pages.map((p) => p.page) : pages).map((n) => {
            const size = request.pages.find((p) => p.page === n) ?? { page: n, width: 612, height: 792 };
            const here = fields.filter((f) => f.page === n);
            return (
              <Sheet
                key={n}
                number={n}
                count={request.pages.length}
                size={size}
                picture={pdf && <PdfPage pdf={pdf} number={n} />}
                note={here.length ? (here.length === 1 ? tr("1 box for you") : tr("{n} boxes for you", { n: here.length })) : undefined}
              >
                {here.map((f) => {
                  const task = tasks.find((t) => t.fields.some((x) => x.id === f.id));
                  return (
                    <Box key={f.id} field={f} size={size} state={boxState(f, tasks, entry, target?.key ?? null)} entry={entry} name={name} task={task} problem={problems[task?.key ?? ""]} onActivate={activate} />
                  );
                })}
              </Sheet>
            );
          })}
          <p className="sg-end muted">
            {tr("That's everything on the pages.")} <button type="button" className="link-btn" onClick={() => setDialog({ kind: "decline" })}>{tr("Can't sign this? Decline…")}</button>
          </p>
        </div>

        <aside className="sg-aside" aria-label={tr("Your steps")}>
          {request.message && (
            <blockquote className="sg-message">
              <p>{isolate(request.message)}</p>
              {request.sender.name && <footer>{isolate(request.sender.name)}</footer>}
            </blockquote>
          )}
          <dl className="sg-facts">
            {request.expires_at && (
              <>
                <dt>{tr("Sign before")}</dt>
                <dd>
                  {isolate(deadline(request.expires_at))} {left && <Badge tone={left.level === "ok" ? "neutral" : left.level === "soon" ? "amber" : "red"}>{isolate(left.text)}</Badge>}
                </dd>
              </>
            )}
            <dt>{tr("You sign as")}</dt>
            <dd>
              {name || "—"}
              {signer.role ? <span className="faint"> · {isolate(signer.role)}</span> : null}
            </dd>
            {request.document_sha256 && (
              <>
                <dt title={tr("A fingerprint of the file you are asked to sign. It is kept with the signature, so the file can be checked later.")}>{tr("Fingerprint")}</dt>
                <dd className="sg-fp">
                  <span className="mono" title={request.document_sha256}>
                    {isolate(short(request.document_sha256))}
                  </span>
                  <button
                    type="button"
                    className="icon-btn icon-btn-sm"
                    aria-label={tr("Copy the document's fingerprint (SHA-256)")}
                    onClick={async () => ((await copyText(request.document_sha256 ?? "")) ? toast(tr("Fingerprint copied")) : toast(tr("Select it and copy by hand"), "error"))}
                  >
                    <Copy size={13} aria-hidden="true" />
                  </button>
                </dd>
              </>
            )}
            {inOrder && (
              <>
                <dt>{tr("Order")}</dt>
                <dd>{tr("One signer after another. You're up.")}</dd>
              </>
            )}
          </dl>
          <ol className="sg-steps">
            {tasks.map((t) => {
              const isCur = target?.key === t.key;
              const ok = isDone(t, entry);
              return (
                <li key={t.key}>
                  <button type="button" className="sg-step" data-state={ok ? "done" : isCur ? "current" : "todo"} aria-current={isCur ? "step" : undefined} onClick={() => activate(t)}>
                    <span className="sg-step-mark" aria-hidden="true">
                      {ok ? <Check size={13} /> : <PenLine size={13} />}
                    </span>
                    <span className="sg-step-text">
                      <strong>{isolate(t.label)}</strong>
                      <small>
                        {isolate(pagesText([...new Set(t.fields.map((f) => f.page))]))}
                        {t.required ? "" : tr(" · optional")}
                        {ok ? tr(" · done") : ""}
                      </small>
                    </span>
                  </button>
                </li>
              );
            })}
          </ol>
          <div className="sg-primary">
            <Button variant="primary" onClick={primary}>
              {isolate(label)}
            </Button>
            {!everything && target === null && <p className="faint">{tr("Nothing required is left.")}</p>}
          </div>
          <p className="faint" style={{ fontSize: "var(--text-xs)" }}>
            {tr("This link is yours alone: signing with it is recorded as you. Please don't forward it.")}
          </p>
          <p className="sg-kind">
            <Info size={14} aria-hidden="true" /> <span>{tr("An electronic signature with an audit trail, not a certificate-based one.")}</span>{isolate(" ")}
            <button type="button" className="link-btn" onClick={() => setDialog({ kind: "about", back: null })}>
              {tr("What this means")}
            </button>
          </p>
        </aside>
      </main>

      <div className="sg-bar">
        <div className="sg-bar-text">
          <strong>
            {tr("{done} of {total} done", { done: done.done, total: done.total })}
          </strong>
          <span>{everything ? tr("Ready to sign") : tr("Next step")}</span>
        </div>
        <Button variant="primary" onClick={primary}>
          {isolate(label)}
        </Button>
      </div>

      <Dialog
        open={markTask !== null}
        onClose={() => setDialog(null)}
        title={markTask === "initials" ? tr("Your initials") : tr("Your signature")}
        footer={
          <>
            <Button onClick={() => setDialog(null)}>{tr("Cancel")}</Button>
            <Button
              variant="primary"
              disabled={!pending}
              onClick={() => {
                const mark = pending;
                if (mark && markTask) commit((e) => ({ ...e, [markTask]: mark }), markTask);
              }}
            >
              {tr("Use")} {markTask === "initials" ? tr("these initials") : tr("this signature")}
            </Button>
          </>
        }
      >
        {markTask && (
          <>
            <p className="muted" style={{ marginBottom: 12 }}>
              {markTask === "signature"
                ? tr("It goes on every signature box on the document ({value}).", { value: tasks.find((t) => t.key === "signature")?.fields.length ?? 1 })
                : tr("It goes on every initials box on the document ({value}).", { value: tasks.find((t) => t.key === "initials")?.fields.length ?? 1 })}
            </p>
            <MarkPad kind={markTask} typed={markTask === "initials" ? initialsOf(name) : name} onChange={setPending} />
          </>
        )}
      </Dialog>

      <Dialog
        open={textTask !== undefined}
        onClose={() => setDialog(null)}
        title={textTask?.label ?? ""}
        footer={
          <>
            <Button onClick={() => setDialog(null)}>{tr("Cancel")}</Button>
            <Button type="submit" form="sg-text-form" variant="primary" disabled={textTask?.required && !text.trim()}>
              {tr("Save")}
            </Button>
          </>
        }
      >
        <form
          id="sg-text-form"
          onSubmit={(e) => {
            e.preventDefault();
            if (textTask) commit((en) => ({ ...en, values: { ...en.values, [textTask.key]: text.trim() } }), textTask.key);
          }}
        >
          <Field label={textTask?.label ?? ""} htmlFor="sg-text" required={textTask?.required} error={textTask ? problems[textTask.key] : undefined} hint={textTask?.required ? undefined : tr("Optional")}>
            <input id="sg-text" className="input" maxLength={500} autoComplete="off" value={text} onChange={(e) => setText(e.target.value)} />
          </Field>
        </form>
      </Dialog>

      <Dialog
        open={dialog?.kind === "review"}
        onClose={() => !busy && setDialog(null)}
        title={tr("Review and sign")}
        footer={
          <>
            <Button disabled={busy} onClick={() => setDialog(null)}>
              {tr("Back")}
            </Button>
            <Button variant="primary" loading={busy} disabled={!agreed} onClick={() => void submit()}>
              {tr("Sign and finish")}
            </Button>
          </>
        }
      >
        <div className="stack" style={{ gap: 14 }}>
          <p>
            {placed(tr("You are signing {p1} as {p2}{p3}"), { p1: <strong>{isolate(request.title)}</strong>, p2: <strong>{name || tr("the signer")}</strong>, p3: signer.role ? ` (${signer.role})` : "" })}.
          </p>
          {(entry.signature || entry.initials) && (
            <div className="sg-review-marks">
              {entry.signature && (
                <figure>
                  <div className="sg-paper sg-review-mark">
                    <MarkView mark={entry.signature} />
                  </div>
                  <figcaption>{tr("Your signature")}</figcaption>
                </figure>
              )}
              {entry.initials && (
                <figure>
                  <div className="sg-paper sg-review-mark">
                    <MarkView mark={entry.initials} />
                  </div>
                  <figcaption>{tr("Your initials")}</figcaption>
                </figure>
              )}
            </div>
          )}
          {Object.entries(problems).some(([k]) => k !== "consent") && (
            <Alert tone="error">
              <strong>{tr("The sender's document didn't accept everything.")}</strong>
              <ul style={{ margin: "4px 0 0", paddingLeft: 18 }}>
                {Object.entries(problems)
                  .filter(([k]) => k !== "consent")
                  .map(([k, m]) => {
                    const t = tasks.find((x) => x.key === k);
                    return (
                      <li key={k}>
                        {isolate(m)}{isolate(" ")}
                        {t && (
                          <button type="button" className="link-btn" onClick={() => activate(t)}>
                            {tr("Fix it")}
                          </button>
                        )}
                      </li>
                    );
                  })}
              </ul>
            </Alert>
          )}
          {error && <Alert tone="error">{isolate(error)}</Alert>}
          <label className="sg-consent">
            <input type="checkbox" checked={agreed} onChange={(e) => setAgreed(e.target.checked)} />
            <span>{isolate(view.consent)}</span>
          </label>
          {problems.consent && <p className="field-error">{isolate(problems.consent)}</p>}
          <p className="sg-kind">
            <Info size={14} aria-hidden="true" />{isolate(" ")}
            <span>
              {tr("This is an electronic signature with an audit trail. It is not a PAdES, advanced or qualified electronic signature.")}{isolate(" ")}
              <button type="button" className="link-btn" onClick={() => setDialog({ kind: "about", back: { kind: "review" } })}>
                {tr("What this means")}
              </button>
            </span>
          </p>
        </div>
      </Dialog>

      <DeclineDialog open={dialog?.kind === "decline"} onClose={() => setDialog(null)} onDecline={decline} />
      <AboutDialog open={dialog?.kind === "about"} onClose={() => setDialog(dialog?.kind === "about" ? dialog.back : null)} />
    </div>
  );
}

// ───────────────────────── the sheets' boxes ─────────────────────────

function Box({
  field: f,
  size,
  state,
  entry,
  name,
  task,
  problem,
  onActivate,
}: {
  field: SigningField;
  size: { width: number; height: number };
  state: ReturnType<typeof boxState>;
  entry: Entry;
  name: string;
  task: Task | undefined;
  problem?: string;
  onActivate: (t: Task) => void;
}) {
  const style = boxStyle(f, size);
  const word = f.label || LABEL[f.field_type];
  const text = task?.kind === "text" && typeof entry.values[task.key] === "string" ? (entry.values[task.key] as string) : "";
  const mark = f.field_type === "Signature" ? entry.signature : f.field_type === "Initials" ? entry.initials : null;
  let content: ReactNode;
  switch (f.field_type) {
    case "Signature":
    case "Initials": {
      content = mark ? (
        <MarkView mark={mark} />
      ) : (
        <>
          <PenLine size="1.1em" aria-hidden="true" />
          <span>{isolate(LABEL[f.field_type])}</span>
        </>
      );
      break;
    }
    case "Name":
      content = <span>{name || tr("Name")}</span>;
      break;
    case "Date Signed":
      content = <span>{tr("Date signed")}</span>;
      break;
    case "Text":
      content = <span className="sg-box-text">{text || word}</span>;
      break;
    case "Checkbox":
      content = state === "done" ? <Check size="1.2em" aria-hidden="true" /> : <span className="sg-tick" aria-hidden="true" />;
      break;
  }
  const status = state === "auto" ? tr("filled in for you") : state === "done" ? "done" : f.required ? "required" : "optional";
  if (state === "auto" || !task) {
    return (
      <div id={`box-${f.id}`} className="sg-box" data-state="auto" data-kind={f.field_type} style={style} title={tr("{word}: filled in for you when you sign", { word })}>
        {content}
      </div>
    );
  }
  return (
    <button
      type="button"
      id={`box-${f.id}`}
      className={mark ? "sg-box sg-paper" : "sg-box"}
      data-state={state}
      data-kind={f.field_type}
      data-problem={problem ? "" : undefined}
      style={style}
      title={problem ?? word}
      aria-label={(problem ? tr("{word}, page {page}, {status}. {problem}", { word, page: f.page, status, problem }) : tr("{word}, page {page}, {status}", { word, page: f.page, status }))}
      onClick={() => onActivate(task)}
    >
      {content}
    </button>
  );
}

// ───────────────────────── the document ─────────────────────────

/** The PDF in the page, for a computer with a mouse: phones cannot show one in a page (they open
 *  it), and a link cannot carry an access code, so those who have one download it. It loads only
 *  when opened. */
function Preview({ src }: { src: string }) {
  const [open, setOpen] = useState(false);
  if (!window.matchMedia("(hover: hover) and (pointer: fine)").matches) return null;
  return (
    <details className="sg-read" onToggle={(e) => setOpen(e.currentTarget.open)}>
      <summary>{tr("Read the document here")}</summary>
      {open && <iframe className="sg-frame" title={tr("The document to sign")} src={src} />}
    </details>
  );
}

/** The PDF itself, in the browser's own viewer: a link when no code is needed, else a download
 *  (a link cannot carry the code). */
function DocumentLink({ base, hasCode, call, name, pages }: { base: string; hasCode: boolean; call: Call; name?: string | null; pages: number }) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const label = hasCode
    ? !pages ? tr("Download the PDF") : pages === 1 ? tr("Download the PDF (1 page)") : tr("Download the PDF ({n} pages)", { n: pages })
    : !pages ? tr("Open the PDF") : pages === 1 ? tr("Open the PDF (1 page)") : tr("Open the PDF ({n} pages)", { n: pages });
  if (!hasCode)
    return (
      <a className="btn btn-md" href={`${base}/document`} target="_blank" rel="noopener noreferrer">
        <FileText size={15} aria-hidden="true" /> {isolate(label)}
      </a>
    );
  const get = async () => {
    setBusy(true);
    setError(null);
    try {
      const r = await call("/document");
      if (!r.ok) throw new Error((await r.json().catch(() => null))?.message ?? tr("The document couldn't be fetched."));
      saveBlob(await r.blob(), name || "document.pdf");
    } catch (e) {
      setError(e instanceof Error ? e.message : offline());
    } finally {
      setBusy(false);
    }
  };
  return (
    <span>
      <Button loading={busy} onClick={() => void get()}>
        <Download size={15} aria-hidden="true" /> {isolate(label)}
      </Button>
      {error && (
        <span className="field-error" role="alert">
          {isolate(" ")}
          {isolate(error)}
        </span>
      )}
    </span>
  );
}

// ───────────────────────── declining, and what this signature is ─────────────────────────

export function DeclineDialog({ open, onClose, onDecline }: { open: boolean; onClose: () => void; onDecline: (reason: string) => Promise<string | null> }) {
  const [reason, setReason] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  return (
    <Dialog
      open={open}
      onClose={() => !busy && onClose()}
      title={tr("Decline to sign")}
      footer={
        <>
          <Button disabled={busy} onClick={onClose}>
            {tr("Go back")}
          </Button>
          <Button
            variant="danger"
            loading={busy}
            disabled={!reason.trim()}
            onClick={async () => {
              setBusy(true);
              setError(await onDecline(reason.trim()));
              setBusy(false);
            }}
          >
            {tr("Decline")}
          </Button>
        </>
      }
    >
      <div className="stack" style={{ gap: 12 }}>
        <p>
          {placed(tr("Declining ends this request {p1}, and the sender is told why. You can't undo it from here."), { p1: <strong>{tr("for everyone")}</strong> })}
        </p>
        <Field label={tr("Why are you declining?")} htmlFor="sg-reason" required error={error ?? undefined} hint={tr("The sender reads this.")}>
          <textarea id="sg-reason" className="textarea" rows={3} maxLength={1000} value={reason} onChange={(e) => setReason(e.target.value)} />
        </Field>
      </div>
    </Dialog>
  );
}

export function AboutDialog({ open, onClose }: { open: boolean; onClose: () => void }) {
  return (
    <Dialog open={open} onClose={onClose} title={tr("What kind of signature this is")} footer={<Button variant="primary" onClick={onClose}>{tr("Got it")}</Button>}>
      <div className="stack sg-about" style={{ gap: 10 }}>
        <p>
          {placed(tr("This is an {p1}. When you sign, Sign records that you agreed, when, and from which browser, and seals the finished PDF with an extra page that lists those steps. If the file is changed afterwards, a check of it shows."), { p1: <strong>{tr("electronic signature with an audit trail")}</strong> })}
        </p>
        <p>
          {placed(tr("It is {p1} a PAdES signature, and not an advanced or qualified electronic signature. The PDF carries no certificate that a PDF reader can validate, and you are identified by your personal link (and your access code, if you were given one), not by an identity check."), { p1: <strong>{tr("not")}</strong> })}
        </p>
        <p>
          {tr("Many agreements can be signed this way; whether it is enough for yours is for you and the sender to decide. If a law or the other side needs a qualified signature, don't sign here.")}
        </p>
      </div>
    </Dialog>
  );
}
