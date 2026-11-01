// Placing a template's boxes: the same editor as a draft's, for the roles the template holds. Saved
// against the version on screen; leaving with changes asks first.

import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useRef, useState } from "react";
import { Link, useParams } from "react-router";
import { api, ApiError, docPath, download, errorMessage } from "@ferrum/client";
import { Alert, Button, ErrorState, PageLoading, toast, useShellTitle, useUnsavedGuard } from "@ferrum/kit";
import { templateQuery } from "./data";
import { Editor, useBoxes } from "./Editor";
import { pageSizes, type Template } from "./model";
import { sameBoxes, toBoxes, toRows } from "./placement";

export function TemplateFieldsRoute() {
  const { id = "" } = useParams();
  const q = useQuery(templateQuery(id));
  useShellTitle(q.data ? `${q.data.template_name}: boxes` : "Template boxes");
  if (q.error) return <ErrorState error={q.error} onRetry={() => void q.refetch()} />;
  if (!q.data) return <PageLoading />;
  return <TemplateFields key={id} template={q.data} />;
}

function TemplateFields({ template }: { template: Template }) {
  const qc = useQueryClient();
  const base = useRef(template);
  const ed = useBoxes(toBoxes(template.fields ?? []));
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const dirty = !sameBoxes(ed.boxes, toBoxes(base.current.fields ?? []));
  const { dialog } = useUnsavedGuard(dirty);
  const roles = (template.roles ?? []).map((r) => ({ role: r.role, label: r.role }));

  const save = async () => {
    setSaving(true);
    setError(null);
    try {
      const saved = await api.patch<Template>(docPath("SignTemplate", template.id), { fields: toRows(ed.boxes) }, { ifMatch: base.current.version });
      const ids = new Map<string, string>();
      (saved.fields ?? []).forEach((f, i) => ed.boxes[i] && f.id && ids.set(ed.boxes[i].key, f.id));
      ed.adopt(ids);
      base.current = saved;
      qc.setQueryData(templateQuery(template.id).queryKey, saved);
      toast("Boxes saved");
    } catch (e) {
      setError(e instanceof ApiError && e.status === 409 ? "Someone else changed this template. Reload the page to see their version." : errorMessage(e));
    } finally {
      setSaving(false);
    }
  };

  return (
    <div className="sg-app sg-prepare">
      <div className="crumbs">
        <Link to="/templates">Templates</Link>
      </div>
      <header className="sg-prep-head">
        <div>
          <h1 className="page-title">{template.template_name}</h1>
          <p className="page-sub">
            Where each role signs, for every request made from this template ·{" "}
            <span aria-live="polite">{saving ? "Saving…" : dirty ? "Unsaved changes" : "Saved"}</span>
          </p>
        </div>
        <div className="page-actions">
          <Link className="btn btn-md" to={`/e/SignTemplate/${template.id}`}>
            Roles and details
          </Link>
          <Button variant="primary" loading={saving} disabled={!dirty} onClick={() => void save()}>
            Save boxes
          </Button>
        </div>
      </header>
      {error && <Alert tone="error">{error}</Alert>}
      <Editor ed={ed} roles={roles} pages={pageSizes(template)} onPdf={() => download(template.document ?? "", template.document_name ?? "template.pdf").catch((e) => toast(errorMessage(e), "error"))} />
      {dialog}
    </div>
  );
}
