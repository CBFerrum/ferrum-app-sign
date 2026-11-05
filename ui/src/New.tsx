// Starting a request: upload the PDF to be signed, or begin from a template. Uploading makes the
// draft at once (the server reads the PDF and says if it cannot), so it can be left and come back to.

import { type DragEvent, useState } from "react";
import { Link, useNavigate } from "react-router";
import { FileUp } from "lucide-react";
import { api, errorMessage, uploadFile } from "@ferrum/client";
import { Alert, Button, Field, useShellTitle } from "@ferrum/kit";
import { MAX_MB, refusal, sizeText, titleOf } from "./send";
import { TemplateCards } from "./Templates";

export function NewRequest() {
  useShellTitle("New request");
  const navigate = useNavigate();
  const [file, setFile] = useState<File | null>(null);
  const [title, setTitle] = useState("");
  const [over, setOver] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const take = (f: File | undefined) => {
    if (!f) return;
    const why = refusal(f);
    setError(why);
    if (why) return;
    setFile(f);
    setTitle(titleOf(f.name));
  };
  const drop = (e: DragEvent) => {
    e.preventDefault();
    setOver(false);
    take(e.dataTransfer.files[0]);
  };

  const start = async () => {
    if (!file) return;
    setBusy(true);
    setError(null);
    try {
      const stored = await uploadFile(file);
      const made = await api.post<{ id: string }>("/api/v1/SignRequest", { title: title.trim(), document: stored.url });
      navigate(`/request/${made.id}/prepare`);
    } catch (e) {
      setError(errorMessage(e));
      setBusy(false);
    }
  };

  return (
    <div className="sg-app sg-new">
      <div className="crumbs">
        <Link to="/">Signatures</Link>
      </div>
      <h1 className="page-title">New request</h1>
      <p className="page-sub">Start with the document to be signed.</p>

      <section className="sg-start" aria-labelledby="upload-head">
        <h2 id="upload-head" className="sg-h2">
          Upload a PDF
        </h2>
        <label className="sg-drop" data-over={over || undefined} onDragOver={(e) => { e.preventDefault(); setOver(true); }} onDragLeave={() => setOver(false)} onDrop={drop}>
          <input type="file" accept="application/pdf,.pdf" className="sr-only" onChange={(e) => take(e.target.files?.[0])} />
          <FileUp size={22} aria-hidden="true" />
          {file ? (
            <span>
              <strong>{file.name}</strong>
              <span className="faint"> · {sizeText(file.size)} · choose another</span>
            </span>
          ) : (
            <span>
              <strong>Choose a PDF</strong> or drop it here
              <span className="faint"> · up to {MAX_MB} MB, not password-protected</span>
            </span>
          )}
        </label>
        {error && <Alert tone="error">{error}</Alert>}
        {file && (
          <div className="stack">
            <Field label="Title" htmlFor="new-title" hint="Signers see this in their email and on their page.">
              <input id="new-title" className="input" value={title} maxLength={200} onChange={(e) => setTitle(e.target.value)} />
            </Field>
            <div className="row">
              <Button variant="primary" loading={busy} disabled={!title.trim()} onClick={() => void start()}>
                Continue to the signers
              </Button>
              <span className="faint">Nothing is sent yet.</span>
            </div>
          </div>
        )}
      </section>

      <section className="sg-start" aria-labelledby="tpl-head">
        <div className="row">
          <h2 id="tpl-head" className="sg-h2">
            Or start from a template
          </h2>
          <span className="spacer" />
          <Link to="/templates">All templates</Link>
        </div>
        <TemplateCards limit={6} />
      </section>
    </div>
  );
}
