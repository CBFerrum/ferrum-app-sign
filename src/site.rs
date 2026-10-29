//! Public endpoints at `/api/site/{tenant}/sign/...` — signers have no account; their link's
//! token authenticates them:
//!
//! * `GET /s/{token}` — the signing page (self-contained HTML; it calls the endpoints below)
//! * `GET /signing/{token}` — the request with this signer's fields only
//! * `GET /signing/{token}/document` — the original PDF
//! * `POST /signing/{token}/sign` — `{consent, signature, initials?, values?}`
//! * `POST /signing/{token}/decline` — `{reason}`
//! * `POST /verify` — a PDF as the body: is it a document sealed here, unchanged?
//!
//! The access code, when the signer has one, goes in the `X-Access-Code` header. Signed-in users
//! have no endpoints here: they work through the records API and the requests' actions.

use crate::signing::{self, Client, Gate, Reply};
use crate::{clean, pdf, token};
use async_trait::async_trait;
use base64::Engine as _;
use ferrum_kernel::{Error, Kernel, Result, Site, SiteRequest, SiteResponse};

pub struct SignSite;

fn header<'a>(req: &'a SiteRequest, name: &str) -> Option<&'a str> {
    req.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).and_then(|(_, v)| v.as_str()).map(str::trim).filter(|v| !v.is_empty())
}

#[async_trait]
impl Site for SignSite {
    /// `POST /verify` takes a whole PDF; the signing endpoints check their own, smaller limit.
    fn max_body(&self) -> usize {
        pdf::MAX_BYTES + 64 * 1024
    }

    async fn handle(&self, k: &Kernel, req: SiteRequest) -> Result<SiteResponse> {
        if req.user.is_some() {
            return Err(Error::NotFound("endpoint".into()));
        }
        let path: Vec<&str> = req.path.split('/').filter(|p| !p.is_empty()).collect();
        let client = Client {
            ip: req.client_ip.clone(),
            user_agent: header(&req, "user-agent").map(|u| clean(u, 500)),
            access_code: header(&req, "x-access-code").map(|c| clean(c, 64)),
        };
        let tenant = req.tenant;
        let mut resp = match (req.method.as_str(), path.as_slice()) {
            ("GET", ["s", t]) => page(t)?,
            ("GET", ["signing", t]) => signing::view(k, tenant, t, &client).await?.into_response(),
            ("GET", ["signing", t, "document"]) => match signing::document(k, tenant, t, &client).await? {
                Gate::Open((name, bytes)) => pdf_response(&name, bytes),
                Gate::Gone(m, state) => Reply::Gone(m, state).into_response(),
            },
            ("POST", ["signing", t, "sign"]) => signing::sign(k, tenant, t, &client, &req.body).await?.into_response(),
            ("POST", ["signing", t, "decline"]) => signing::decline(k, tenant, t, &client, &req.body).await?.into_response(),
            ("POST", ["verify"]) => SiteResponse::json(200, &signing::verify_presented(k, tenant, &req.body).await?),
            _ => return Err(Error::NotFound("endpoint".into())),
        };
        for (k, v) in [("cache-control", "no-store"), ("referrer-policy", "no-referrer"), ("x-content-type-options", "nosniff"), ("x-robots-tag", "noindex")] {
            resp.headers.push((k.into(), v.into()));
        }
        Ok(resp)
    }
}

fn pdf_response(name: &str, bytes: Vec<u8>) -> SiteResponse {
    let ascii: String = name.chars().map(|c| if c.is_ascii_graphic() && c != '"' && c != '\\' || c == ' ' { c } else { '_' }).collect();
    SiteResponse {
        status: 200,
        content_type: "application/pdf".into(),
        headers: vec![("content-disposition".into(), format!("inline; filename=\"{ascii}\""))],
        body: bytes,
    }
}

/// The signing page. Any well-formed token gets the same static page (its script asks the API);
/// it loads nothing from elsewhere and runs only its own script.
fn page(token_str: &str) -> Result<SiteResponse> {
    if token::parse(token_str).is_none() {
        return Err(Error::NotFound("signing link".into()));
    }
    let nonce = base64::engine::general_purpose::STANDARD.encode(rand::random::<[u8; 16]>());
    let mut resp = SiteResponse::text(200, "text/html; charset=utf-8", PAGE.replace("__NONCE__", &nonce));
    resp.headers.push((
        "content-security-policy".into(),
        format!(
            "default-src 'none'; script-src 'nonce-{nonce}'; style-src 'unsafe-inline'; img-src data: blob:; connect-src 'self'; \
             frame-src blob:; object-src blob:; base-uri 'none'; form-action 'none'; frame-ancestors 'none'"
        ),
    ));
    resp.headers.push(("x-frame-options".into(), "DENY".into()));
    Ok(resp)
}

const PAGE: &str = r#"<!doctype html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<meta name="robots" content="noindex"><title>Sign a document</title>
<style>
:root{--fg:#1f2328;--muted:#57606a;--line:#d0d7de;--bg:#f6f8fa;--card:#fff;--accent:#1f6feb;--danger:#cf222e}
@media (prefers-color-scheme:dark){:root{--fg:#e6edf3;--muted:#8d96a0;--line:#30363d;--bg:#0d1117;--card:#161b22;--accent:#4493f8;--danger:#f85149}}
*{box-sizing:border-box}body{margin:0;font:15px/1.5 system-ui,-apple-system,"Segoe UI",sans-serif;color:var(--fg);background:var(--bg)}
main{max-width:860px;margin:0 auto;padding:24px 16px 64px}
.card{background:var(--card);border:1px solid var(--line);border-radius:8px;padding:20px;margin-bottom:16px}
h1{font-size:20px;margin:0 0 4px}h2{font-size:16px;margin:0 0 12px}.muted{color:var(--muted);font-size:13px;overflow-wrap:anywhere}
iframe{width:100%;height:70vh;border:1px solid var(--line);border-radius:6px;background:#fff}
label{display:block;margin:14px 0 4px;font-weight:600}
input[type=text],textarea{width:100%;padding:8px;border:1px solid var(--line);border-radius:6px;font:inherit;background:var(--card);color:var(--fg)}
canvas{display:block;width:100%;height:180px;border:1px dashed var(--line);border-radius:6px;background:#fff;touch-action:none}
button{font:inherit;padding:9px 18px;border-radius:6px;border:1px solid var(--line);background:var(--card);color:var(--fg);cursor:pointer}
button:disabled{opacity:.6;cursor:wait}button.primary{background:var(--accent);border-color:var(--accent);color:#fff}button.danger{color:var(--danger)}
.row{display:flex;gap:8px;flex-wrap:wrap;align-items:center;margin-top:12px}.error{color:var(--danger)}
.tabs button[aria-pressed=true]{border-color:var(--accent);color:var(--accent)}
label.check{display:flex;gap:8px;align-items:flex-start;font-weight:400}ul{padding-left:20px;margin:0}
</style></head>
<body><main id="app"><div class="card"><p>Loading…</p></div></main>
<script nonce="__NONCE__">
(() => {
  const token = location.pathname.split('/').pop();
  const api = location.pathname.replace(/\/s\/[^/]+$/, '/signing/' + token);
  const app = document.getElementById('app');
  let code = '';
  const h = (tag, attrs, ...kids) => {
    const e = document.createElement(tag);
    for (const [k, v] of Object.entries(attrs || {})) {
      if (k === 'text') e.textContent = v; else if (k.startsWith('on')) e[k] = v; else e.setAttribute(k, v);
    }
    for (const c of kids) if (c !== null && c !== undefined) e.append(c);
    return e;
  };
  const card = (...kids) => h('div', {class: 'card'}, ...kids);
  const show = (...cards) => app.replaceChildren(...cards);
  const say = (title, text) => show(card(h('h1', {text: title}), h('p', {text: text || ''})));
  const call = (path, opts) => {
    const o = Object.assign({cache: 'no-store'}, opts || {});
    o.headers = Object.assign({}, o.headers || {});
    if (code) o.headers['X-Access-Code'] = code;
    return fetch(api + path, o);
  };
  const failure = async (r) => {
    let m = 'Something went wrong (' + r.status + ').';
    try { const j = await r.json(); m = j.message || m; if (j.fields) m = j.fields.map(f => f.message).join(' '); } catch (_) {}
    return m;
  };
  const initialsOf = (n) => (n || '').split(/\s+/).filter(Boolean).map(w => w[0].toUpperCase()).slice(0, 4).join('');

  async function start() {
    const r = await call('');
    if (r.status === 401) return askCode('');
    if (r.status === 403 && code) return askCode(await failure(r));
    if (r.status === 404) return say('This link is not valid', 'Use the most recent link the sender gave you.');
    if (r.status === 410) return say('Nothing to sign', (await r.json()).message);
    if (!r.ok) return say('Not available', await failure(r));
    render(await r.json());
  }

  function askCode(error) {
    const input = h('input', {type: 'text', autocomplete: 'one-time-code', 'aria-label': 'Access code'});
    const go = h('button', {class: 'primary', text: 'Continue', onclick: () => { code = input.value.trim(); start(); }});
    show(card(h('h1', {text: 'Access code'}), h('p', {text: 'Enter the access code the sender gave you.'}), input,
      error ? h('p', {class: 'error', text: error}) : null, h('div', {class: 'row'}, go)));
    input.focus();
  }

  function pad(name) {
    const canvas = h('canvas', {'aria-label': 'Draw your signature here'});
    const typed = h('input', {type: 'text', maxlength: '100', value: name || '', 'aria-label': 'Typed signature'});
    const g = canvas.getContext('2d');
    let mode = 'draw', drawn = false, last = null;
    const size = () => {
      const r = canvas.getBoundingClientRect(), dpr = Math.min(window.devicePixelRatio || 1, 3);
      canvas.width = Math.max(1, Math.round(r.width * dpr)); canvas.height = Math.max(1, Math.round(r.height * dpr));
      g.setTransform(dpr, 0, 0, dpr, 0, 0); g.lineWidth = 2.2; g.lineCap = 'round'; g.lineJoin = 'round'; g.strokeStyle = '#111';
      drawn = false;
    };
    const at = (e) => { const r = canvas.getBoundingClientRect(); return [e.clientX - r.left, e.clientY - r.top]; };
    canvas.addEventListener('pointerdown', (e) => { canvas.setPointerCapture(e.pointerId); last = at(e); });
    canvas.addEventListener('pointermove', (e) => {
      if (!last) return;
      const p = at(e); g.beginPath(); g.moveTo(last[0], last[1]); g.lineTo(p[0], p[1]); g.stroke(); last = p; drawn = true;
    });
    for (const ev of ['pointerup', 'pointercancel']) canvas.addEventListener(ev, () => { last = null; });
    const drawBox = h('div', {}, canvas, h('div', {class: 'row'}, h('button', {text: 'Clear', onclick: size})));
    const typeBox = h('div', {hidden: ''}, typed);
    const drawBtn = h('button', {text: 'Draw', 'aria-pressed': 'true'}), typeBtn = h('button', {text: 'Type', 'aria-pressed': 'false'});
    const pick = (m) => {
      mode = m; drawBtn.setAttribute('aria-pressed', String(m === 'draw')); typeBtn.setAttribute('aria-pressed', String(m === 'type'));
      drawBox.hidden = m !== 'draw'; typeBox.hidden = m !== 'type';
    };
    drawBtn.onclick = () => pick('draw'); typeBtn.onclick = () => pick('type');
    requestAnimationFrame(size);
    window.addEventListener('resize', () => { if (!drawn) size(); });
    return {
      el: h('div', {}, h('label', {text: 'Your signature'}), h('div', {class: 'row tabs'}, drawBtn, typeBtn), drawBox, typeBox),
      value: () => mode === 'draw' ? (drawn ? {image: canvas.toDataURL('image/png')} : null) : (typed.value.trim() ? {text: typed.value.trim()} : null),
    };
  }

  function render(d) {
    const req = d.request;
    const from = [req.sender.name, req.sender.organization].filter(Boolean).join(', ');
    const frame = h('iframe', {title: 'The document to sign'});
    const download = h('a', {text: 'Download the PDF'});
    call('/document').then(async (r) => {
      if (!r.ok) { frame.replaceWith(h('p', {class: 'error', text: await failure(r)})); return; }
      const url = URL.createObjectURL(await r.blob());
      frame.src = url; download.href = url; download.download = req.document_name || 'document.pdf';
    });
    const list = h('ul');
    for (const f of d.fields) list.append(h('li', {text: 'Page ' + f.page + ': ' + (f.label || f.field_type) + (f.required ? '' : ' (optional)')}));
    const inputs = {}, form = h('div');
    for (const f of d.fields) {
      const what = (f.label || f.field_type) + ' (page ' + f.page + (f.required ? ')' : ', optional)');
      if (f.field_type === 'Text') {
        const i = h('input', {type: 'text', maxlength: '500'}); inputs[f.id] = () => i.value.trim() || null;
        form.append(h('label', {text: what}), i);
      } else if (f.field_type === 'Checkbox') {
        const i = h('input', {type: 'checkbox'}); inputs[f.id] = () => i.checked;
        form.append(h('label', {class: 'check'}, i, h('span', {text: what})));
      }
    }
    const signature = d.needs.signature ? pad(d.signer.name) : null;
    const initials = d.needs.initials ? h('input', {type: 'text', maxlength: '10', value: initialsOf(d.signer.name), 'aria-label': 'Initials'}) : null;
    const consent = h('input', {type: 'checkbox'});
    const error = h('p', {class: 'error', role: 'alert'});
    const signBtn = h('button', {class: 'primary', text: 'Sign'});
    const declineBtn = h('button', {class: 'danger', text: 'Decline', onclick: () => decline()});
    const until = req.expires_at ? ' · sign before ' + new Date(req.expires_at).toLocaleString() : '';
    show(
      card(h('h1', {text: req.title}), h('p', {class: 'muted', text: 'From ' + from + ' · ' + req.number + until}), req.message ? h('p', {text: req.message}) : null),
      card(h('h2', {text: 'Document'}), frame, h('p', {class: 'muted'}, download, ' · SHA-256 ' + req.document_sha256)),
      card(h('h2', {text: 'What you fill in'}), list, form, signature ? signature.el : null,
        initials ? h('label', {text: 'Your initials'}) : null, initials,
        h('label', {class: 'check'}, consent, h('span', {text: d.consent})), error, h('div', {class: 'row'}, signBtn, declineBtn)));
    signBtn.onclick = async () => {
      error.textContent = '';
      if (!consent.checked) { error.textContent = 'Tick the box to agree to sign electronically.'; return; }
      const body = {consent: true, values: {}};
      for (const [id, get] of Object.entries(inputs)) { const v = get(); if (v !== null) body.values[id] = v; }
      if (signature) { const v = signature.value(); if (!v) { error.textContent = 'Draw or type your signature.'; return; } body.signature = v; }
      if (initials && initials.value.trim()) body.initials = {text: initials.value.trim()};
      signBtn.disabled = true;
      try {
        const r = await call('/sign', {method: 'POST', headers: {'Content-Type': 'application/json'}, body: JSON.stringify(body)});
        if (r.ok) {
          const j = await r.json();
          return say('Signed', j.request_status === 'Completed' ? 'Thank you. Everyone has signed; the sender has the completed document.' : 'Thank you. Your signature is recorded.');
        }
        if (r.status === 410) return say('Nothing to sign', (await r.json()).message);
        error.textContent = await failure(r);
      } finally { signBtn.disabled = false; }
    };
  }

  function decline() {
    const reason = h('textarea', {rows: '3', maxlength: '1000', 'aria-label': 'Reason'});
    const error = h('p', {class: 'error', role: 'alert'});
    const send = async () => {
      const r = await call('/decline', {method: 'POST', headers: {'Content-Type': 'application/json'}, body: JSON.stringify({reason: reason.value})});
      if (r.ok) return say('Declined', 'The sender has been told.');
      error.textContent = await failure(r);
    };
    show(card(h('h1', {text: 'Decline to sign'}), h('p', {text: 'Tell the sender why. This ends the request for everyone.'}), reason, error,
      h('div', {class: 'row'}, h('button', {class: 'danger', text: 'Decline', onclick: send}), h('button', {text: 'Back', onclick: () => start()}))));
  }

  start().catch((e) => say('Not available', 'The page could not load: ' + e.message));
})();
</script></body></html>
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_page_runs_only_its_own_script() {
        assert!(page("short").is_err());
        let resp = page(&"A".repeat(token::TOKEN_LEN)).unwrap();
        let html = String::from_utf8(resp.body).unwrap();
        let csp = resp.headers.iter().find(|(k, _)| k == "content-security-policy").map(|(_, v)| v.clone()).unwrap();
        let nonce = csp.split("'nonce-").nth(1).and_then(|r| r.split('\'').next()).unwrap();
        assert!(html.contains(&format!("<script nonce=\"{nonce}\">")) && !html.contains("__NONCE__"));
        assert!(!html.contains("src=\"http") && !html.contains("href=\"http"), "nothing from elsewhere");
        assert!(csp.contains("default-src 'none'") && csp.contains("frame-ancestors 'none'"));
    }
}
