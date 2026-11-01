// The queries and calls the screens share. Keys are listed here so a change anywhere refreshes
// everywhere a request is shown (live events do the same from the other side).

import type { QueryClient } from "@tanstack/react-query";
import { api, docPath } from "@ferrum/client";
import type { LinkRow, SignEvent, SignRequest, Template } from "./model";

export const requestQuery = (id: string) => ({
  queryKey: ["sign", "request", id],
  queryFn: ({ signal }: { signal?: AbortSignal }) => api.get<SignRequest>(docPath("SignRequest", id), undefined, signal),
});

export const eventsQuery = (id: string) => ({
  queryKey: ["sign", "events", id],
  queryFn: ({ signal }: { signal?: AbortSignal }) =>
    api
      .get<{ rows: SignEvent[] }>("/api/v1/SignEvent", { filters: JSON.stringify({ request: id }), sort: "seq asc", limit: 500, fields: "seq,event,occurred_at,role,signer_name,signer_email,actor,details,event_hash" }, signal)
      .then((r) => r.rows),
});

export const templatesQuery = {
  queryKey: ["sign", "templates"],
  queryFn: ({ signal }: { signal?: AbortSignal }) =>
    api.get<{ rows: Template[] }>("/api/v1/SignTemplate", { filters: JSON.stringify({ disabled: false }), sort: "template_name asc", limit: 200, fields: "template_name,title,page_count,signing_mode,document_name,version" }, signal).then((r) => r.rows),
};

export const templateQuery = (id: string) => ({
  queryKey: ["sign", "template", id],
  queryFn: ({ signal }: { signal?: AbortSignal }) => api.get<Template>(docPath("SignTemplate", id), undefined, signal),
});

/** Runs one of a request's actions (`send`, `remind`, `resend`, `void`, `verify`). */
export const act = <T>(id: string, action: string, params: object = {}) => api.post<T>(docPath("SignRequest", id, "actions", action), params);

export const sendRequest = (id: string) => act<{ status: string; expires_at?: string; signers: LinkRow[] }>(id, "send");

/** Everything about requests shown anywhere is read again. */
export const refresh = (qc: QueryClient, id?: string) => {
  void qc.invalidateQueries({ queryKey: ["sign", "requests"] });
  if (id) {
    void qc.invalidateQueries({ queryKey: ["sign", "request", id] });
    void qc.invalidateQueries({ queryKey: ["sign", "events", id] });
  }
};
