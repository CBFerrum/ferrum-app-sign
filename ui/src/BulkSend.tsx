// Bulk send: one template sent to many people, a request each. Starting here makes the bulk send as a draft (the rows are the
// pasted list); it is checked and started on its own page, which shows each row as it is sent.

import { useQuery } from "@tanstack/react-query";
import { useState } from "react";
import { Link, useNavigate } from "react-router";
import { api, docPath, errorMessage, t, isolate } from "@ferrum/client";
import { Alert, Button, Field, ListSkeleton, StatePanel, useShellTitle } from "@ferrum/kit";
import { columnsOf, parseRecipients } from "./bulk";
import { templateQuery, templatesQuery } from "./data";
import { Nav } from "./Nav";

export function BulkSend() {
  useShellTitle(t("Bulk send"));
  const navigate = useNavigate();
  const templates = useQuery(templatesQuery);
  const [template, setTemplate] = useState("");
  const chosen = useQuery({ ...templateQuery(template), enabled: !!template });
  const roles = (chosen.data?.roles ?? []).map((r) => r.role);
  const [title, setTitle] = useState("");
  const [list, setList] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const parsed = parseRecipients(list, roles);

  const make = async () => {
    if (parsed.problems.length) return setError(parsed.problems.join(" "));
    setBusy(true);
    setError(null);
    try {
      const made = await api.post<{ id: string }>(docPath("SignBulkSend"), { title: title.trim(), template, recipients: parsed.rows });
      navigate(`/e/SignBulkSend/${made.id}`);
    } catch (e) {
      setError(errorMessage(e));
      setBusy(false);
    }
  };

  return (
    <div className="sg-app sg-new">
      <Nav />
      <h1 className="page-title">{t("Bulk send")}</h1>
      <p className="page-sub">{t("Send one template to many people at once. Each gets a request of their own.")}</p>
      {templates.error ? (
        <StatePanel kind="failed" error={templates.error} onRetry={() => void templates.refetch()} />
      ) : !templates.data ? (
        <ListSkeleton rows={2} />
      ) : !templates.data.length ? (
        <StatePanel kind="empty" title={t("No templates yet")} action={<Link to="/templates">{t("Templates")}</Link>}>
          {t("A bulk send starts from a template.")}
        </StatePanel>
      ) : (
        <div className="stack">
          <Field label={t("Template")} htmlFor="bulk-template" required>
            <select id="bulk-template" className="select" value={template} onChange={(e) => { setTemplate(e.target.value); setTitle(templates.data?.find((x) => x.id === e.target.value)?.title ?? ""); }}>
              <option value="">{t("Choose a template")}</option>
              {templates.data.map((x) => (
                <option key={x.id} value={x.id}>
                  {isolate(x.template_name)}
                </option>
              ))}
            </select>
          </Field>
          {template && (
            <>
              <Field label={t("Title")} htmlFor="bulk-title" required hint={t("Every request gets it.")}>
                <input id="bulk-title" className="input" value={title} maxLength={200} onChange={(e) => setTitle(e.target.value)} />
              </Field>
              <Field label={t("Recipients")} htmlFor="bulk-list" required hint={t("One line each: {columns}. Paste rows from a spreadsheet.", { columns: columnsOf(roles) })}>
                <textarea id="bulk-list" className="input" rows={10} value={list} onChange={(e) => setList(e.target.value)} />
              </Field>
              {list.trim() && parsed.problems.length > 0 && <Alert tone="warning">{isolate(parsed.problems.slice(0, 5).join(" "))}</Alert>}
              {error && <Alert tone="error">{isolate(error)}</Alert>}
              <div className="row">
                <Button variant="primary" loading={busy} disabled={!title.trim() || !roles.length || parsed.problems.length > 0} onClick={() => void make()}>
                  {t("Make the bulk send")}
                </Button>
                <span className="faint">{t("Nothing is sent until you start it.")}</span>
              </div>
            </>
          )}
        </div>
      )}
    </div>
  );
}
