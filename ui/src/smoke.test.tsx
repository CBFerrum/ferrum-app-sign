import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { renderToStaticMarkup } from "react-dom/server";
import { MemoryRouter } from "react-router";
import { describe, expect, it, vi } from "vitest";
import { Editor, useBoxes } from "./Editor";
import type { SignRequest } from "./model";
import { draftOf } from "./send";
import { Review, SignersStep } from "./Prepare";
import { RequestView } from "./RequestPage";
import { LinksDialog } from "./Links";
import { MarkPad, MarkView } from "./Pad";
import { Requests } from "./Requests";
import { AboutDialog, DeclineDialog, Ready } from "./SignPage";
import { TemplateCards } from "./Templates";
import type { SigningView } from "./signing";

vi.stubGlobal("window", { location: { pathname: "/sign/s/acme/TOKEN" }, matchMedia: () => ({ matches: false }), addEventListener() {}, removeEventListener() {} });

const view: SigningView = {
  request: {
    number: "SIGN-2026-00012",
    title: "Master Services Agreement",
    message: "Please sign by Friday.",
    status: "Sent",
    signing_mode: "Sequential",
    expires_at: "2099-11-14T17:00:00.000Z",
    document_name: "msa.pdf",
    document_sha256: "ab".repeat(32),
    pages: [
      { page: 1, width: 612, height: 792 },
      { page: 2, width: 612, height: 792 },
      { page: 3, width: 612, height: 792 },
    ],
    sender: { organization: "Acme Ltd", name: "Priya Shah", email: "priya@acme.test" },
  },
  signer: { role: "Client", name: "Jane Doe", email: "jane@client.test", status: "Viewed" },
  fields: [
    { id: "a", field_type: "Initials", page: 1, x: 500, y: 700, width: 56, height: 32, required: true },
    { id: "b", field_type: "Text", label: "Company name", page: 1, x: 72, y: 200, width: 200, height: 22, required: true },
    { id: "c", field_type: "Signature", page: 3, x: 72, y: 600, width: 160, height: 44, required: true },
    { id: "d", field_type: "Date Signed", page: 3, x: 300, y: 610, width: 96, height: 22, required: true },
    { id: "e", field_type: "Name", page: 3, x: 72, y: 560, width: 160, height: 22, required: true },
    { id: "f", field_type: "Checkbox", label: "I accept", page: 3, x: 72, y: 500, width: 18, height: 18, required: true },
  ],
  needs: { signature: true, initials: true },
  consent: "I agree to sign this document electronically.",
};

describe("the signer's page, drawn once", () => {
  it("shows the pages with something to do, the steps and the next button", () => {
    const html = renderToStaticMarkup(<Ready view={view} base="/api/site/acme/sign/signing/T" hasCode={false} call={() => Promise.reject(new Error("no"))} onChange={() => {}} onSigned={() => {}} onDeclined={() => {}} reopen={() => {}} />);
    expect(html).toContain("Master Services Agreement");
    expect(html).toContain("Your boxes are on pages 1 and 3.");
    expect(html).toContain("Page 1 of 3");
    expect(html).not.toContain("Page 2 of 3");
    expect(html).toContain("Fill in “Company name”");
    expect(html).toContain("0 of 4");
    expect(html).toContain("/api/site/acme/sign/signing/T/document");
    expect(html).toContain("not a certificate-based one");
  });
});

// The screens for people who are signed in draw their first frame without error and say what matters.
const request = (over: Partial<SignRequest> = {}): SignRequest => ({
  id: "r1",
  version: 4,
  number: "SIGN-2026-00012",
  title: "Master Services Agreement",
  status: "In Progress",
  signing_mode: "Sequential",
  document: "/files/abc",
  document_name: "msa.pdf",
  document_sha256: "cd".repeat(32),
  page_count: 2,
  page_sizes: [
    [612, 792],
    [612, 792],
  ],
  expires_at: "2099-01-01T00:00:00.000Z",
  sent_at: "2026-11-01T10:00:00.000Z",
  sender_email: "priya@acme.test",
  signers: [
    { id: "s1", role: "Client", name: "Jane Doe", email: "jane@client.test", signing_order: 1, status: "Signed", signed_at: "2026-11-01T11:00:00.000Z" },
    { id: "s2", role: "Company", name: "Ravi Kumar", email: "ravi@acme.test", signing_order: 2, status: "Viewed", viewed_at: "2026-11-01T12:00:00.000Z", reminders_sent: 2, last_reminded_at: "2026-11-01T13:00:00.000Z" },
    { id: "s3", role: "Witness", name: "Wen Li", email: "wen@acme.test", signing_order: 3, status: "Waiting" },
  ],
  fields: [
    { id: "f1", role: "Client", field_type: "Signature", page: 1, x: "72", y: "600", width: "160", height: "44", required: true },
    { id: "f2", role: "Company", field_type: "Signature", page: 2, x: "72", y: "600", width: "160", height: "44", required: true },
    { id: "f3", role: "Witness", field_type: "Signature", page: 2, x: "300", y: "600", width: "160", height: "44", required: true },
  ],
  ...over,
});

const inApp = (node: React.ReactNode) =>
  renderToStaticMarkup(
    <QueryClientProvider client={new QueryClient()}>
      <MemoryRouter>{node}</MemoryRouter>
    </QueryClientProvider>,
  );

function Boxes({ r }: { r: SignRequest }) {
  const d = draftOf(r);
  const ed = useBoxes(d.boxes);
  return <Editor ed={ed} roles={d.signers.map((s) => ({ role: s.role, label: s.name }))} pages={[{ width: 612, height: 792 }, { width: 612, height: 792 }]} onPdf={() => {}} />;
}

describe("the screens for senders", () => {
  it("draws the editor with a box for each signer in their colour, and says who lacks one", () => {
    const html = inApp(<Boxes r={request({ fields: request().fields.slice(0, 2) })} />);
    expect(html).toContain("Page 1 of 2");
    expect(html).toContain('aria-label="Signature for Client, page 1"');
    expect(html).toContain("sg-c0");
    expect(html).toContain("sg-c1");
    expect(html).toContain("has no signature box yet");
    expect(html).toContain("Add a box");
    expect(html).toContain("not pictures of them");
  });

  it("says there is nobody to place boxes for when no one signs yet", () => {
    const html = inApp(<Boxes r={request({ signers: [], fields: [] })} />);
    expect(html).toContain("There is nobody to place boxes for yet");
  });

  it("draws the signers' step and the review", () => {
    const draft = draftOf(request({ status: "Draft" }));
    const signers = inApp(<SignersStep draft={draft} apply={() => {}} rows={[]} problems={[]} />);
    expect(signers).toContain("One after another");
    expect(signers).toContain("Last day to sign");
    const review = inApp(<Review request={request({ status: "Draft" })} draft={draft} problems={[]} go={() => {}} />);
    expect(review).toContain("Ready to send");
    expect(review).toContain("Jane Doe");
    expect(review).toContain("1 signature");
    expect(review).toContain("What kind of signature this is");
    expect(review).toContain("does not add a PAdES, advanced or qualified");
    const blocked = inApp(<Review request={request({ status: "Draft" })} draft={{ ...draft, boxes: [] }} problems={[{ step: "fields", text: "Jane Doe has no signature box." }]} go={() => {}} />);
    expect(blocked).toContain("Before it can go");
    expect(blocked).toContain("Fix it");
  });
});

describe("the screen that follows a request", () => {
  it("says who is holding it up, with the people in their turn", () => {
    const html = inApp(<RequestView request={request()} />);
    expect(html).toContain("Waiting for Ravi Kumar");
    expect(html).toContain("Send a reminder");
    expect(html).toContain("Reminded 2 times");
    expect(html).toContain("Waiting for their turn");
    expect(html).toContain("Check integrity");
    expect(html).toContain("not a certificate-based one");
  });

  it("offers the signed copy and the fingerprints once everyone has signed", () => {
    const done = request({
      status: "Completed",
      completed_at: "2026-11-02T10:00:00.000Z",
      sealed_document: "/files/def",
      sealed_sha256: "ef".repeat(32),
      signers: request().signers.map((s) => ({ ...s, status: "Signed" as const })),
    });
    const html = inApp(<RequestView request={done} />);
    expect(html).toContain("Download the signed copy");
    expect(html).toContain("Signed copy");
    expect(html).not.toContain("Send a reminder");
  });

  it("says why a request that did not finish ended", () => {
    const declined = request({ status: "Declined", signers: request().signers.map((s, i) => (i === 1 ? { ...s, status: "Declined" as const, decline_reason: "Wrong amount" } : s)) });
    expect(inApp(<RequestView request={declined} />)).toContain("Wrong amount");
    expect(inApp(<RequestView request={request({ status: "Voided", void_reason: "Sent in error" })} />)).toContain("Sent in error");
    expect(inApp(<RequestView request={request({ status: "Expired" })} />)).toContain("The last day to sign passed");
  });
});

describe("what a signer is asked for", () => {
  it("offers drawing and typing, with the name ready to type", () => {
    const html = renderToStaticMarkup(<MarkPad kind="signature" typed="Jane Doe" onChange={() => {}} />);
    expect(html).toContain("Draw");
    expect(html).toContain("Type");
    expect(html).toContain('value="Jane Doe"');
    expect(html).toContain("Drawing area for your signature");
    expect(renderToStaticMarkup(<MarkPad kind="initials" typed="JD" onChange={() => {}} />)).toContain('value="JD"');
  });

  it("shows a mark as the image or the text it is", () => {
    expect(renderToStaticMarkup(<MarkView mark={{ image: "data:image/png;base64,AAAA" }} />)).toContain("<img");
    expect(renderToStaticMarkup(<MarkView mark={{ text: "Jane Doe" }} />)).toContain("Jane Doe");
  });

  it("asks why before declining, and says it ends the request for everyone", () => {
    const html = renderToStaticMarkup(<DeclineDialog open onClose={() => {}} onDecline={async () => null} />);
    expect(html).toContain("for everyone");
    expect(html).toContain("Why are you declining?");
  });

  it("says plainly what this signature is not", () => {
    const html = renderToStaticMarkup(<AboutDialog open onClose={() => {}} />);
    expect(html).toContain("electronic signature with an audit trail");
    expect(html).toContain("not</strong> a PAdES signature");
    expect(html).toContain("qualified");
  });
});

describe("the lists", () => {
  const client = () => {
    const c = new QueryClient();
    const rows = [
      { id: "r1", version: 1, title: "Master Services Agreement", number: "SIGN-2026-00012", status: "In Progress", document_name: "msa.pdf", expires_at: "2099-01-01T00:00:00.000Z", signing_mode: "Sequential", created_at: "2026-11-01T09:00:00.000Z" },
      { id: "r2", version: 1, title: "NDA", number: "SIGN-2026-00013", status: "Draft", document_name: "nda.pdf", signing_mode: "Sequential", created_at: "2026-11-01T10:00:00.000Z" },
      { id: "r3", version: 1, title: "Lease", number: "SIGN-2026-00011", status: "Completed", completed_at: "2026-10-30T10:00:00.000Z", sealed_document: "/files/x", signing_mode: "Sequential", created_at: "2026-10-29T10:00:00.000Z" },
    ];
    c.setQueryData(["sign", "requests"], { pages: [{ rows }], pageParams: [""] });
    c.setQueryData(["sign", "request", "r1"], request());
    return c;
  };
  const draw = (c: QueryClient, node: React.ReactNode, at = "/") =>
    renderToStaticMarkup(
      <QueryClientProvider client={c}>
        <MemoryRouter initialEntries={[at]}>{node}</MemoryRouter>
      </QueryClientProvider>,
    );

  it("opens on what is in progress, with who is holding each up", () => {
    const html = draw(client(), <Requests />);
    expect(html).toContain("Master Services Agreement");
    expect(html).toContain("Waiting for Ravi Kumar");
    expect(html).toContain('aria-label="1 of 3 signed"');
    expect(html).not.toContain(">NDA<");
    expect(html).toMatch(/In progress<span class="sg-count-chip">1<\/span>/);
    expect(html).toMatch(/Drafts<span class="sg-count-chip">1<\/span>/);
  });

  it("shows the drafts, and the finished ones, on their own tabs", () => {
    expect(draw(client(), <Requests />, "/?tab=drafts")).toContain(">NDA<");
    expect(draw(client(), <Requests />, "/?tab=completed")).toContain(">Lease<");
    expect(draw(client(), <Requests />, "/?tab=closed")).toContain("Nothing was declined");
  });

  it("keeps the search in the address", () => {
    expect(draw(client(), <Requests />, "/?tab=drafts&q=nda")).toContain(">NDA<");
    expect(draw(client(), <Requests />, "/?tab=drafts&q=zzz")).toContain("Nothing matches");
    expect(draw(client(), <Requests />, "/?q=sign-2026-00012")).toContain("Master Services Agreement");
  });

  it("starts a first-time workspace off", () => {
    const empty = new QueryClient();
    empty.setQueryData(["sign", "requests"], { pages: [{ rows: [] }], pageParams: [""] });
    const html = draw(empty, <Requests />);
    expect(html).toContain("Send your first document for signature");
    expect(html).toContain("New request");
  });

  it("lists templates with what to do with each", () => {
    const c = new QueryClient();
    c.setQueryData(["sign", "templates"], [{ id: "t1", template_name: "Service Agreement", page_count: 3, signing_mode: "Sequential", document_name: "service.pdf" }]);
    const html = draw(c, <TemplateCards />);
    expect(html).toContain("Service Agreement");
    expect(html).toContain("3 pages · Signed in order · service.pdf");
    expect(html).toContain(">Use<");
  });

  it("gives out links to pass on, and says nothing was emailed", () => {
    const html = renderToStaticMarkup(
      <LinksDialog open title="Request sent" links={[{ role: "Client", name: "Jane Doe", email: "jane@client.test", emailed: false, url: "/api/site/acme/sign/s/TOKEN" }, { role: "Company", name: "Ravi", email: "ravi@acme.test", emailed: true }]} onClose={() => {}} />,
    );
    expect(html).toContain("nothing was emailed");
    expect(html).toContain("Emailed");
    expect(html).toContain("/api/site/acme/sign/s/TOKEN");
  });
});
