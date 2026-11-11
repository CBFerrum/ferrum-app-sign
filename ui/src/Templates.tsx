// Templates: a PDF with its roles and boxes placed once, used again and again. The library lists
// them; "Use" asks who signs this time and makes a draft that is ready to review. (Used from the
// start screen too, where it is the second way to begin.)

import { useQuery } from "@tanstack/react-query";
import { type FormEvent, useState } from "react";
import { Link, useNavigate } from "react-router";
import { FileText, Plus } from "lucide-react";
import { api, docPath, errorMessage, type FieldError, t as tr, isolate } from "@ferrum/client";
import { Alert, Button, Dialog, Field, FormSkeleton, ListSkeleton, StatePanel, useShellTitle } from "@ferrum/kit";
import { can, useEntityMeta } from "@ferrum/records";
import { templateQuery, templatesQuery } from "./data";
import { looksLikeEmail, peopleOf, type Person, templateBody } from "./send";
import { Nav } from "./Nav";
import type { Template } from "./model";

/** Who signs this time, then the draft. */
export function UseTemplate({ id, onClose }: { id: string; onClose: () => void }) {
  const navigate = useNavigate();
  const q = useQuery(templateQuery(id));
  const t = q.data;
  return (
    <Dialog open onClose={onClose} title={t ? tr("Use “{template_name}”", { template_name: t.template_name }) : tr("Use a template")} wide>
      {q.error ? <StatePanel kind="failed" error={q.error} onRetry={() => void q.refetch()} /> : !t ? <FormSkeleton fields={3} /> : <Form key={t.id} template={t} onClose={onClose} onMade={(rid) => navigate(`/request/${rid}/prepare/review`)} />}
    </Dialog>
  );
}

function Form({ template: t, onClose, onMade }: { template: Template; onClose: () => void; onMade: (id: string) => void }) {
  const [who, setWho] = useState(() => peopleOf(t));
  const [title, setTitle] = useState(t.title || t.template_name);
  const [message, setMessage] = useState(t.message ?? "");
  const [expires, setExpires] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [rows, setRows] = useState<FieldError[]>([]);
  const set = (i: number, change: Partial<Person>) => setWho((w) => w.map((p, j) => (j === i ? { ...p, ...change } : p)));
  const ready = !!title.trim() && who.every((p) => p.name.trim() && looksLikeEmail(p.email));

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    setBusy(true);
    setError(null);
    setRows([]);
    try {
      const made = await api.post<{ id: string }>(docPath("SignTemplate", t.id, "actions", "create_request"), templateBody(title, message, expires, who));
      onMade(made.id);
    } catch (err) {
      setError(errorMessage(err, false));
      setRows((err as { fields?: FieldError[] }).fields ?? []);
      setBusy(false);
    }
  };
  const problem = (i: number, field: string) => rows.find((r) => r.row?.[1] === i && r.field === field)?.message;

  return (
    <form className="stack" onSubmit={submit}>
      <p className="muted">
        {tr("Say who signs this time. The document and where each person signs come from the template; you can still change them before sending.")}
      </p>
      {error && <Alert tone="error">{isolate(error)}</Alert>}
      {who.map((p, i) => (
        <fieldset key={p.role} className="sg-person">
          <legend>{isolate(p.role)}</legend>
          <div className="form-grid">
            <Field label={tr("Name")} htmlFor={`u-name-${i}`} required error={problem(i, "name")}>
              <input id={`u-name-${i}`} className="input" value={p.name} onChange={(e) => set(i, { name: e.target.value })} autoComplete="off" />
            </Field>
            <Field label={tr("Email")} htmlFor={`u-email-${i}`} required error={problem(i, "email")}>
              <input id={`u-email-${i}`} className="input" type="email" value={p.email} onChange={(e) => set(i, { email: e.target.value })} autoComplete="off" />
            </Field>
            <Field label={tr("Access code")} htmlFor={`u-code-${i}`} hint={tr("Optional. Tell them separately; it is never emailed.")} error={problem(i, "access_code")}>
              <input id={`u-code-${i}`} className="input" value={p.code} onChange={(e) => set(i, { code: e.target.value })} autoComplete="off" />
            </Field>
          </div>
        </fieldset>
      ))}
      <div className="form-grid">
        <Field label={tr("Title")} htmlFor="u-title" required>
          <input id="u-title" className="input" value={title} onChange={(e) => setTitle(e.target.value)} />
        </Field>
        <Field label={tr("Last day to sign")} htmlFor="u-expires" hint={tr("Leave empty for the workspace's usual time.")}>
          <input id="u-expires" className="input" type="date" value={expires} onChange={(e) => setExpires(e.target.value)} />
        </Field>
        <Field label={tr("Message to the signers")} htmlFor="u-message" className="span-all">
          <textarea id="u-message" className="textarea" rows={3} maxLength={5000} value={message} onChange={(e) => setMessage(e.target.value)} />
        </Field>
      </div>
      <div className="row" style={{ justifyContent: "flex-end" }}>
        <Button onClick={onClose}>{tr("Cancel")}</Button>
        <Button type="submit" variant="primary" loading={busy} disabled={!ready}>
          {tr("Make the draft")}
        </Button>
      </div>
    </form>
  );
}

/** The templates as cards, each with what it can do. */
export function TemplateCards({ limit }: { limit?: number }) {
  const q = useQuery(templatesQuery);
  const meta = useEntityMeta("SignTemplate");
  const writable = can(meta.data?.permissions, "write");
  const [using, setUsing] = useState<string | null>(null);
  const list = (q.data ?? []).slice(0, limit);
  if (q.error) return <StatePanel kind="failed" error={q.error} onRetry={() => void q.refetch()} />;
  if (!q.data) return <ListSkeleton rows={3} />;
  if (!list.length)
    return (
      <StatePanel
        kind="empty"
        title={tr("No templates yet")}
        action={
          writable ? (
            <Link className="btn btn-md" to="/e/SignTemplate/new">
              <Plus size={15} aria-hidden="true" /> {tr("New template")}
            </Link>
          ) : undefined
        }
      >
        {tr("A template is a PDF you send again and again, with its signers' roles and boxes placed once, such as a standard agreement.")}
      </StatePanel>
    );
  return (
    <>
      <ul className="sg-cards">
        {list.map((t) => (
          <li key={t.id} className="sg-card">
            <span className="sg-card-icon" aria-hidden="true">
              <FileText size={18} />
            </span>
            <div className="sg-card-body">
              <strong>{isolate(t.template_name)}</strong>
              <span className="faint">
                {isolate([t.page_count ? `${t.page_count} ${t.page_count === 1 ? "page" : "pages"}` : null, t.signing_mode === "Parallel" ? tr("All sign at once") : tr("Signed in order"), t.document_name].filter(Boolean).join(" · "))}
              </span>
            </div>
            <div className="sg-card-actions">
              <Button variant="primary" size="sm" onClick={() => setUsing(t.id)}>
                {tr("Use")}
              </Button>
              {writable && (
                <Link className="btn btn-sm" to={`/template/${t.id}/fields`}>
                  {tr("Place boxes")}
                </Link>
              )}
              {writable && (
                <Link className="btn btn-sm btn-ghost" to={`/e/SignTemplate/${t.id}`}>
                  {tr("Details")}
                </Link>
              )}
            </div>
          </li>
        ))}
      </ul>
      {using && <UseTemplate id={using} onClose={() => setUsing(null)} />}
    </>
  );
}

export function Templates() {
  useShellTitle(tr("Templates"));
  const meta = useEntityMeta("SignTemplate");
  const writable = can(meta.data?.permissions, "write");
  return (
    <div className="sg-app">
      <header className="sg-head">
        <div>
          <Nav />
          <p className="page-sub">{tr("Documents you send again and again, with their signers and boxes ready.")}</p>
        </div>
        {writable && (
          <div className="sg-tools">
            <Link className="btn btn-md" to="/e/SignTemplate/new">
              <Plus size={15} aria-hidden="true" /> {tr("New template")}
            </Link>
          </div>
        )}
      </header>
      <TemplateCards />
    </div>
  );
}
