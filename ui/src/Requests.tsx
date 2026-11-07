// Sign's home: every request by where it stands, who is holding each one up, and the few things
// worth doing without opening it (a reminder, the signed copy). It stays current as signers act.

import { useInfiniteQuery, useQueries, useQueryClient } from "@tanstack/react-query";
import { useCallback, useMemo, useState } from "react";
import { Link, useNavigate, useSearchParams } from "react-router";
import { Ellipsis, Plus } from "lucide-react";
import { api, docPath, download, errorMessage, relativeTime, t as tr } from "@ferrum/client";
import { Badge, Button, confirmDialog, EmptyState, ErrorState, Spinner, toast, useLiveEntity, useShellTitle } from "@ferrum/kit";
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuSeparator, DropdownMenuTrigger, Skeleton } from "@ferrum/kit/primitives";
import { act, refresh, requestQuery } from "./data";
import { LinksDialog } from "./Links";
import type { LinkRow, RequestRow, Signer } from "./model";
import { Nav } from "./Nav";
import { isOpen, standing, stepState, TABS, type TabId, tabOf, toneOf, turnOf } from "./track";
import { moment, timeLeft } from "./time";

const FIELDS = "number,title,status,sent_at,expires_at,signing_mode,sealed_document,created_at,version,document_name,completed_at";
/** Progress is read for the first few in-progress requests only: each is a record of its own. */
const DETAILED = 30;

const matches = (r: RequestRow, q: string) => {
  const s = q.trim().toLowerCase();
  return !s || [r.title, r.number, r.document_name].some((v) => v?.toLowerCase().includes(s));
};

/** One segment for each signer: signed, up now, declined, or waiting their turn. */
export function Segments({ signers, mode }: { signers: Signer[]; mode: RequestRow["signing_mode"] }) {
  const turn = turnOf({ signing_mode: mode, signers });
  const signed = signers.filter((s) => s.status === "Signed").length;
  return (
    <span className="sg-segments" role="img" aria-label={`${signed} of ${signers.length} signed`}>
      {signers.map((s, i) => (
        <span key={s.id ?? i} data-state={stepState(s, turn)} />
      ))}
    </span>
  );
}

export function Requests() {
  useShellTitle("Signatures");
  const qc = useQueryClient();
  const navigate = useNavigate();
  const [sp, setSp] = useSearchParams();
  const tab: TabId = TABS.some((t) => t.id === sp.get("tab")) ? (sp.get("tab") as TabId) : "progress";
  // The list and what was searched for live in the address, so a link shows the same thing.
  const search = sp.get("q") ?? "";
  const show = (change: Record<string, string | null>) => {
    const next = new URLSearchParams(sp);
    for (const [k, v] of Object.entries(change)) v ? next.set(k, v) : next.delete(k);
    setSp(next, { replace: true });
  };
  const [links, setLinks] = useState<LinkRow[] | null>(null);

  const q = useInfiniteQuery({
    queryKey: ["sign", "requests"],
    initialPageParam: "",
    queryFn: ({ pageParam, signal }) =>
      api.get<{ rows: RequestRow[]; next_cursor?: string | null }>("/api/v1/SignRequest", { fields: FIELDS, sort: "created_at desc", limit: 100, cursor: pageParam || undefined }, signal),
    getNextPageParam: (last) => last.next_cursor || undefined,
  });
  useLiveEntity("SignRequest", useCallback(() => refresh(qc), [qc]));

  const rows = useMemo(() => q.data?.pages.flatMap((p) => p.rows) ?? [], [q.data]);
  const count = (t: TabId) => rows.filter((r) => tabOf(r.status) === t).length;
  const shown = useMemo(() => rows.filter((r) => tabOf(r.status) === tab && matches(r, search)), [rows, tab, search]);
  const details = useQueries({ queries: shown.filter((r) => isOpen(r.status)).slice(0, DETAILED).map((r) => requestQuery(r.id)) });
  const detail = new Map(details.flatMap((d) => (d.data ? [[d.data.id, d.data] as const] : [])));

  const remind = async (r: RequestRow) => {
    try {
      const out = await act<{ reminded: LinkRow[] }>(r.id, "remind");
      refresh(qc, r.id);
      if (out.reminded.some((l) => !l.emailed)) setLinks(out.reminded);
      else toast(`Reminder sent to ${out.reminded.map((l) => l.name || l.email).join(", ")}`);
    } catch (e) {
      toast(errorMessage(e), "error");
    }
  };
  const discard = async (r: RequestRow) => {
    if (!(await confirmDialog({ title: `Delete “${r.title}”?`, message: "The draft goes. Nothing was sent.", confirmLabel: "Delete draft", danger: true }))) return;
    try {
      await api.del(docPath("SignRequest", r.id), { ifMatch: r.version });
      refresh(qc);
      toast(tr("Draft deleted"));
    } catch (e) {
      toast(errorMessage(e), "error");
    }
  };
  const copy = async (r: RequestRow) => {
    try {
      await download(r.sealed_document ?? "", `${r.title} - signed.pdf`);
    } catch (e) {
      toast(errorMessage(e), "error");
    }
  };

  const tabInfo = TABS.find((t) => t.id === tab)!;
  const body = q.error ? (
    <ErrorState error={q.error} onRetry={() => void q.refetch()} />
  ) : !q.data ? (
    <Spinner label={tr("Loading your requests")} />
  ) : !rows.length ? (
    <EmptyState title={tr("Send your first document for signature")} icon="file">
      <p className="muted" style={{ maxWidth: "34rem" }}>
        {tr("Upload a PDF, say who signs and where, and each person gets a link to sign on any device, with no account. You see who has signed, and the finished document keeps an audit trail.")}
      </p>
      <Link className="btn btn-primary btn-md" to="/new">
        <Plus size={15} aria-hidden="true" /> {tr("New request")}
      </Link>
    </EmptyState>
  ) : !shown.length ? (
    <EmptyState title={search ? "Nothing matches" : tabInfo.empty} icon={search ? "search" : "check"}>
      {search ? (
        <button type="button" className="link-btn" onClick={() => show({ q: null })}>
          {tr("Clear the search")}
        </button>
      ) : (
        (tab === "progress" || tab === "drafts") && (
          <Link to="/new">{tr("Start a request")}</Link>
        )
      )}
    </EmptyState>
  ) : (
    <ul className="sg-list" aria-label={tabInfo.label}>
      {shown.map((r) => {
        const d = detail.get(r.id);
        const s = d ? standing(d) : null;
        const left = isOpen(r.status) ? timeLeft(r.expires_at) : null;
        return (
          <li key={r.id} className="sg-row">
            <Link className="sg-row-main" to={r.status === "Draft" ? `/request/${r.id}/prepare` : `/request/${r.id}`}>
              <span className="sg-row-title">{r.title}</span>
              <span className="sg-row-meta">{[r.number, r.document_name].filter(Boolean).join(" · ")}</span>
            </Link>
            <div className="sg-row-state">
              <Badge tone={toneOf(r.status)}>{r.status}</Badge>
              {isOpen(r.status) &&
                (d && s ? (
                  <>
                    <span>{s.text}</span>
                    <Segments signers={d.signers} mode={d.signing_mode} />
                  </>
                ) : (
                  <Skeleton className="h-4 w-40" />
                ))}
            </div>
            <div className="sg-row-when">
              {left ? (
                <Badge tone={left.level === "late" ? "red" : left.level === "soon" ? "amber" : "neutral"}>{left.text}</Badge>
              ) : r.completed_at ? (
                <span title={moment(r.completed_at)}>{tr("Completed")} {relativeTime(r.completed_at)}</span>
              ) : r.sent_at ? (
                <span title={moment(r.sent_at)}>{tr("Sent")} {relativeTime(r.sent_at)}</span>
              ) : r.created_at ? (
                <span title={moment(r.created_at)}>{tr("Started")} {relativeTime(r.created_at)}</span>
              ) : null}
            </div>
            <DropdownMenu>
              <DropdownMenuTrigger asChild>
                <button type="button" className="icon-btn" aria-label={`Actions for ${r.title}`}>
                  <Ellipsis size={16} aria-hidden="true" />
                </button>
              </DropdownMenuTrigger>
              <DropdownMenuContent align="end">
                <DropdownMenuItem onSelect={() => navigate(r.status === "Draft" ? `/request/${r.id}/prepare` : `/request/${r.id}`)}>{r.status === "Draft" ? "Continue editing" : "Open"}</DropdownMenuItem>
                {isOpen(r.status) && <DropdownMenuItem onSelect={() => void remind(r)}>{tr("Send a reminder")}</DropdownMenuItem>}
                {r.status === "Completed" && r.sealed_document && <DropdownMenuItem onSelect={() => void copy(r)}>{tr("Download the signed copy")}</DropdownMenuItem>}
                {r.status === "Draft" && (
                  <>
                    <DropdownMenuSeparator />
                    <DropdownMenuItem onSelect={() => void discard(r)}>{tr("Delete draft…")}</DropdownMenuItem>
                  </>
                )}
              </DropdownMenuContent>
            </DropdownMenu>
          </li>
        );
      })}
    </ul>
  );

  return (
    <div className="sg-app">
      <header className="sg-head">
        <div>
          <Nav />
          <p className="page-sub" aria-live="polite">
            {q.data ? `${rows.length} ${rows.length === 1 ? "request" : "requests"}${q.hasNextPage ? " or more" : ""}` : " "}
          </p>
        </div>
        <div className="sg-tools">
          <input className="input sg-search" type="search" placeholder={tr("Find a request…")} aria-label={tr("Find a request")} value={search} onChange={(e) => show({ q: e.target.value || null })} />
          <Link className="btn btn-primary btn-md" to="/new">
            <Plus size={15} aria-hidden="true" /> {tr("New request")}
          </Link>
        </div>
      </header>
      <div className="sg-switch" role="tablist" aria-label={tr("Requests by where they stand")}>
        {TABS.map((t) => (
          <button key={t.id} type="button" role="tab" aria-selected={tab === t.id} onClick={() => show({ tab: t.id === "progress" ? null : t.id })}>
            {t.label}
            {q.data && <span className="sg-count-chip">{count(t.id)}</span>}
          </button>
        ))}
      </div>
      {body}
      {q.hasNextPage && (
        <Button loading={q.isFetchingNextPage} onClick={() => void q.fetchNextPage()}>
          {tr("Show older requests")}
        </Button>
      )}
      <LinksDialog open={links !== null} title={tr("Reminder links")} links={links ?? []} onClose={() => setLinks(null)} />
    </div>
  );
}
