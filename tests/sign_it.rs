//! Sign end to end against a real PostgreSQL (`FERRUM_SIGN_TEST_DATABASE_URL`, default
//! `postgres://ferrum:ferrum@localhost:5432/ferrum_sign_test`; the database is reset first).
//!
//! Senders draft requests through the records API and run their actions; signers act only
//! through the public endpoints with the tokens of their links, as the signing page does. One
//! workspace has no Mail (links are handed out), the other has the real Mail app (links and the
//! sealed copies are emailed). Covered: drafting refusals, sequential and parallel signing with
//! two signers, access codes and lockout, every token misuse, idempotent and concurrent
//! submissions, sealing (stamped pages + certificate as an incremental update), verification
//! and tamper detection, the append-only hash-chained trail, decline, void, expiry, reminders
//! exactly once, corrections, templates, permissions and reports.

use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use ferrum_kernel::tenant::NewTenant;
use ferrum_kernel::{Ctx, Db, DbConfig, Doc, Error, HookRegistry, Kernel, KernelSettings, ListQuery, SiteRequest, SiteResponse};
use ferrum_meta::Catalog;
use hmac::{Hmac, Mac};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use uuid::Uuid;

const PASSWORD: &str = "Correct-Horse-9";

fn obj(v: Value) -> Doc {
    v.as_object().cloned().unwrap()
}

fn uid(v: &Doc) -> Uuid {
    v["id"].as_str().unwrap().parse().unwrap()
}

fn sha(b: &[u8]) -> String {
    hex::encode(Sha256::digest(b))
}

/// A refusal of the expected kind.
#[track_caller]
fn refused<T: std::fmt::Debug>(r: Result<T, Error>, kind: &str) -> String {
    let ok = matches!(
        (&r, kind),
        (Err(Error::NotFound(_)), "not found")
            | (Err(Error::Forbidden(_)), "forbidden")
            | (Err(Error::Invalid(_) | Error::Validation(_)), "invalid")
            | (Err(Error::Conflict(_)), "conflict")
            | (Err(Error::Unauthorized(_)), "unauthorized")
            | (Err(Error::TooManyRequests(_)), "too many")
    );
    assert!(ok, "expected {kind}, got {r:?}");
    match r {
        Err(Error::Validation(v)) => v.iter().map(|f| f.message.clone()).collect::<Vec<_>>().join("; "),
        Err(e) => e.to_string(),
        Ok(_) => unreachable!(),
    }
}

// ───────────────────────────── fixtures ─────────────────────────────

/// A two-page PDF: page 1 portrait Letter, page 2 the same box shown rotated 90° (landscape).
fn contract_pdf(label: &str) -> Vec<u8> {
    use lopdf::{Dictionary, Document, Object, Stream};
    let mut doc = Document::with_version("1.7");
    let pages_id = doc.new_object_id();
    let mut font = Dictionary::new();
    font.set("Type", Object::Name(b"Font".to_vec()));
    font.set("Subtype", Object::Name(b"Type1".to_vec()));
    font.set("BaseFont", Object::Name(b"Helvetica".to_vec()));
    let font = doc.add_object(font);
    let mut fonts = Dictionary::new();
    fonts.set("F1", font);
    let mut res = Dictionary::new();
    res.set("Font", fonts);
    let mut kids = vec![];
    for (i, rotate) in [0i64, 90].into_iter().enumerate() {
        let body = format!("BT /F1 14 Tf 72 720 Td ({label}, page {}) Tj ET", i + 1);
        let content = doc.add_object(Stream::new(Dictionary::new(), body.into_bytes()));
        let mut page = Dictionary::new();
        page.set("Type", Object::Name(b"Page".to_vec()));
        page.set("Parent", pages_id);
        page.set("Contents", content);
        page.set("Resources", res.clone());
        page.set("MediaBox", vec![Object::Integer(0), Object::Integer(0), Object::Integer(612), Object::Integer(792)]);
        if rotate != 0 {
            page.set("Rotate", rotate);
        }
        kids.push(Object::Reference(doc.add_object(page)));
    }
    let mut tree = Dictionary::new();
    tree.set("Type", Object::Name(b"Pages".to_vec()));
    tree.set("Count", 2i64);
    tree.set("Kids", kids);
    doc.objects.insert(pages_id, Object::Dictionary(tree));
    let mut catalog = Dictionary::new();
    catalog.set("Type", Object::Name(b"Catalog".to_vec()));
    catalog.set("Pages", pages_id);
    let catalog = doc.add_object(catalog);
    doc.trailer.set("Root", catalog);
    let mut out = Vec::new();
    doc.save_to(&mut out).unwrap();
    out
}

/// A drawn signature: a transparent canvas with a dark stroke, as a PNG data URL.
fn drawn(w: u32, h: u32) -> String {
    let mut data = vec![0u8; (w * h * 4) as usize];
    for (i, p) in data.as_chunks_mut::<4>().0.iter_mut().enumerate() {
        let (x, y) = (i as u32 % w, i as u32 / w);
        if y.abs_diff(h / 2 + (x % 20) / 4) <= 1 && x > 4 && x < w - 4 {
            p.copy_from_slice(&[17, 17, 17, 255]);
        }
    }
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, w, h);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        enc.write_header().unwrap().write_image_data(&data).unwrap();
    }
    format!("data:image/png;base64,{}", STANDARD.encode(out))
}

fn blank(w: u32, h: u32) -> String {
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, w, h);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        enc.write_header().unwrap().write_image_data(&vec![0u8; (w * h * 4) as usize]).unwrap();
    }
    format!("data:image/png;base64,{}", STANDARD.encode(out))
}

/// The platform's object storage, in memory, with a way to tamper with a stored file.
#[derive(Default)]
struct MemStore(Mutex<HashMap<String, Vec<u8>>>);

#[async_trait::async_trait]
impl ferrum_kernel::files::FileStore for MemStore {
    async fn put(&self, key: &str, data: &[u8]) -> ferrum_kernel::Result<()> {
        self.0.lock().unwrap().insert(key.to_string(), data.to_vec());
        Ok(())
    }
    async fn get(&self, key: &str) -> ferrum_kernel::Result<Vec<u8>> {
        self.0.lock().unwrap().get(key).cloned().ok_or_else(|| Error::NotFound("object".into()))
    }
    async fn delete(&self, key: &str) -> ferrum_kernel::Result<()> {
        self.0.lock().unwrap().remove(key);
        Ok(())
    }
}

impl MemStore {
    /// Flips one byte in the middle of a stored file; returns the original bytes.
    fn tamper(&self, file: Uuid) -> Vec<u8> {
        let mut map = self.0.lock().unwrap();
        let (_, data) = map.iter_mut().find(|(k, _)| k.ends_with(&file.to_string())).expect("stored object");
        let before = data.clone();
        let mid = data.len() / 2;
        data[mid] ^= 0x20;
        before
    }
    fn restore(&self, file: Uuid, bytes: Vec<u8>) {
        let mut map = self.0.lock().unwrap();
        let (_, data) = map.iter_mut().find(|(k, _)| k.ends_with(&file.to_string())).expect("stored object");
        *data = bytes;
    }
}

/// A PDF service standing in for Gotenberg: every page it is sent comes back as a two-page PDF.
async fn fake_pdf_renderer() -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            let mut buf = Vec::new();
            let mut chunk = [0u8; 8192];
            loop {
                let n = sock.read(&mut chunk).await.unwrap_or(0);
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&chunk[..n]);
                let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") else { continue };
                let head = String::from_utf8_lossy(&buf[..end]).to_lowercase();
                let len: usize = head.lines().find_map(|l| l.strip_prefix("content-length:")).and_then(|v| v.trim().parse().ok()).unwrap_or(0);
                if buf.len() >= end + 4 + len {
                    break;
                }
            }
            let body = contract_pdf("Generated quote");
            let head = format!("HTTP/1.1 200 OK\r\nContent-Type: application/pdf\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
            let _ = sock.write_all(head.as_bytes()).await;
            let _ = sock.write_all(&body).await;
        }
    });
    format!("http://{addr}")
}

// ───────────────────────────── harness ─────────────────────────────

/// The tests share one database, which each resets.
static DB: std::sync::Mutex<()> = std::sync::Mutex::new(());

struct T {
    k: Arc<Kernel>,
    store: Arc<MemStore>,
    /// Sign's own handle on the vault, as its registration was given it: what reads its secret fields.
    sign: ferrum_kernel::vault::Vault,
    /// The handles of the test's own apps (each package of `boot_with`'s, as an app of its name).
    apps: HashMap<String, ferrum_kernel::vault::Vault>,
}

/// The server's mailer in memory: `(to, subject, html, attached file names)`.
#[derive(Default)]
struct TestMailer(Mutex<Vec<(String, String, String, Vec<String>)>>);

#[async_trait::async_trait]
impl ferrum_kernel::Mailer for TestMailer {
    async fn send(&self, to: &str, subject: &str, html: &str) -> ferrum_kernel::Result<()> {
        self.0.lock().unwrap().push((to.into(), subject.into(), html.into(), vec![]));
        Ok(())
    }
    fn can_attach(&self) -> bool {
        true
    }
    async fn send_with(&self, to: &str, subject: &str, html: &str, files: &[ferrum_kernel::MailAttachment]) -> ferrum_kernel::Result<()> {
        self.0.lock().unwrap().push((to.into(), subject.into(), html.into(), files.iter().map(|f| f.file_name.clone()).collect()));
        Ok(())
    }
}

struct Tenant {
    id: Uuid,
    admin: Ctx,
}

impl T {
    async fn tenant(&self, slug: &str, packages: &[&str], tz: &str) -> Tenant {
        let id = self
            .k
            .create_tenant(NewTenant {
                slug: slug.into(),
                name: format!("{} Corp", slug[..1].to_uppercase() + &slug[1..]),
                admin_email: format!("admin@{slug}.test"),
                admin_password: PASSWORD.into(),
                admin_name: None,
                packages: packages.iter().map(|p| p.to_string()).collect(),
                settings: Some(json!({"timezone": tz})),
            })
            .await
            .expect("tenant");
        let session = self.k.login(slug, &format!("admin@{slug}.test"), PASSWORD).await.expect("login");
        let admin = self.k.load_ctx(id, session.user, Some(session.token_version), "it".into()).await.expect("ctx");
        Tenant { id, admin }
    }

    async fn user(&self, t: &Tenant, email: &str, name: &str, roles: &[&str]) -> Ctx {
        let roles: Vec<String> = roles.iter().map(|r| r.to_string()).collect();
        let id = self.k.create_user(&t.admin, email, name, Some(PASSWORD), &roles, false).await.expect("user");
        self.k.load_ctx(t.id, id, None, "it".into()).await.expect("ctx")
    }

    fn sys(&self, t: &Tenant) -> Ctx {
        Ctx::system(t.id, t.admin.schema.clone())
    }

    async fn upload(&self, ctx: &Ctx, name: &str, bytes: &[u8]) -> String {
        let f = self.k.store_file(ctx, None, name, "application/pdf", bytes).await.expect("upload");
        format!("/files/{}", f["id"].as_str().unwrap())
    }

    async fn get(&self, ctx: &Ctx, entity: &str, id: Uuid) -> Doc {
        self.k.get(ctx, entity, id, false).await.unwrap_or_else(|e| panic!("get {entity}: {e:?}"))
    }

    async fn action(&self, ctx: &Ctx, id: Uuid, action: &str, params: Value) -> Value {
        self.k.run_action(ctx, "SignRequest", id, action, params).await.unwrap_or_else(|e| panic!("{action}: {e:?}"))
    }

    async fn list(&self, ctx: &Ctx, entity: &str, filters: Value) -> Vec<Doc> {
        let q = ListQuery {
            filters: ferrum_kernel::query::parse_filters(&filters).unwrap(),
            limit: Some(500),
            sort: Some("created_at asc".into()),
            ..Default::default()
        };
        self.k.list(ctx, entity, &q).await.unwrap_or_else(|e| panic!("list {entity}: {e:?}")).rows
    }

    /// The request's audit trail as `event:role` strings, in order.
    async fn trail(&self, t: &Tenant, request: Uuid) -> Vec<String> {
        let mut tx = self.k.db.tenant_tx(t.id).await.unwrap();
        let rows: Vec<(String, Option<String>)> = sqlx::query_as("select event, role from e_sign_event where tenant_id = $1 and request = $2 order by seq")
            .bind(t.id)
            .bind(request)
            .fetch_all(&mut *tx)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        rows.into_iter().map(|(e, r)| r.map_or(e.clone(), |r| format!("{e}:{r}"))).collect()
    }

    async fn count(&self, t: &Tenant, request: Uuid, prefix: &str) -> usize {
        self.trail(t, request).await.iter().filter(|e| e.starts_with(prefix)).count()
    }

    /// `POST /verify` with a PDF.
    async fn verify_pdf(&self, t: &Tenant, bytes: Vec<u8>) -> Value {
        let resp = self.site(t, "POST", "/verify", None, bytes).await.expect("verify");
        serde_json::from_slice(&resp.body).unwrap()
    }

    async fn sql(&self, t: &Tenant, sql: &str, id: Uuid) -> Result<u64, sqlx::Error> {
        let mut tx = self.k.db.tenant_tx(t.id).await.unwrap();
        let n = sqlx::query(sqlx::AssertSqlSafe(sql.to_string())).bind(t.id).bind(id).execute(&mut *tx).await?.rows_affected();
        tx.commit().await?;
        Ok(n)
    }

    async fn scalar<O: Send + Unpin + for<'r> sqlx::Decode<'r, sqlx::Postgres> + sqlx::Type<sqlx::Postgres>>(&self, t: &Tenant, sql: &str, id: Uuid) -> O {
        let mut tx = self.k.db.tenant_tx(t.id).await.unwrap();
        let v = sqlx::query_scalar(sqlx::AssertSqlSafe(sql.to_string())).bind(t.id).bind(id).fetch_one(&mut *tx).await.unwrap_or_else(|e| panic!("{sql}: {e}"));
        tx.commit().await.unwrap();
        v
    }

    /// Bell notifications of a user, newest last: `(title, body)`.
    async fn notifications(&self, t: &Tenant, user: &Ctx) -> Vec<(String, Option<String>)> {
        let mut tx = self.k.db.tenant_tx(t.id).await.unwrap();
        let rows = sqlx::query_as("select title, body from notification where tenant_id = $1 and user_id = $2 order by created_at, id")
            .bind(t.id)
            .bind(user.user)
            .fetch_all(&mut *tx)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        rows
    }

    async fn key(&self, t: &Tenant) -> Vec<u8> {
        let mut tx = self.k.db.tenant_tx(t.id).await.unwrap();
        let id: Uuid = sqlx::query_scalar("select id from e_sign_settings where tenant_id = $1").bind(t.id).fetch_one(&mut *tx).await.unwrap();
        let hexed = self.sign.field(&self.k, &mut tx, t.id, "SignSettings", id, "signing_key").await.unwrap().expect("signing key");
        tx.commit().await.unwrap();
        hex::decode(hexed.as_str()).unwrap()
    }

    /// One call to the public site, as the server hands it to the package.
    async fn site(&self, t: &Tenant, method: &str, path: &str, code: Option<&str>, body: Vec<u8>) -> Result<SiteResponse, Error> {
        let code: Vec<(&str, &str)> = code.map(|c| ("x-access-code", c)).into_iter().collect();
        self.site_with(t, method, path, &code, body).await
    }

    /// [`site`](Self::site) with the request's headers (`user-agent` by default).
    async fn site_with(&self, t: &Tenant, method: &str, path: &str, extra: &[(&str, &str)], body: Vec<u8>) -> Result<SiteResponse, Error> {
        self.site_as(t, None, method, path, extra, body).await
    }

    /// [`site_with`](Self::site_with) for a caller who carries a site session (a signed-in customer): the platform hands the
    /// package that person's context.
    async fn site_as(&self, t: &Tenant, user: Option<&Ctx>, method: &str, path: &str, extra: &[(&str, &str)], body: Vec<u8>) -> Result<SiteResponse, Error> {
        let mut headers = Map::new();
        headers.insert("user-agent".into(), json!("Mozilla/5.0 (it) Firefox/140.0"));
        for (k, v) in extra {
            headers.insert(k.to_string(), json!(v));
        }
        let (path, query) = path.split_once('?').unwrap_or((path, ""));
        let query: Map<String, Value> = query.split('&').filter_map(|p| p.split_once('=')).map(|(k, v)| (k.to_string(), json!(v))).collect();
        let req =
            SiteRequest { tenant: t.id, user: user.cloned(), method: method.into(), path: path.into(), query, headers, body, client_ip: "203.0.113.7".into() };
        let resp = self.k.hooks.site("sign").expect("sign site").handle(&self.k, req).await?;
        assert!(resp.headers.iter().any(|(k, v)| k == "cache-control" && v == "no-store"));
        Ok(resp)
    }

    /// A JSON call: `(status, body)`.
    async fn json(&self, t: &Tenant, method: &str, path: &str, code: Option<&str>, body: Option<Value>) -> Result<(u16, Value), Error> {
        let resp = self.site(t, method, path, code, body.map(|b| b.to_string().into_bytes()).unwrap_or_default()).await?;
        assert_eq!(resp.content_type, "application/json");
        Ok((resp.status, serde_json::from_slice(&resp.body).unwrap()))
    }

    async fn view(&self, t: &Tenant, token: &str, code: Option<&str>) -> Result<(u16, Value), Error> {
        self.json(t, "GET", &format!("/signing/{token}"), code, None).await
    }

    async fn sign(&self, t: &Tenant, token: &str, code: Option<&str>, body: Value) -> Result<(u16, Value), Error> {
        self.json(t, "POST", &format!("/signing/{token}/sign"), code, Some(body)).await
    }

    async fn report(&self, ctx: &Ctx, name: &str, params: &[(&str, &str)]) -> Value {
        let p: HashMap<String, String> = params.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        ferrum_domain::reports::run(&self.k, ctx, name, &p).await.unwrap_or_else(|e| panic!("report {name}: {e:?}"))
    }

    async fn maintenance(&self, tenant: &Tenant) {
        ferrum_app_sign::maintenance(&self.k, tenant.id).await.expect("maintenance");
    }
}

/// The token at the end of a signing URL.
fn token_of(url: &str) -> String {
    url.rsplit('/').next().unwrap().to_string()
}

/// A link's token computed independently from the documented format: the signer row id and the
/// first 16 bytes of HMAC-SHA256 over length-prefixed context, tenant, request, signer and email.
fn token_for(key: &[u8], tenant: Uuid, request: Uuid, signer: Uuid, email: &str) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).unwrap();
    for part in [b"ferrum-sign/link/v1".as_slice(), tenant.as_bytes(), request.as_bytes(), signer.as_bytes(), email.to_lowercase().as_bytes()] {
        mac.update(&(part.len() as u32).to_be_bytes());
        mac.update(part);
    }
    let mut raw = signer.as_bytes().to_vec();
    raw.extend_from_slice(&mac.finalize().into_bytes()[..16]);
    URL_SAFE_NO_PAD.encode(raw)
}

fn signer_row(doc: &Doc, role: &str) -> Doc {
    doc["signers"].as_array().unwrap().iter().find(|r| r["role"] == json!(role)).unwrap().as_object().cloned().unwrap()
}

fn field_id(doc: &Doc, label: &str) -> String {
    doc["fields"].as_array().unwrap().iter().find(|r| r["label"] == json!(label)).unwrap()["id"].as_str().unwrap().to_string()
}

fn field(role: &str, kind: &str, label: &str, page: i64, x: i64, y: i64, w: i64, h: i64) -> Value {
    json!({"role": role, "field_type": kind, "label": label, "page": page, "x": x, "y": y, "width": w, "height": h})
}

/// A one-signer request asking for a code by text message, sent; returns it and the signer's token.
async fn code_request(t: &T, sender: &Ctx, pdf: &[u8], who: &str, phone: &str) -> (Uuid, String) {
    let req = json!({"title": format!("Code for {who}"), "document": t.upload(sender, &format!("{who}.pdf"), pdf).await, "verify_identity": "Text Code",
        "signers": [{"role": "Signer", "name": who, "email": format!("{who}@loan.test"), "phone": phone}],
        "fields": [field("Signer", "Signature", "S", 1, 72, 600, 200, 50)]});
    let id = uid(&t.k.insert(sender, "SignRequest", obj(req)).await.unwrap());
    let sent = t.action(sender, id, "send", json!({})).await;
    (id, token_of(sent["signers"][0]["url"].as_str().unwrap()))
}

/// Asks for a code on the public route and reads it as a test may: from the platform's test channel, where a
/// delivery holds it in the clear (everywhere else it is sealed).
async fn mailed_code(t: &T, acme: &Tenant, token: &str, phone: &str) -> String {
    let (status, _) = t.json(acme, "POST", &format!("/signing/{token}/code"), None, None).await.unwrap();
    assert_eq!(status, 200);
    texted_code(t, acme, phone).await
}

/// The latest code the test channel delivered to `phone`.
async fn texted_code(t: &T, acme: &Tenant, phone: &str) -> String {
    let mut tx = t.k.db.system_tx().await.unwrap();
    // Only rows this statement can lock, and only those still in the future: another scenario's runner may hold the rest
    // (two statements making every workspace's jobs due, in different orders, deadlocked: 40P01).
    sqlx::query("update job set run_at = now() where id in (select id from job where status = 'queued' and run_at > now() for update skip locked)")
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    while t.k.run_jobs_once(4).await.unwrap() > 0 {}
    let sent = ferrum_kernel::messages::test_deliveries(acme.id).into_iter().rev().find(|d| d.0 == phone).expect("the code was delivered");
    let text = sent.1["text"].as_str().unwrap().to_string();
    text.split(|c: char| !c.is_ascii_digit()).find(|w| w.len() == 6).unwrap_or_else(|| panic!("no six-digit code in {text:?}")).to_string()
}

#[test]
fn sign_end_to_end() {
    let _db = DB.lock().unwrap_or_else(|e| e.into_inner());
    // Same runtime shape as the server: unoptimized async hook chains need more than 2 MiB.
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().thread_stack_size(16 << 20).build().unwrap();
    rt.block_on(async { tokio::spawn(scenario()).await.unwrap() });
}

async fn scenario() {
    let url = std::env::var("FERRUM_SIGN_TEST_DATABASE_URL").unwrap_or_else(|_| "postgres://ferrum:ferrum@localhost:5432/ferrum_sign_test".into());
    let db = Db::connect(DbConfig { url: url.clone(), max_connections: 16, ..Default::default() }).await.expect("connect");
    sqlx::raw_sql("drop schema if exists vault cascade; drop schema if exists public cascade; create schema public;").execute(&db.pool).await.expect("reset");
    ferrum_kernel::vault::open(&db, opener(&url), false).await.expect("the vault");
    let mut packages = ferrum_domain::packages().expect("platform packages");
    packages.extend(ferrum_app_sign::packages().expect("app packages"));
    let catalog = Catalog::from_packages(packages).expect("catalog");
    let mut hooks = HookRegistry::default();
    ferrum_domain::register(&mut hooks);
    let sign_packages: Vec<String> = ferrum_app_sign::packages().unwrap().iter().map(|p| p.name().to_string()).collect();
    let mut sign = None;
    hooks.app("sign", &sign_packages, |h| {
        ferrum_app_sign::register(h);
        sign = Some(h.vault());
    });
    hooks.own("sign", &sign_packages);
    ferrum_domain::reports::set_sources(ferrum_app_sign::reports().into_iter().map(|r| ("sign".to_string(), r)).collect()).expect("Sign's reports");
    let settings = KernelSettings { pdf_url: Some(fake_pdf_renderer().await), ..Default::default() };
    let k = Kernel::boot(db, catalog, hooks, settings, None).await.expect("boot");
    let store = Arc::new(MemStore::default());
    k.set_file_store(store.clone());
    let t = T { k: k.clone(), store, sign: sign.expect("Sign's handle"), apps: HashMap::new() };

    // ───────────── a workspace without Mail, in New York ─────────────
    let acme = t.tenant("acme", &["sign"], "America/New_York").await;
    let asha = t.user(&acme, "asha@acme.test", "Asha Rao", &["Sign User"]).await;
    let ravi = t.user(&acme, "ravi@acme.test", "Ravi Kumar", &["Sign User"]).await;
    let mira = t.user(&acme, "mira@acme.test", "Mira Chen", &["Sign Manager"]).await;
    let nobody = t.user(&acme, "nobody@acme.test", "No Role", &[]).await;
    t.k.insert(&mira, "SignSettings", obj(json!({"sender_name": "Acme Corp", "reminder_every_days": 1, "max_reminders": 2}))).await.expect("settings");

    let pdf = contract_pdf("Master Services Agreement");
    let document = t.upload(&asha, "msa.pdf", &pdf).await;

    // ───────────── drafting: only real PDFs, fields inside their pages ─────────────
    let signers = json!([
        {"role": "Client", "name": "Jane Doe", "email": "Jane@Client.test", "signing_order": 1},
        {"role": "Company", "name": "Ravi Kumar", "email": "ravi@acme.test", "signing_order": 2, "access_code": "BLUE-4242"},
    ]);
    let fields = json!([
        field("Client", "Signature", "Client signature", 1, 72, 600, 200, 50),
        field("Client", "Date Signed", "Client date", 1, 300, 620, 120, 20),
        field("Client", "Text", "Company name", 1, 72, 560, 250, 20),
        field("Client", "Checkbox", "I accept the terms", 1, 72, 530, 14, 14),
        field("Client", "Initials", "Client initials", 2, 700, 560, 50, 30),
        field("Company", "Signature", "Company signature", 2, 72, 480, 220, 60),
        field("Company", "Name", "Company signatory", 2, 72, 550, 220, 20),
    ]);
    let draft = |doc: &str, fields: Value| json!({"title": "Master Services Agreement", "message": "Please sign by Friday.", "document": doc, "signers": signers, "fields": fields});
    let text = t.k.store_file(&asha, None, "notes.txt", "text/plain", b"hello, not a pdf").await.unwrap();
    let m = refused(t.k.insert(&asha, "SignRequest", obj(draft(&format!("/files/{}", text["id"].as_str().unwrap()), fields.clone()))).await, "invalid");
    assert!(m.contains("not a PDF"), "{m}");
    let fake = t.upload(&asha, "fake.pdf", b"%PDF-1.7\n1 0 obj << /Type /Catalog >> endobj\ntrailer << >>\n%%EOF").await;
    let m = refused(t.k.insert(&asha, "SignRequest", obj(draft(&fake, fields.clone()))).await, "invalid");
    assert!(m.contains("cannot be read") || m.contains("no pages"), "a broken PDF with a PDF header: {m}");
    let ravis = t.upload(&ravi, "ravi.pdf", &pdf).await;
    assert!(refused(t.k.insert(&asha, "SignRequest", obj(draft(&ravis, fields.clone()))).await, "invalid").contains("may not use it"), "someone else's upload");
    let mut outside = fields.clone();
    outside[0]["x"] = json!(500); // 500 + 200 > 612
    assert!(refused(t.k.insert(&asha, "SignRequest", obj(draft(&document, outside))).await, "invalid").contains("outside page 1"));
    let mut landscape = fields.clone();
    landscape[4]["y"] = json!(600); // page 2 is displayed 792 × 612: 600 + 30 > 612
    assert!(refused(t.k.insert(&asha, "SignRequest", obj(draft(&document, landscape))).await, "invalid").contains("outside page 2"));
    let mut missing_page = fields.clone();
    missing_page[0]["page"] = json!(3);
    assert!(refused(t.k.insert(&asha, "SignRequest", obj(draft(&document, missing_page))).await, "invalid").contains("page 3 does not exist"));
    let mut stranger = fields.clone();
    stranger[0]["role"] = json!("Witness");
    assert!(refused(t.k.insert(&asha, "SignRequest", obj(draft(&document, stranger))).await, "invalid").contains("no signer has the role Witness"));
    let short_code = json!({"title": "x", "document": document, "signers": [{"role": "A", "name": "A", "email": "a@x.test", "access_code": "123"}]});
    assert!(refused(t.k.insert(&asha, "SignRequest", obj(short_code)).await, "invalid").contains("4 to 64"));

    let msa = t.k.insert(&asha, "SignRequest", obj(draft(&document, fields.clone()))).await.expect("draft");
    let msa_id = uid(&msa);
    assert_eq!(msa["status"], json!("Draft"));
    assert!(msa["number"].as_str().unwrap().starts_with("SIGN-"));
    assert_eq!((msa["page_count"].clone(), msa["page_sizes"].clone()), (json!(2), json!([[612.0, 792.0], [792.0, 612.0]])));
    assert_eq!(msa["document_sha256"], json!(sha(&pdf)));
    assert_eq!((msa["document_name"].clone(), msa["sender_email"].clone()), (json!("msa.pdf"), json!("asha@acme.test")));
    let company = signer_row(&msa, "Company");
    assert!(company["access_code"].is_null() && company["access_code_hash"].as_str().unwrap().starts_with("$argon2id$"), "access codes are kept hashed");
    assert!(signer_row(&msa, "Client")["email"] == json!("jane@client.test"));
    let attached: String = t.scalar(&acme, "select entity from file_object where tenant_id = $1 and id = $2", document[7..].parse().unwrap()).await;
    assert_eq!(attached, "SignRequest", "the upload now belongs to the request");

    // A signer without a signature field cannot be sent.
    let lone = t
        .k
        .insert(&asha, "SignRequest", obj(json!({"title": "No signature", "document": document, "signers": [{"role": "A", "name": "Ann", "email": "ann@x.test"}], "fields": [field("A", "Text", "Note", 1, 10, 10, 100, 20)]})))
        .await
        .unwrap();
    assert!(refused(t.k.run_action(&asha, "SignRequest", uid(&lone), "send", json!({})).await, "invalid").contains("required Signature field"));
    t.k.delete(&asha, "SignRequest", uid(&lone), None).await.expect("drafts can be deleted");

    // ───────────── sending without Mail: copyable links, sequential order ─────────────
    let sent = t.action(&asha, msa_id, "send", json!({})).await;
    assert_eq!(sent["status"], json!("Sent"));
    let links = sent["signers"].as_array().unwrap();
    assert_eq!(links.len(), 1, "sequential: only the first signer is asked");
    assert_eq!((links[0]["role"].clone(), links[0]["emailed"].clone()), (json!("Client"), json!(false)));
    let client_url = links[0]["url"].as_str().unwrap().to_string();
    assert!(client_url.starts_with("/api/site/acme/sign/s/"), "{client_url}");
    let client = token_of(&client_url);
    let msa = t.get(&asha, "SignRequest", msa_id).await;
    let today = acme.admin.today();
    assert_eq!(msa["expires_on"], json!((today + chrono::Days::new(30)).to_string()));
    let ny: chrono_tz::Tz = "America/New_York".parse().unwrap();
    let local_midnight = chrono::TimeZone::from_local_datetime(&ny, &(today + chrono::Days::new(31)).and_hms_opt(0, 0, 0).unwrap()).earliest().unwrap();
    assert_eq!(msa["expires_at"].as_str().unwrap(), local_midnight.with_timezone(&chrono::Utc).to_rfc3339_opts(chrono::SecondsFormat::Millis, true));
    assert_eq!((signer_row(&msa, "Client")["status"].clone(), signer_row(&msa, "Company")["status"].clone()), (json!("Sent"), json!("Waiting")));
    assert!(
        t.notifications(&acme, &asha)
            .await
            .iter()
            .any(|(title, body)| title.starts_with("Signing links for") && body.as_deref().unwrap_or_default().contains(&client))
    );
    assert_eq!(t.trail(&acme, msa_id).await, vec!["Sent:Client"]);
    let original_file: Uuid = document[7..].parse().unwrap();
    let frozen: String = t.scalar(&acme, "select entity from file_object where tenant_id = $1 and id = $2", original_file).await;
    assert_eq!(frozen, "SignEvent", "the original belongs to the audit trail once sent");
    refused(t.k.delete_file(&asha, original_file).await, "forbidden");
    refused(t.k.delete_file(&mira, original_file).await, "forbidden");
    assert_eq!(t.k.read_file(&asha, original_file).await.expect("the sender still reads it").2, pdf);
    // Sent requests do not change, except corrections of signers who have not signed.
    assert!(refused(t.k.update(&asha, "SignRequest", msa_id, obj(json!({"title": "Changed"})), None).await, "invalid").contains("cannot change"));
    refused(t.k.delete(&asha, "SignRequest", msa_id, None).await, "invalid");

    // ───────────── every token misuse is refused ─────────────
    let key = t.key(&acme).await;
    let company_row = signer_row(&msa, "Company");
    let company_token = token_for(&key, acme.id, msa_id, uid(&company_row), "ravi@acme.test");
    assert_eq!(token_for(&key, acme.id, msa_id, uid(&signer_row(&msa, "Client")), "jane@client.test"), client, "the documented format");
    let mut flipped = client.clone().into_bytes();
    flipped[30] = if flipped[30] == b'A' { b'B' } else { b'A' };
    let flipped = String::from_utf8(flipped).unwrap();
    for bad in [flipped.as_str(), "short", &format!("{}!", &client[..42]), &format!("{client}A")] {
        refused(t.view(&acme, bad, None).await, "not found");
        refused(t.sign(&acme, bad, None, json!({"consent": true})).await, "not found");
    }
    let other = token_for(&key, Uuid::now_v7(), msa_id, uid(&company_row), "ravi@acme.test");
    refused(t.view(&acme, &other, None).await, "not found");
    let wrong_email = token_for(&key, acme.id, msa_id, uid(&company_row), "someone@else.test");
    refused(t.view(&acme, &wrong_email, None).await, "not found");
    assert!(
        refused(t.view(&acme, &company_token, Some("BLUE-4242")).await, "forbidden").contains("not your turn"),
        "sequential: only the current signer's link works"
    );
    assert_eq!(t.site(&acme, "GET", "/s/short", None, vec![]).await.err().map(|e| e.status()), Some(404));
    let page = t.site(&acme, "GET", &format!("/s/{client}"), None, vec![]).await.unwrap();
    assert!(page.content_type.starts_with("text/html") && page.headers.iter().any(|(k, v)| k == "content-security-policy" && v.contains("default-src 'none'")));

    // ───────────── the first signer: own fields only, validation, idempotency ─────────────
    let (status, view) = t.view(&acme, &client, None).await.unwrap();
    assert_eq!(status, 200);
    assert_eq!(view["fields"].as_array().unwrap().len(), 5, "only the Client's fields");
    assert!(view["fields"].as_array().unwrap().iter().all(|f| f["label"] != json!("Company signature")));
    assert_eq!((view["needs"]["signature"].clone(), view["needs"]["initials"].clone()), (json!(true), json!(true)));
    assert_eq!(view["request"]["sender"]["organization"], json!("Acme Corp"));
    assert_eq!(view["request"]["pages"][1], json!({"page": 2, "width": 792.0, "height": 612.0}));
    t.view(&acme, &client, None).await.unwrap();
    let doc_resp = t.site(&acme, "GET", &format!("/signing/{client}/document"), None, vec![]).await.unwrap();
    assert_eq!((doc_resp.content_type.as_str(), doc_resp.body == pdf), ("application/pdf", true));
    assert_eq!(t.trail(&acme, msa_id).await, vec!["Sent:Client", "Viewed:Client"], "one view event, however often the link is opened");

    let msa = t.get(&asha, "SignRequest", msa_id).await;
    let (company_name, accept, company_signatory) = (field_id(&msa, "Company name"), field_id(&msa, "I accept the terms"), field_id(&msa, "Company signatory"));
    let good =
        json!({"consent": true, "signature": {"image": drawn(600, 200)}, "values": {company_name.clone(): "Client Industries LLC", accept.clone(): true}});
    let with = |patch: Value| {
        let mut b = good.clone();
        for (k, v) in patch.as_object().unwrap() {
            b[k] = v.clone();
        }
        b
    };
    assert!(refused(t.sign(&acme, &client, None, with(json!({"consent": false}))).await, "invalid").contains("agree to sign electronically"));
    assert!(refused(t.sign(&acme, &client, None, with(json!({"signature": {"image": "data:image/jpeg;base64,/9j/4AAQ"}}))).await, "invalid").contains("PNG"));
    assert!(refused(t.sign(&acme, &client, None, with(json!({"signature": {"image": blank(600, 200)}}))).await, "invalid").contains("empty"));
    assert!(refused(t.sign(&acme, &client, None, with(json!({"signature": {"image": drawn(8, 4)}}))).await, "invalid").contains("pixels"));
    assert!(refused(t.sign(&acme, &client, None, with(json!({"signature": null}))).await, "invalid").contains("draw or type"));
    let long = "A very long company name that cannot possibly fit into a box this small, Incorporated".repeat(2);
    assert!(
        refused(t.sign(&acme, &client, None, with(json!({"values": {company_name.clone(): long, accept.clone(): true}}))).await, "invalid")
            .contains("too long")
    );
    assert!(
        refused(t.sign(&acme, &client, None, with(json!({"values": {company_name.clone(): "Client Industries LLC"}}))).await, "invalid")
            .contains("tick this box")
    );
    assert!(
        refused(t.sign(&acme, &client, None, with(json!({"values": {company_signatory.clone(): "me"}}))).await, "invalid").contains("not one of your"),
        "someone else's field"
    );

    let (status, signed) = t.sign(&acme, &client, None, good.clone()).await.unwrap();
    assert_eq!(
        (status, signed["status"].clone(), signed["request_status"].clone(), signed["replayed"].clone()),
        (200, json!("Signed"), json!("In Progress"), json!(false))
    );
    let (_, again) = t.sign(&acme, &client, None, good.clone()).await.unwrap();
    assert_eq!(again["replayed"], json!(true), "a double submit gets the first answer");
    refused(t.sign(&acme, &client, None, with(json!({"signature": {"text": "Jane Doe"}}))).await, "conflict");
    let (status, gone) = t.view(&acme, &client, None).await.unwrap();
    assert_eq!((status, gone["error"].clone()), (410, json!("gone")), "single use: the link closes after signing");
    assert!(gone["message"].as_str().unwrap().contains("already signed"));
    let msa = t.get(&asha, "SignRequest", msa_id).await;
    let jane = signer_row(&msa, "Client");
    assert_eq!((jane["status"].clone(), jane["initials_text"].clone()), (json!("Signed"), json!("JD")), "initials from the name when not given");
    assert_eq!(jane["ip_hash"].as_str().unwrap().len(), 32);
    assert_ne!(jane["ip_hash"], json!("203.0.113.7"), "addresses are kept hashed");
    assert_eq!(jane["user_agent"], json!("Mozilla/5.0 (it) Firefox/140.0"));
    let values: HashMap<String, Value> =
        msa["fields"].as_array().unwrap().iter().map(|f| (f["label"].as_str().unwrap().to_string(), f["value"].clone())).collect();
    assert_eq!(values["Company name"], json!("Client Industries LLC"));
    assert_eq!(values["I accept the terms"], json!("Yes"));
    assert_eq!(values["Client date"], json!(today.to_string()));
    assert_eq!(values["Client signature"], json!("Drawn signature"));
    assert!(values["Company signatory"].is_null(), "the next signer's fields are untouched");
    assert_eq!(msa["status"], json!("In Progress"));
    assert_eq!(signer_row(&msa, "Company")["status"], json!("Sent"), "now it is the Company's turn");
    let turn = t.notifications(&acme, &asha).await.into_iter().find(|(title, _)| title.contains("turn to sign")).expect("the sender is told");
    assert!(turn.1.unwrap().contains(&company_token), "with the next signer's link, as no Mail is installed");

    // ───────────── the second signer: access code and lockout ─────────────
    let m = refused(t.view(&acme, &company_token, None).await, "unauthorized");
    assert!(m.contains("access code"));
    for _ in 0..4 {
        refused(t.view(&acme, &company_token, Some("WRONG-1111")).await, "forbidden");
    }
    assert!(refused(t.view(&acme, &company_token, Some("WRONG-1111")).await, "too many").contains("try again after"));
    refused(t.view(&acme, &company_token, Some("BLUE-4242")).await, "too many");
    let denied = t.trail(&acme, msa_id).await.iter().filter(|e| e.starts_with("Access Denied")).count();
    assert_eq!(denied, 5);
    let table = "e_sign_request__signers";
    t.sql(&acme, &format!("update {table} set locked_until = now() - interval '1 minute' where tenant_id = $1 and id = $2"), uid(&company_row)).await.unwrap();
    let (status, _) = t.view(&acme, &company_token, Some("BLUE-4242")).await.unwrap();
    assert_eq!(status, 200);
    refused(t.sign(&acme, &company_token, None, json!({"consent": true, "signature": {"text": "Ravi Kumar"}})).await, "unauthorized");
    let (_, done) = t.sign(&acme, &company_token, Some("BLUE-4242"), json!({"consent": true, "signature": {"text": "Ravi Kumar"}})).await.unwrap();
    assert_eq!(done["request_status"], json!("Completed"), "the last signature seals the document");

    // ───────────── the sealed PDF ─────────────
    let msa = t.get(&asha, "SignRequest", msa_id).await;
    assert_eq!(msa["status"], json!("Completed"));
    assert!(msa["completed_at"].is_string() && msa["seal_error"].is_null());
    let sealed_file: Uuid = msa["sealed_document"].as_str().unwrap()[7..].parse().unwrap();
    let (name, ctype, sealed) = t.k.read_file(&asha, sealed_file).await.expect("the sender downloads the sealed PDF");
    assert_eq!((name.as_str(), ctype.as_str()), ("Master Services Agreement - signed.pdf", "application/pdf"));
    assert_eq!(msa["sealed_sha256"], json!(sha(&sealed)));
    let parsed = lopdf::Document::load_mem(&sealed).expect("the sealed PDF parses");
    let pages = parsed.get_pages();
    assert!(pages.len() >= 3, "two pages and the certificate");
    let page_text = |n: u32| String::from_utf8_lossy(&parsed.get_page_content(pages[&n])).to_string();
    assert!(page_text(1).contains("Do") && page_text(1).contains(&hex::encode("Client Industries LLC")), "page 1 carries the drawn signature and the text");
    assert!(page_text(2).contains("0 1 -1 0 612 0 cm") && page_text(2).contains(&hex::encode("Ravi Kumar")), "the rotated page is stamped upright");
    let certificate = page_text(3);
    for expected in ["Certificate of Completion", "Original document SHA-256", msa["number"].as_str().unwrap(), "Jane Doe <jane@client.test>, Client"] {
        assert!(certificate.contains(&hex::encode(expected)), "certificate shows {expected}");
    }
    let events = t.list(&asha, "SignEvent", json!({"request": msa_id.to_string()})).await;
    let completed = events.iter().find(|e| e["event"] == json!("Completed")).expect("completed event");
    let details = completed["details"].as_str().unwrap();
    let prefix: usize = details.split("its first ").nth(1).unwrap().split(' ').next().unwrap().parse().unwrap();
    let signed_sha = details.split("signed document SHA-256 ").nth(1).unwrap().split(' ').next().unwrap();
    assert_eq!(sha(&sealed[..prefix]), signed_sha, "the signed document is a verifiable prefix of the sealed file");
    assert!(certificate.contains(&hex::encode(format!("Signed document SHA-256: {signed_sha}"))), "and its hash is on the certificate");
    assert_eq!(lopdf::Document::load_mem(&sealed[..prefix]).unwrap().get_pages().len(), 2);
    let trail = t.trail(&acme, msa_id).await;
    assert_eq!(&trail[..3], ["Sent:Client", "Viewed:Client", "Signed:Client"]);
    assert_eq!(&trail[trail.len() - 3..], ["Viewed:Company", "Signed:Company", "Completed"]);
    assert!(t.notifications(&acme, &asha).await.iter().any(|(title, _)| title.ends_with("is signed by everyone")));
    for token in [&client, &company_token] {
        assert_eq!(t.view(&acme, token, Some("BLUE-4242")).await.unwrap().0, 410, "links stop working once complete");
    }

    // ───────────── verification and tamper detection ─────────────
    let report = t.action(&asha, msa_id, "verify", json!({})).await;
    assert_eq!(report["valid"], json!(true), "{report}");
    assert_eq!((report["audit_trail"]["events"].clone(), report["sealed"]["readable"].clone()), (json!(trail.len()), json!(true)));
    refused(t.json(&acme, "POST", "/verify", None, None).await, "invalid");
    let genuine = t.verify_pdf(&acme, sealed.clone()).await;
    assert_eq!((genuine["valid"].clone(), genuine["request"]["number"].clone()), (json!(true), msa["number"].clone()));
    assert_eq!(t.verify_pdf(&acme, pdf.clone()).await["valid"], json!(false), "the unsigned original is not a sealed document");
    let mut edited = sealed.clone();
    let at = edited.len() / 3;
    edited[at] ^= 1;
    assert_eq!(t.verify_pdf(&acme, edited).await["valid"], json!(false), "a changed copy does not verify");
    let kept = t.store.tamper(sealed_file);
    let broken = t.action(&asha, msa_id, "verify", json!({})).await;
    assert_eq!((broken["valid"].clone(), broken["sealed"]["ok"].clone()), (json!(false), json!(false)), "tampering with the stored file is detected");
    assert_eq!(t.verify_pdf(&acme, sealed.clone()).await["valid"], json!(false), "and the public check no longer vouches for it");
    t.store.restore(sealed_file, kept);
    assert_eq!(t.action(&asha, msa_id, "verify", json!({})).await["valid"], json!(true));

    // The audit trail is append-only.
    let first_event = uid(&events[0]);
    assert!(
        t.sql(&acme, "update e_sign_event set details = 'edited' where tenant_id = $1 and id = $2", first_event).await.is_err(),
        "the database refuses edits"
    );
    refused(t.k.update(&acme.admin, "SignEvent", first_event, obj(json!({"details": "edited"})), None).await, "forbidden");
    refused(t.k.delete(&acme.admin, "SignEvent", first_event, None).await, "forbidden");
    refused(
        t.k.insert(&acme.admin, "SignEvent", obj(json!({"request": msa_id.to_string(), "seq": 99, "event": "Signed", "occurred_at": "2026-01-01T00:00:00Z"})))
            .await,
        "forbidden",
    );
    refused(t.k.update(&mira, "SignEvent", first_event, obj(json!({"details": "edited"})), None).await, "forbidden");

    // ───────────── parallel signing: concurrent signers and double submits ─────────────
    let nda =
        t.k.insert(
            &asha,
            "SignRequest",
            obj(json!({
                "title": "Mutual NDA", "document": t.upload(&asha, "nda.pdf", &contract_pdf("Mutual NDA")).await, "signing_mode": "Parallel",
                "signers": [{"role": "Party A", "name": "Ann Lee", "email": "ann@a.test"}, {"role": "Party B", "name": "Bo Park", "email": "bo@b.test"}],
                "fields": [field("Party A", "Signature", "A signs", 1, 72, 100, 200, 50), field("Party B", "Signature", "B signs", 1, 320, 100, 200, 50)],
            })),
        )
        .await
        .unwrap();
    let nda_id = uid(&nda);
    let sent = t.action(&asha, nda_id, "send", json!({})).await;
    let tokens: Vec<String> = sent["signers"].as_array().unwrap().iter().map(|l| token_of(l["url"].as_str().unwrap())).collect();
    assert_eq!(tokens.len(), 2, "parallel: everyone is asked at once");
    let body_a = json!({"consent": true, "signature": {"image": drawn(400, 120)}});
    let body_b = json!({"consent": true, "signature": {"text": "Bo Park"}});
    let (a1, a2, b) = tokio::join!(
        t.sign(&acme, &tokens[0], None, body_a.clone()),
        t.sign(&acme, &tokens[0], None, body_a.clone()),
        t.sign(&acme, &tokens[1], None, body_b.clone())
    );
    for r in [&a1, &a2, &b] {
        assert_eq!(r.as_ref().unwrap().0, 200, "{r:?}");
    }
    let nda = t.get(&asha, "SignRequest", nda_id).await;
    assert_eq!(nda["status"], json!("Completed"));
    let trail = t.trail(&acme, nda_id).await;
    assert_eq!(trail.iter().filter(|e| e.starts_with("Signed:Party A")).count(), 1, "the double submit signed once");
    assert_eq!(trail.iter().filter(|e| *e == "Completed").count(), 1, "sealed exactly once");
    let sealed_copies: i64 = t
        .scalar(&acme, "select count(*) from file_object f join e_sign_event e on e.tenant_id = f.tenant_id and e.id = f.record_id where f.tenant_id = $1 and e.request = $2 and f.file_name like '%signed.pdf'", nda_id)
        .await;
    assert_eq!(sealed_copies, 1);
    assert_eq!(t.action(&asha, nda_id, "verify", json!({})).await["valid"], json!(true));
    // Removing an event by hand breaks the chain, and verification says so.
    t.sql(&acme, "delete from e_sign_event where tenant_id = $1 and request = $2 and seq = 2", nda_id).await.unwrap();
    let broken = t.action(&asha, nda_id, "verify", json!({})).await;
    assert_eq!((broken["valid"].clone(), broken["audit_trail"]["problem"].clone()), (json!(false), json!("event 2 is missing")));

    // ───────────── decline ─────────────
    let one = |title: &str, mode: &str, signers: Value, fields: Value, doc: String| json!({"title": title, "document": doc, "signing_mode": mode, "signers": signers, "fields": fields});
    let two = json!([{"role": "A", "name": "Ann Lee", "email": "ann@a.test", "signing_order": 1}, {"role": "B", "name": "Bo Park", "email": "bo@b.test", "signing_order": 2}]);
    let two_fields = json!([field("A", "Signature", "A", 1, 72, 100, 200, 50), field("B", "Signature", "B", 1, 320, 100, 200, 50)]);
    let lease =
        t.k.insert(&asha, "SignRequest", obj(one("Lease", "Parallel", two.clone(), two_fields.clone(), t.upload(&asha, "lease.pdf", &pdf).await)))
            .await
            .unwrap();
    let lease_id = uid(&lease);
    let sent = t.action(&asha, lease_id, "send", json!({})).await;
    let lt: Vec<String> = sent["signers"].as_array().unwrap().iter().map(|l| token_of(l["url"].as_str().unwrap())).collect();
    refused(t.json(&acme, "POST", &format!("/signing/{}/decline", lt[0]), None, Some(json!({"reason": "  "}))).await, "invalid");
    let (status, declined) = t.json(&acme, "POST", &format!("/signing/{}/decline", lt[0]), None, Some(json!({"reason": "The rent is wrong"}))).await.unwrap();
    assert_eq!((status, declined["status"].clone()), (200, json!("Declined")));
    let (_, again) = t.json(&acme, "POST", &format!("/signing/{}/decline", lt[0]), None, Some(json!({"reason": "The rent is wrong"}))).await.unwrap();
    assert_eq!(again["replayed"], json!(true));
    let lease = t.get(&asha, "SignRequest", lease_id).await;
    assert_eq!((lease["status"].clone(), signer_row(&lease, "A")["decline_reason"].clone()), (json!("Declined"), json!("The rent is wrong")));
    let (status, gone) = t.sign(&acme, &lt[1], None, json!({"consent": true, "signature": {"text": "Bo Park"}})).await.unwrap();
    assert_eq!(status, 410, "the other signer can no longer sign");
    assert!(gone["message"].as_str().unwrap().contains("declined"));
    assert!(
        t.notifications(&acme, &asha).await.iter().any(|(title, body)| title.starts_with("Ann Lee declined") && body.as_deref() == Some("The rent is wrong"))
    );
    assert_eq!(t.trail(&acme, lease_id).await.last().map(String::as_str), Some("Declined:A"));
    refused(t.k.run_action(&asha, "SignRequest", lease_id, "void", json!({})).await, "invalid");

    // ───────────── void ─────────────
    // What the actions read, their pages ask for: one signer to resend to, and why it is voided.
    let asks = |action: &str| -> Vec<String> {
        let ent = asha.schema.entity("SignRequest").unwrap();
        ent.def.actions.iter().find(|a| a.name == action).unwrap().params.iter().map(|p| p.name.clone()).collect()
    };
    assert_eq!((asks("resend"), asks("void")), (vec!["signer".to_string()], vec!["reason".to_string()]));
    let loan =
        t.k.insert(&asha, "SignRequest", obj(one("Loan", "Sequential", two.clone(), two_fields.clone(), t.upload(&asha, "loan.pdf", &pdf).await)))
            .await
            .unwrap();
    let loan_id = uid(&loan);
    let sent = t.action(&asha, loan_id, "send", json!({})).await;
    let loan_token = token_of(sent["signers"][0]["url"].as_str().unwrap());
    refused(t.k.run_action(&ravi, "SignRequest", loan_id, "void", json!({})).await, "not found");
    assert_eq!(t.action(&asha, loan_id, "void", json!({"reason": "Terms changed"})).await["status"], json!("Voided"));
    let (status, gone) = t.view(&acme, &loan_token, None).await.unwrap();
    assert!(status == 410 && gone["message"].as_str().unwrap().contains("withdrew"));
    refused(t.k.run_action(&asha, "SignRequest", loan_id, "void", json!({})).await, "invalid");
    refused(t.k.run_action(&asha, "SignRequest", loan_id, "remind", json!({})).await, "invalid");
    assert_eq!(t.get(&asha, "SignRequest", loan_id).await["void_reason"], json!("Terms changed"));
    assert_eq!(t.trail(&acme, loan_id).await, vec!["Sent:A", "Voided"]);

    // ───────────── expiry ─────────────
    let quote =
        t.k.insert(&asha, "SignRequest", obj(one("Quote", "Parallel", two.clone(), two_fields.clone(), t.upload(&asha, "quote.pdf", &pdf).await)))
            .await
            .unwrap();
    let quote_id = uid(&quote);
    let sent = t.action(&asha, quote_id, "send", json!({})).await;
    let quote_token = token_of(sent["signers"][0]["url"].as_str().unwrap());
    t.sql(&acme, "update e_sign_request set expires_at = now() - interval '1 minute' where tenant_id = $1 and id = $2", quote_id).await.unwrap();
    let (status, gone) = t.view(&acme, &quote_token, None).await.unwrap();
    assert!(status == 410 && gone["message"].as_str().unwrap().contains("expired"), "an expired link is refused even before maintenance runs");
    t.maintenance(&acme).await;
    t.maintenance(&acme).await;
    assert_eq!(t.get(&asha, "SignRequest", quote_id).await["status"], json!("Expired"));
    assert_eq!(t.trail(&acme, quote_id).await.iter().filter(|e| *e == "Expired").count(), 1, "expired once");
    assert!(t.notifications(&acme, &asha).await.iter().any(|(title, _)| title.ends_with("expired before everyone signed")));

    // ───────────── reminders on cadence, each exactly once ─────────────
    let deal =
        t.k.insert(&asha, "SignRequest", obj(one("Deal", "Parallel", two.clone(), two_fields.clone(), t.upload(&asha, "deal.pdf", &pdf).await))).await.unwrap();
    let deal_id = uid(&deal);
    t.action(&asha, deal_id, "send", json!({})).await;
    t.maintenance(&acme).await;
    assert_eq!(t.count(&acme, deal_id, "Reminded").await, 0, "not due yet");
    t.sql(&acme, &format!("update {table} set sent_at = now() - interval '25 hours' where tenant_id = $1 and parent_id = $2"), deal_id).await.unwrap();
    let (r1, r2) = tokio::join!(ferrum_app_sign::maintenance(&t.k, acme.id), ferrum_app_sign::maintenance(&t.k, acme.id));
    r1.unwrap();
    r2.unwrap();
    t.maintenance(&acme).await;
    assert_eq!(t.count(&acme, deal_id, "Reminded").await, 2, "one reminder for each of the two signers, even with concurrent runs");
    let deal = t.get(&asha, "SignRequest", deal_id).await;
    assert_eq!((signer_row(&deal, "A")["reminders_sent"].clone(), signer_row(&deal, "B")["reminders_sent"].clone()), (json!(1), json!(1)));
    assert!(t.notifications(&acme, &asha).await.iter().any(|(title, _)| title == "Ann Lee has not signed yet" || title.starts_with("Ann Lee has not signed")));
    t.sql(&acme, &format!("update {table} set last_reminded_at = now() - interval '25 hours' where tenant_id = $1 and parent_id = $2"), deal_id).await.unwrap();
    t.maintenance(&acme).await;
    t.maintenance(&acme).await;
    assert_eq!(t.count(&acme, deal_id, "Reminded").await, 4, "the second round");
    t.sql(&acme, &format!("update {table} set last_reminded_at = now() - interval '25 hours' where tenant_id = $1 and parent_id = $2"), deal_id).await.unwrap();
    t.maintenance(&acme).await;
    assert_eq!(t.count(&acme, deal_id, "Reminded").await, 4, "no more than max_reminders (2) per signer");
    let manual = t.action(&asha, deal_id, "remind", json!({})).await;
    assert_eq!(manual["reminded"].as_array().unwrap().len(), 2, "the sender can still remind by hand");
    assert!(manual["reminded"][0]["url"].is_string(), "with the link to pass on, as there is no Mail");

    // ───────────── corrections and resending ─────────────
    let gift =
        t.k.insert(&asha, "SignRequest", obj(one("Gift Deed", "Sequential", two.clone(), two_fields.clone(), t.upload(&asha, "gift.pdf", &pdf).await)))
            .await
            .unwrap();
    let gift_id = uid(&gift);
    let sent = t.action(&asha, gift_id, "send", json!({})).await;
    let old_token = token_of(sent["signers"][0]["url"].as_str().unwrap());
    let gift = t.get(&asha, "SignRequest", gift_id).await;
    let mut rows = gift["signers"].clone();
    rows[0]["email"] = json!("ann.lee@a.test");
    t.k.update(&asha, "SignRequest", gift_id, obj(json!({"signers": rows})), None).await.expect("correct a signer's email");
    refused(t.view(&acme, &old_token, None).await, "not found");
    let resent = t.action(&asha, gift_id, "resend", json!({"signer": "A"})).await;
    let new_token = token_of(resent["resent"][0]["url"].as_str().unwrap());
    assert_ne!(new_token, old_token);
    assert_eq!(t.view(&acme, &new_token, None).await.unwrap().0, 200);
    assert!(refused(t.k.run_action(&asha, "SignRequest", gift_id, "resend", json!({"signer": "B"})).await, "invalid").contains("cannot sign now"));
    let trail = t.trail(&acme, gift_id).await;
    assert!(trail.contains(&"Corrected:A".to_string()) && trail.contains(&"Resent:A".to_string()));
    t.sign(&acme, &new_token, None, json!({"consent": true, "signature": {"text": "Ann Lee"}})).await.unwrap();
    let gift = t.get(&asha, "SignRequest", gift_id).await;
    let mut rows = gift["signers"].clone();
    rows[0]["email"] = json!("someone@else.test");
    assert!(refused(t.k.update(&asha, "SignRequest", gift_id, obj(json!({"signers": rows})), None).await, "invalid").contains("already signed"));

    // ───────────── templates ─────────────
    let template_pdf = t.upload(&mira, "service.pdf", &contract_pdf("Service Agreement")).await;
    let roles = json!([{"role": "Client", "signing_order": 1}, {"role": "Company", "signing_order": 2, "default_name": "Ravi Kumar", "default_email": "ravi@acme.test"}]);
    let tfields =
        json!([field("Client", "Signature", "Client signs", 1, 72, 600, 200, 50), field("Company", "Signature", "Company signs", 2, 72, 400, 200, 50)]);
    let mut bad = tfields.clone();
    bad[1]["y"] = json!(590);
    refused(
        t.k.insert(&mira, "SignTemplate", obj(json!({"template_name": "Service", "document": template_pdf, "roles": roles, "fields": bad}))).await,
        "invalid",
    );
    let template = t.k.insert(&mira, "SignTemplate", obj(json!({"template_name": "Service", "title": "Service Agreement", "document": template_pdf, "roles": roles, "fields": tfields, "expiry_days": 7}))).await.expect("template");
    refused(t.k.insert(&asha, "SignTemplate", obj(json!({"template_name": "Mine", "document": template_pdf, "roles": roles}))).await, "forbidden");
    let made =
        t.k.run_action(
            &asha,
            "SignTemplate",
            uid(&template),
            "create_request",
            json!({"signers": [{"role": "client", "name": "Jane Doe", "email": "jane@client.test"}]}),
        )
        .await
        .expect("create from template");
    assert_eq!(made["entity"], json!("SignRequest"));
    let from_template = t.get(&asha, "SignRequest", made["id"].as_str().unwrap().parse().unwrap()).await;
    assert_eq!(
        (from_template["title"].clone(), from_template["template"].clone(), from_template["status"].clone()),
        (json!("Service Agreement"), template["id"].clone(), json!("Draft"))
    );
    assert_eq!(from_template["fields"].as_array().unwrap().len(), 2);
    assert_eq!(
        (signer_row(&from_template, "Client")["email"].clone(), signer_row(&from_template, "Company")["email"].clone()),
        (json!("jane@client.test"), json!("ravi@acme.test"))
    );
    assert_ne!(from_template["document"], template["document"], "the request has its own copy of the PDF");
    assert_eq!(from_template["document_sha256"], template["document_sha256"]);
    assert_eq!(from_template["created_by"], json!(asha.user.to_string()), "the request is the caller's");
    let sent = t.action(&asha, uid(&from_template), "send", json!({})).await;
    assert_eq!(
        t.get(&asha, "SignRequest", uid(&from_template)).await["expires_on"],
        json!((today + chrono::Days::new(7)).to_string()),
        "the template's expiry"
    );
    assert_eq!(sent["signers"].as_array().unwrap().len(), 1);
    refused(t.k.run_action(&nobody, "SignTemplate", uid(&template), "create_request", json!({})).await, "forbidden");
    assert!(
        refused(t.k.run_action(&asha, "SignTemplate", uid(&template), "create_request", json!({"signers": [{"role": "Witness"}]})).await, "invalid")
            .contains("no role")
    );
    t.k.update(&mira, "SignTemplate", uid(&template), obj(json!({"disabled": true})), None).await.unwrap();
    refused(t.k.run_action(&asha, "SignTemplate", uid(&template), "create_request", json!({})).await, "invalid");

    // ───────────── permissions: senders see their own requests ─────────────
    refused(t.k.get(&ravi, "SignRequest", msa_id, false).await, "not found");
    refused(t.k.run_action(&ravi, "SignRequest", msa_id, "verify", json!({})).await, "not found");
    refused(t.k.read_file(&ravi, sealed_file).await, "not found");
    assert!(t.list(&ravi, "SignRequest", json!({})).await.is_empty());
    assert!(t.list(&ravi, "SignEvent", json!({})).await.is_empty(), "nor their audit trails");
    let own = t.k.insert(&ravi, "SignRequest", obj(json!({"title": "Ravi's own", "document": ravis}))).await.expect("ravi drafts with his own upload");
    assert_eq!(t.list(&ravi, "SignRequest", json!({})).await.len(), 1);
    assert!(t.list(&asha, "SignRequest", json!({})).await.iter().all(|r| r["id"] != own["id"]));
    assert_eq!(t.list(&mira, "SignRequest", json!({})).await.len(), 9, "the manager sees every request");
    assert!(!t.list(&asha, "SignEvent", json!({"request": msa_id.to_string()})).await.is_empty(), "the sender reads her trail");
    refused(t.k.run_action(&asha, "SignSettings", uid(&t.list(&mira, "SignSettings", json!({})).await[0]), "rotate_signing_key", json!({})).await, "forbidden");
    refused(t.k.list(&nobody, "SignRequest", &ListQuery::default()).await.map(|p| p.rows.len()), "forbidden");
    let linked =
        t.k.insert(&asha, "SignRequest", obj(json!({"title": "Linked", "document": document, "reference_entity": "SignRequest", "reference_id": own["id"]})))
            .await;
    assert!(refused(linked, "invalid").contains("cannot read it"), "a request can only link records its sender can read");

    // ───────────── reports ─────────────
    let by_status = t.report(&asha, "sign_requests_by_status", &[]).await;
    let count = |r: &Value, status: &str| r["rows"].as_array().unwrap().iter().find(|x| x["status"] == json!(status)).unwrap()["requests"].clone();
    assert_eq!(
        ["Completed", "Declined", "Voided", "Expired", "Sent", "In Progress", "Draft"].map(|s| count(&by_status, s)),
        [json!(2), json!(1), json!(1), json!(1), json!(2), json!(1), json!(0)]
    );
    assert_eq!(by_status["totals"]["requests"], json!(8), "asha's requests only");
    let ravis_view = t.report(&ravi, "sign_requests_by_status", &[]).await;
    assert_eq!((ravis_view["totals"]["requests"].clone(), count(&ravis_view, "Draft")), (json!(1), json!(1)));
    assert_eq!(t.report(&mira, "sign_requests_by_status", &[]).await["totals"]["requests"], json!(9));
    let times = t.report(&asha, "sign_completion_time", &[("group_by", "Template")]).await;
    assert_eq!(times["totals"]["completed"], json!(2));
    assert!(times["totals"]["average_hours"].as_str().unwrap().parse::<f64>().unwrap() >= 0.0);
    let pending = t.report(&asha, "sign_pending_signatures", &[]).await;
    let waiting: HashMap<String, i64> =
        pending["rows"].as_array().unwrap().iter().map(|r| (r["signer"].as_str().unwrap().to_string(), r["pending"].as_i64().unwrap())).collect();
    assert_eq!(waiting.get("ann@a.test"), Some(&1), "Deal");
    assert_eq!(waiting.get("bo@b.test"), Some(&2), "Deal and Gift Deed");
    assert_eq!(waiting.get("jane@client.test"), Some(&1), "the request from the template");
    refused(ferrum_domain::reports::run(&t.k, &nobody, "sign_pending_signatures", &HashMap::new()).await, "forbidden");

    // ───────────── identity by a one-time code (DC-40) ─────────────
    // Codes are sealed on their way out; a test reads them from the platform's test channel.
    let channel: ferrum_kernel::messages::ChannelInput =
        serde_json::from_value(json!({"kind": "text", "name": "Test", "description": {"test": true}})).unwrap();
    t.k.save_message_channel(&acme.admin, None, &channel).await.expect("test channel");
    let by_text = json!({"title": "Guarantee", "document": t.upload(&asha, "g.pdf", &pdf).await,
        "signers": [{"role": "Guarantor", "name": "Gus", "email": "gus@loan.test", "verify_identity": "Text Code"}],
        "fields": [field("Guarantor", "Signature", "G", 1, 72, 600, 200, 50)]});
    let by_text = uid(&t.k.insert(&asha, "SignRequest", obj(by_text)).await.unwrap());
    assert!(refused(t.k.run_action(&asha, "SignRequest", by_text, "send", json!({})).await, "invalid").contains("code by text message"));
    let loan = json!({"title": "Loan agreement", "document": t.upload(&asha, "loan.pdf", &pdf).await, "verify_identity": "Text Code",
        "signers": [{"role": "Borrower", "name": "Bea Borrower", "email": "bea@loan.test", "phone": "+4917011111111"}],
        "fields": [field("Borrower", "Signature", "B", 1, 72, 600, 200, 50)]});
    let loan_id = uid(&t.k.insert(&asha, "SignRequest", obj(loan)).await.unwrap());
    let sent = t.action(&asha, loan_id, "send", json!({})).await;
    let bea = token_of(sent["signers"][0]["url"].as_str().unwrap());
    let (_, seen) = t.view(&acme, &bea, None).await.unwrap();
    assert_eq!(seen["identity"], json!({"code": "text", "verified": false}));
    let signs = json!({"consent": true, "signature": {"text": "Bea Borrower"}});
    assert!(refused(t.sign(&acme, &bea, None, signs.clone()).await, "forbidden").contains("confirm the code"));
    let (status, said) = t.json(&acme, "POST", &format!("/signing/{bea}/code"), None, None).await.unwrap();
    assert_eq!((status, said["channel"].as_str()), (200, Some("text")));
    let code = texted_code(&t, &acme, "+4917011111111").await;
    let wrong = if code == "000000" { "111111" } else { "000000" };
    refused(t.json(&acme, "POST", &format!("/signing/{bea}/verify"), None, Some(json!({"code": wrong}))).await, "forbidden");
    let (_, ok) = t.json(&acme, "POST", &format!("/signing/{bea}/verify"), None, Some(json!({"code": code}))).await.unwrap();
    assert_eq!(ok["verified"], json!(true));
    let (_, done) = t.sign(&acme, &bea, None, signs).await.unwrap();
    assert_eq!(done["request_status"], json!("Completed"));
    let trail = t.trail(&acme, loan_id).await;
    assert!(trail.iter().any(|e| e == "Code Sent:Borrower") && trail.iter().any(|e| e == "Identity Verified:Borrower"), "{trail:?}");
    assert_eq!(signer_row(&t.get(&asha, "SignRequest", loan_id).await, "Borrower")["identity_channel"], json!("text"));

    // ───────────── the one-time code through the public routes: expired, too many tries, once only ─────────────
    let signs = json!({"consent": true, "signature": {"text": "Signer"}});
    let verify = |token: &str, code: &str| {
        let (t, acme, path, body) = (&t, &acme, format!("/signing/{token}/verify"), json!({"code": code}));
        async move { t.json(acme, "POST", &path, None, Some(body)).await }
    };
    // Expired: a correct code that has outlived its ten minutes is refused, and the signer may not sign.
    let (_, eve) = code_request(&t, &asha, &pdf, "eve", "+4917022222222").await;
    let late = mailed_code(&t, &acme, &eve, "+4917022222222").await;
    let mut tx = t.k.db.tenant_tx(acme.id).await.unwrap();
    sqlx::query("update message_code set expires_at = now() - interval '1 minute' where tenant_id = $1 and recipient = '+4917022222222'")
        .bind(acme.id)
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert!(refused(verify(&eve, &late).await, "forbidden").contains("no longer works"));
    assert!(refused(t.sign(&acme, &eve, None, signs.clone()).await, "forbidden").contains("confirm the code"));
    // Too many tries: five wrong codes use the code up; the right one is refused after them, a new one works.
    let (_, tom) = code_request(&t, &asha, &pdf, "tom", "+4917033333333").await;
    let right = mailed_code(&t, &acme, &tom, "+4917033333333").await;
    let wrong = if right == "000000" { "111111" } else { "000000" };
    for _ in 0..5 {
        refused(verify(&tom, wrong).await, "forbidden");
    }
    assert!(refused(verify(&tom, &right).await, "forbidden").contains("no longer works"), "the sixth try fails even with the right code");
    assert!(refused(t.sign(&acme, &tom, None, signs.clone()).await, "forbidden").contains("confirm the code"));
    let fresh = mailed_code(&t, &acme, &tom, "+4917033333333").await;
    assert_eq!(verify(&tom, &fresh).await.unwrap().1["verified"], json!(true), "a new code starts again");
    // Once: a code that worked does not work again (here for a signer who is already confirmed).
    assert!(refused(verify(&tom, &fresh).await, "forbidden").contains("no longer works"));

    // ───────────── a group, a copy, and a signature handed over (DC-38, DC-45, DC-46) ─────────────
    let board = json!({"title": "Board consent", "document": t.upload(&asha, "board.pdf", &pdf).await, "allow_reassign": true,
        "signers": [
            {"role": "Director A", "name": "Dee", "email": "dee@board.test", "signing_order": 1, "signing_group": "Directors"},
            {"role": "Director B", "name": "Dan", "email": "dan@board.test", "signing_order": 1, "signing_group": "Directors"},
            {"role": "Secretary", "name": "Sec", "email": "sec@board.test", "signing_order": 2},
            {"role": "Auditor", "name": "Aud", "email": "aud@board.test", "action": "Receive a Copy"}],
        "fields": [field("Director A", "Signature", "DA", 1, 72, 100, 200, 50), field("Director B", "Signature", "DB", 1, 320, 100, 200, 50),
                   field("Secretary", "Signature", "S", 1, 72, 300, 200, 50)]});
    let board_id = uid(&t.k.insert(&asha, "SignRequest", obj(board)).await.unwrap());
    let sent = t.action(&asha, board_id, "send", json!({})).await;
    let asked: Vec<&str> = sent["signers"].as_array().unwrap().iter().map(|l| l["role"].as_str().unwrap()).collect();
    assert_eq!(asked, ["Director A", "Director B"], "the group together; the copy is never asked to sign");
    let link = |role: &str| token_of(sent["signers"].as_array().unwrap().iter().find(|l| l["role"] == json!(role)).unwrap()["url"].as_str().unwrap());
    let (dee, dan) = (link("Director A"), link("Director B"));
    let handover = json!({"name": "Dora", "email": "dora@board.test", "reason": "Dan is away"});
    let (_, moved) = t.json(&acme, "POST", &format!("/signing/{dan}/reassign"), None, Some(handover)).await.unwrap();
    assert_eq!(moved["reassigned"], json!(true));
    refused(t.view(&acme, &dan, None).await, "not found");
    let b = signer_row(&t.get(&asha, "SignRequest", board_id).await, "Director B");
    assert_eq!(b["email"], json!("dora@board.test"));
    assert!(b["reassigned_from"].as_str().unwrap().contains("Dan <dan@board.test>: Dan is away"));
    t.sign(&acme, &dee, None, json!({"consent": true, "signature": {"text": "Dee"}})).await.unwrap();
    let after = t.get(&asha, "SignRequest", board_id).await;
    assert_eq!((signer_row(&after, "Director B")["status"].clone(), signer_row(&after, "Secretary")["status"].clone()), (json!("Not Needed"), json!("Sent")));
    let key = t.key(&acme).await;
    let sec = token_for(&key, acme.id, board_id, uid(&signer_row(&after, "Secretary")), "sec@board.test");
    let (_, done) = t.sign(&acme, &sec, None, json!({"consent": true, "signature": {"text": "Sec"}})).await.unwrap();
    assert_eq!(done["request_status"], json!("Completed"), "the group signed once, the secretary after");
    assert!(t.trail(&acme, board_id).await.iter().any(|e| e == "Reassigned:Director B" || e == "Not Needed:Director B"));
    assert_eq!(signer_row(&t.get(&asha, "SignRequest", board_id).await, "Auditor")["signed_at"], Value::Null);

    // ───────────── a request's own reminders (DC-49) ─────────────
    let quiet = json!({"title": "Quiet", "document": t.upload(&asha, "q.pdf", &pdf).await, "reminder_every_days": 0,
        "signers": [{"role": "Q", "name": "Quinn", "email": "quinn@q.test"}], "fields": [field("Q", "Signature", "Q", 1, 72, 600, 200, 50)]});
    let quiet_id = uid(&t.k.insert(&asha, "SignRequest", obj(quiet)).await.unwrap());
    t.action(&asha, quiet_id, "send", json!({})).await;
    t.sql(&acme, "update e_sign_request__signers set sent_at = now() - interval '5 days' where tenant_id = $1 and parent_id = $2", quiet_id).await.unwrap();
    t.maintenance(&acme).await;
    assert_eq!(t.count(&acme, quiet_id, "Reminded").await, 0, "its own setting: no reminders, though the workspace reminds daily");

    // ───────────── a workspace's own record sent for signature by configuration only (DC-47) ─────────────
    let quote = json!({"name": "XQuote", "label": "Quote", "kind": "master", "title_field": "title", "fields": [
        {"name": "title", "type": "data", "required": true}, {"name": "customer", "type": "data"}, {"name": "customer_email", "type": "email"},
        {"name": "status", "type": "select", "options": ["Open", "Out for Signature", "Signed"], "default": "Open"}]});
    let send_it = json!({"name": "x_send_to_sign", "label": "Send to Sign", "permission": "write", "effects": [
        {"type": "generate", "template": "x_quote.letter"},
        {"type": "invoke", "capability": "sign.sign_request.create", "input": {"values":
            "{'title': doc.title, 'document': results[0].url, 'source_sha256': results[0].sha256, 'send_on_create': true, 'reference_entity': 'XQuote', \
              'reference_id': doc.id, 'verify_identity': 'None', 'signers': [{'role': 'Customer', 'name': doc.customer, 'email': doc.customer_email}]}"}},
        {"type": "set_fields", "values": {"status": "'Out for Signature'"}}]});
    let custom = |op: Value| -> ferrum_kernel::customize::NewCustomization { serde_json::from_value(json!({"entity": "XQuote", "op": op})).unwrap() };
    t.k.batch_customizations(&acme.admin, vec![custom(json!({"op": "define_entity", "entity": quote}))], false).await.map_err(|(_, e)| e).expect("the entity");
    // The administrator again, with the workspace's schema as it now is.
    let session = t.k.login("acme", "admin@acme.test", PASSWORD).await.expect("login");
    let admin = t.k.load_ctx(acme.id, session.user, Some(session.token_version), "it".into()).await.expect("ctx");
    let letter: ferrum_meta::DocumentDef =
        serde_json::from_value(json!({"key": "x_quote.letter", "label": "Quote letter", "entity": "XQuote", "output": "page",
        "languages": {"en": {"body": "<h1>{{ doc.title }}</h1><p>For {{ doc.customer }}</p>"}}}))
        .unwrap();
    t.k.save_document_template(&admin, &letter).await.expect("its letter");
    t.k.batch_customizations(&admin, vec![custom(json!({"op": "add_action", "action": send_it}))], false).await.map_err(|(_, e)| e).expect("its button");
    let session = t.k.login("acme", "admin@acme.test", PASSWORD).await.expect("login");
    let admin = t.k.load_ctx(acme.id, session.user, Some(session.token_version), "it".into()).await.expect("ctx");
    ferrum_kernel::automation::save(
        &t.k,
        &admin,
        &obj(json!({"name": "Quotes signed", "entity": "SignRequest", "event": "updated",
            "condition": "doc.status == 'Completed' && doc.reference_entity == 'XQuote'",
            "actions": [{"type": "find", "entity": "XQuote", "by": {"id": "doc.reference_id"}, "update": {"status": "'Signed'"}}]})),
    )
    .await
    .expect("the record moves when it is signed");
    let q = t.k.insert(&admin, "XQuote", obj(json!({"title": "Quote Q-7", "customer": "Cora Client", "customer_email": "cora@client.test"}))).await.unwrap();
    let q_id = uid(&q);
    t.k.run_action(&admin, "XQuote", q_id, "x_send_to_sign", Value::Null).await.expect("sent by its button");
    // Its file was generated in the button's own transaction: upkeep adds the page and sends it once that commits.
    t.maintenance(&acme).await;
    assert_eq!(t.get(&admin, "XQuote", q_id).await["status"], json!("Out for Signature"));
    let made = t.list(&mira, "SignRequest", json!({"reference_entity": "XQuote", "reference_id": q_id.to_string()})).await;
    assert_eq!(made.len(), 1);
    let req = t.get(&mira, "SignRequest", uid(&made[0])).await;
    assert_eq!(
        (req["status"].clone(), req["page_count"].clone(), req["signature_page"].clone()),
        (json!("Sent"), json!(3), json!(3)),
        "a signature page added"
    );
    assert_eq!(req["fields"].as_array().unwrap().iter().map(|f| (f["role"].clone(), f["page"].clone())).collect::<Vec<_>>(), [(json!("Customer"), json!(3))]);
    let cora = token_for(&key, acme.id, uid(&req), uid(&signer_row(&req, "Customer")), "cora@client.test");
    let (_, done) = t.sign(&acme, &cora, None, json!({"consent": true, "signature": {"text": "Cora Client"}})).await.unwrap();
    assert_eq!(done["request_status"], json!("Completed"));
    let events: i64 =
        t.scalar(&acme, "select count(*) from outbox where tenant_id = $1 and topic = 'sign.request_completed' and record_id = $2", uid(&req)).await;
    assert_eq!(events, 1, "the event carries the record it is about");
    while t.k.dispatch_outbox_once().await.unwrap_or(0) > 0 {}
    assert_eq!(t.get(&admin, "XQuote", q_id).await["status"], json!("Signed"), "moved by the workspace's automation");
    // The digest names the file generated: another file is refused.
    let other = json!({"title": "Swapped", "document": t.upload(&asha, "x.pdf", &pdf).await, "source_sha256": "00".repeat(32), "send_on_create": true,
        "signers": [{"role": "C", "name": "C", "email": "c@x.test"}]});
    assert!(refused(t.k.insert(&asha, "SignRequest", obj(other)).await, "invalid").contains("not the one generated"));
}

#[test]
fn a_page_where_there_is_no_pdf_service_cannot_start_a_signature() {
    let _db = DB.lock().unwrap_or_else(|e| e.into_inner());
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().thread_stack_size(16 << 20).build().unwrap();
    rt.block_on(async { tokio::spawn(no_pdf_service()).await.unwrap() });
}

/// F4 of the second walk: a button that generates a document, sends it for signature and sets the record's status
/// on a server with no PDF service. The page cannot be signed, so the press is refused whole: the record keeps its
/// status, and no request, file or "sent" state is left behind.
async fn no_pdf_service() {
    let t = boot(KernelSettings::default()).await;
    let acme = t.tenant("acme", &["sign"], "UTC").await;
    let quote = json!({"name": "XQuote", "label": "Quote", "kind": "master", "title_field": "title", "fields": [
        {"name": "title", "type": "data", "required": true}, {"name": "customer", "type": "data"}, {"name": "customer_email", "type": "email"},
        {"name": "status", "type": "select", "options": ["Open", "Out for Signature", "Signed"], "default": "Open"}]});
    let send_it = json!({"name": "x_send_to_sign", "label": "Send to Sign", "permission": "write", "effects": [
        {"type": "generate", "template": "x_quote.letter"},
        {"type": "invoke", "capability": "sign.sign_request.create", "input": {"values":
            "{'title': doc.title, 'document': results[0].url, 'source_sha256': results[0].sha256, 'send_on_create': true, 'reference_entity': 'XQuote', \
              'reference_id': doc.id, 'signers': [{'role': 'Customer', 'name': doc.customer, 'email': doc.customer_email}]}"}},
        {"type": "set_fields", "values": {"status": "'Out for Signature'"}}]});
    let custom = |op: Value| -> ferrum_kernel::customize::NewCustomization { serde_json::from_value(json!({"entity": "XQuote", "op": op})).unwrap() };
    t.k.batch_customizations(&acme.admin, vec![custom(json!({"op": "define_entity", "entity": quote}))], false).await.map_err(|(_, e)| e).expect("the entity");
    let session = t.k.login("acme", "admin@acme.test", PASSWORD).await.expect("login");
    let admin = t.k.load_ctx(acme.id, session.user, Some(session.token_version), "it".into()).await.expect("ctx");
    let letter: ferrum_meta::DocumentDef =
        serde_json::from_value(json!({"key": "x_quote.letter", "label": "Quote letter", "entity": "XQuote", "output": "page",
        "languages": {"en": {"body": "<h1>{{ doc.title }}</h1><p>For {{ doc.customer }}</p>"}}}))
        .unwrap();
    t.k.save_document_template(&admin, &letter).await.expect("its letter");
    t.k.batch_customizations(&admin, vec![custom(json!({"op": "add_action", "action": send_it}))], false).await.map_err(|(_, e)| e).expect("its button");
    let session = t.k.login("acme", "admin@acme.test", PASSWORD).await.expect("login");
    let admin = t.k.load_ctx(acme.id, session.user, Some(session.token_version), "it".into()).await.expect("ctx");
    let q = t.k.insert(&admin, "XQuote", obj(json!({"title": "Quote Q-7", "customer": "Cora Client", "customer_email": "cora@client.test"}))).await.unwrap();
    let q_id = uid(&q);
    let said = refused(t.k.run_action(&admin, "XQuote", q_id, "x_send_to_sign", Value::Null).await, "invalid");
    assert!(said.contains("FERRUM_PDF_URL"), "the person is told which service is missing: {said}");
    t.maintenance(&acme).await;
    assert_eq!(t.get(&admin, "XQuote", q_id).await["status"], json!("Open"), "the record stays where it was");
    assert!(t.list(&t.sys(&acme), "SignRequest", json!({})).await.is_empty(), "no orphan request");
    let files: i64 = t.scalar(&acme, "select count(*) from file_object where tenant_id = $1 and record_id = $2", q_id).await;
    assert_eq!(files, 0, "no generated file kept for a signature that was not sent");
}

#[test]
fn a_signers_link_goes_through_the_link_host() {
    let _db = DB.lock().unwrap_or_else(|e| e.into_inner());
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().thread_stack_size(16 << 20).build().unwrap();
    rt.block_on(async { tokio::spawn(link_host()).await.unwrap() });
}

/// A signer is someone outside the workspace. With no address of Sign's (nor Mail's), the link they are given is the
/// platform's for people outside: through the operator's link host (`/go/{workspace}/p`), so it keeps working when the
/// workspace's address changes, to the signing endpoints, which every host of the workspace serves. With an address of
/// Sign's own, it is as before; with none at all, it is relative, as `sign_end_to_end` shows.
async fn link_host() {
    let settings = KernelSettings { public_url: Some("https://erp.acme.test".into()), link_host: Some("go.acme.test".into()), ..Default::default() };
    let t = boot(settings).await;
    let acme = t.tenant("acme", &["sign"], "UTC").await;
    let asha = t.user(&acme, "asha@acme.test", "Asha Rao", &["Sign User"]).await;
    let mira = t.user(&acme, "mira@acme.test", "Mira Chen", &["Sign Manager"]).await;
    let pdf = contract_pdf("Order form");
    let send = |title: &'static str| {
        let (t, asha, pdf) = (&t, asha.clone(), pdf.clone());
        async move {
            let request = json!({"title": title, "document": t.upload(&asha, "order.pdf", &pdf).await,
                "signers": [{"role": "Client", "name": "Jane Doe", "email": "jane@client.test"}],
                "fields": [field("Client", "Signature", "Client signature", 1, 72, 600, 200, 50)]});
            let id = uid(&t.k.insert(&asha, "SignRequest", obj(request)).await.unwrap());
            t.action(&asha, id, "send", json!({})).await["signers"][0]["url"].as_str().unwrap().to_string()
        }
    };
    let link = send("Order").await;
    assert!(link.starts_with("https://go.acme.test/go/acme/p/api/site/acme/sign/s/"), "{link}");
    let token = token_of(&link);
    assert!(t.view(&acme, &token, None).await.unwrap().0 == 200, "the link's token is the signer's");
    // Sending made the settings (their signing key); an address of Sign's own goes in them.
    let settings = t.list(&mira, "SignSettings", json!({})).await;
    t.k.update(&mira, "SignSettings", uid(&settings[0]), obj(json!({"public_url": "https://sign.acme.test/"})), None).await.unwrap();
    let link = send("Another order").await;
    assert!(link.starts_with("https://sign.acme.test/api/site/acme/sign/s/"), "{link}");
}

/// A kernel with Sign alone on the suite's database, emptied first.
async fn boot(settings: KernelSettings) -> T {
    boot_with(settings, vec![]).await
}

/// A package of a test's own, from its files.
fn package(files: &[(&str, &str)]) -> ferrum_meta::Package {
    let files: std::collections::BTreeMap<String, String> = files.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
    ferrum_meta::catalog::load_package_files(&files).expect("the test's package")
}

/// [`boot`] with packages of the test's own beside Sign's, and Sign's reports.
async fn boot_with(settings: KernelSettings, extra: Vec<ferrum_meta::Package>) -> T {
    boot_mailed(settings, extra, None).await
}

/// [`boot_with`] on a server that has a mailer.
async fn boot_mailed(settings: KernelSettings, extra: Vec<ferrum_meta::Package>, mail: Option<Arc<TestMailer>>) -> T {
    let url = std::env::var("FERRUM_SIGN_TEST_DATABASE_URL").unwrap_or_else(|_| "postgres://ferrum:ferrum@localhost:5432/ferrum_sign_test".into());
    let db = Db::connect(DbConfig { url: url.clone(), max_connections: 16, ..Default::default() }).await.expect("connect");
    sqlx::raw_sql("drop schema if exists vault cascade; drop schema if exists public cascade; create schema public;").execute(&db.pool).await.expect("reset");
    ferrum_kernel::vault::open(&db, opener(&url), false).await.expect("the vault");
    let mut packages = ferrum_domain::packages().expect("platform packages");
    packages.extend(ferrum_app_sign::packages().expect("app packages"));
    let names: Vec<String> = extra.iter().map(|p| p.name().to_string()).collect();
    packages.extend(extra);
    let mut hooks = HookRegistry::default();
    ferrum_domain::register(&mut hooks);
    let sign_packages: Vec<String> = ferrum_app_sign::packages().unwrap().iter().map(|p| p.name().to_string()).collect();
    let mut sign = None;
    hooks.app("sign", &sign_packages, |h| {
        ferrum_app_sign::register(h);
        sign = Some(h.vault());
    });
    hooks.own("sign", &sign_packages);
    let mut apps = HashMap::new();
    for name in names {
        // An app's id has dashes where its package's name has underscores.
        let id = name.replace('_', "-");
        hooks.app(&id, std::slice::from_ref(&name), |h| {
            apps.insert(name.clone(), h.vault());
        });
        hooks.own(&id, std::slice::from_ref(&name));
    }
    let reports = ferrum_app_sign::reports().into_iter().map(|r| ("sign".to_string(), r)).collect();
    ferrum_domain::reports::set_sources(reports).expect("Sign's reports");
    let mailer = mail.clone().map(|m| m as Arc<dyn ferrum_kernel::Mailer>);
    let k = Kernel::boot(db, Catalog::from_packages(packages).expect("catalog"), hooks, settings, mailer).await.expect("boot");
    let store = Arc::new(MemStore::default());
    k.set_file_store(store.clone());
    T { k, store, sign: sign.expect("Sign's handle"), apps }
}

/// K0-7 (S9): a role that reads Sign Requests but not some of their fields. Reports group, filter and aggregate only on
/// what the caller may read: a grouping on a hidden field is refused, and a report whose figures come from a hidden field
/// is refused rather than answered.
const AUDIT: &str = "package: sign_audit\nversion: 0.1.0\ndepends: [sign]\nroles:\n  - name: Sign Auditor\n    permissions:\n      \
- {entity: SignRequest, read: true, hide: [sender_email]}\n      - {entity: SignTemplate, read: true}\n  - name: Sign Clerk\n    permissions:\n      \
- {entity: SignRequest, read: true, hide: [completed_at, signers]}\n";

#[test]
fn a_report_refuses_grouping_by_a_field_the_caller_cannot_read() {
    run(reports_by_readable_fields());
}

async fn reports_by_readable_fields() {
    let t = boot_with(KernelSettings::default(), vec![package(&[("package.yaml", AUDIT)])]).await;
    let acme = t.tenant("acme", &["sign", "sign_audit"], "UTC").await;
    let asha = t.user(&acme, "asha@acme.test", "Asha Rao", &["Sign User"]).await;
    let auditor = t.user(&acme, "audit@acme.test", "Ari Auditor", &["Sign Auditor"]).await;
    let clerk = t.user(&acme, "clerk@acme.test", "Cy Clerk", &["Sign Clerk"]).await;
    let pdf = contract_pdf("Report");
    let draft = json!({"title": "Reported", "document": t.upload(&asha, "r.pdf", &pdf).await,
        "signers": [{"role": "Client", "name": "Cleo", "email": "cleo@client.test"}], "fields": [field("Client", "Signature", "C", 1, 72, 600, 200, 50)]});
    let id = uid(&t.k.insert(&asha, "SignRequest", obj(draft)).await.unwrap());
    let sent = t.action(&asha, id, "send", json!({})).await;
    t.sign(&acme, &token_of(sent["signers"][0]["url"].as_str().unwrap()), None, json!({"consent": true, "signature": {"text": "Cleo"}})).await.unwrap();
    let run = |ctx: Ctx, name: &'static str, params: Vec<(&'static str, &'static str)>| {
        let t = &t;
        async move {
            let p: HashMap<String, String> = params.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
            ferrum_domain::reports::run(&t.k, &ctx, name, &p).await
        }
    };
    // The auditor reads requests but not who sent them: no grouping by sender; by month and template, yes.
    let m = refused(run(auditor.clone(), "sign_completion_time", vec![("group_by", "Sender")]).await, "forbidden");
    assert!(m.contains("sender_email"), "{m}");
    assert_eq!(run(auditor.clone(), "sign_completion_time", vec![]).await.expect("by month")["totals"]["completed"], json!(1));
    assert_eq!(run(auditor.clone(), "sign_completion_time", vec![("group_by", "Template")]).await.expect("by template")["rows"][0]["completed"], json!(1));
    // The clerk cannot read when requests completed, nor their signers: the reports made of those are refused, not
    // answered with figures computed from them; the count by status, which reads neither, is answered.
    assert!(refused(run(clerk.clone(), "sign_completion_time", vec![]).await, "forbidden").contains("completed_at"));
    assert!(refused(run(clerk.clone(), "sign_pending_signatures", vec![]).await, "forbidden").contains("signers"));
    let counts = run(clerk.clone(), "sign_requests_by_status", vec![]).await.expect("by status");
    assert_eq!(counts["totals"]["requests"], json!(1));
}

fn run(scenario: impl std::future::Future<Output = ()> + Send + 'static) {
    let _db = DB.lock().unwrap_or_else(|e| e.into_inner());
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().thread_stack_size(16 << 20).build().unwrap();
    rt.block_on(async { tokio::spawn(scenario).await.unwrap() });
}

#[test]
fn a_failed_seal_is_retried_with_a_copy_recipient_and_a_group() {
    run(seal_retries());
}

/// sign-1: a request whose seal failed is retried by upkeep until it is sealed, once, whoever else is on it (a
/// recipient of a copy never signs; a group's other members are Not Needed). While it fails, a person is told why
/// (the sender, or the Sign Managers for a request no person made), once, and the reason stays on the request. And no
/// request is ever Completed without its seal: the database refuses it.
async fn seal_retries() {
    let t = boot(KernelSettings::default()).await;
    let acme = t.tenant("acme", &["sign"], "UTC").await;
    let asha = t.user(&acme, "asha@acme.test", "Asha Rao", &["Sign User"]).await;
    let mira = t.user(&acme, "mira@acme.test", "Mira Chen", &["Sign Manager"]).await;
    let pdf = contract_pdf("Retry");
    let backdate = |id: Uuid| {
        let t = &t;
        let acme = &acme;
        async move {
            t.sql(acme, "update e_sign_request set updated_at = now() - interval '5 minutes' where tenant_id = $1 and id = $2", id).await.unwrap();
        }
    };
    let copy = json!({"title": "With a copy", "signers": [
            {"role": "Client", "name": "Cleo", "email": "cleo@client.test"},
            {"role": "Auditor", "name": "Aud", "email": "aud@audit.test", "action": "Receive a Copy"}],
        "fields": [field("Client", "Signature", "C", 1, 72, 600, 200, 50)]});
    let group = json!({"title": "With a group", "signers": [
            {"role": "Director A", "name": "Dee", "email": "dee@board.test", "signing_group": "Directors"},
            {"role": "Director B", "name": "Dan", "email": "dan@board.test", "signing_group": "Directors"}],
        "fields": [field("Director A", "Signature", "DA", 1, 72, 100, 200, 50), field("Director B", "Signature", "DB", 1, 320, 100, 200, 50)]});
    // The group's request is made by no person (as a configured step's may be): its failure goes to the managers.
    let sys = t.sys(&acme);
    for (mut draft, who, by_person) in [(copy, "Client", true), (group, "Director A", false)] {
        draft["document"] = json!(t.upload(&asha, "retry.pdf", &pdf).await);
        let ctx = if by_person { asha.clone() } else { sys.clone() };
        let id = uid(&t.k.insert(&ctx, "SignRequest", obj(draft)).await.unwrap());
        let sent = t.k.run_action(&ctx, "SignRequest", id, "send", json!({})).await.unwrap();
        let link = sent["signers"].as_array().unwrap().iter().find(|l| l["role"] == json!(who)).unwrap()["url"].as_str().unwrap().to_string();
        let original: Uuid = t.get(&sys, "SignRequest", id).await["document"].as_str().unwrap()[7..].parse().unwrap();

        // The last signature cannot be sealed: the stored original was changed.
        let kept = t.store.tamper(original);
        let (status, signed) = t.sign(&acme, &token_of(&link), None, json!({"consent": true, "signature": {"text": "Signer"}})).await.unwrap();
        assert_eq!((status, signed["request_status"].clone()), (200, json!("In Progress")), "{who}: signed, not sealed");
        let r = t.get(&sys, "SignRequest", id).await;
        assert!(r["sealed_document"].is_null() && r["seal_error"].as_str().unwrap().contains("no longer matches"), "{who}: the reason is kept");
        let told = if by_person { &asha } else { &mira };
        let notices = |n: Vec<(String, Option<String>)>| -> Vec<Option<String>> {
            n.into_iter().filter(|(title, _)| title.contains("could not be sealed")).map(|(_, b)| b).collect()
        };
        let first = notices(t.notifications(&acme, told).await);
        assert_eq!(first.len(), 1, "{who}: a person is told");
        assert!(first[0].as_deref().unwrap_or_default().contains("no longer matches"), "{who}: with the reason");

        // Still failing: upkeep tries again, says it failed, keeps the reason, tells nobody twice.
        backdate(id).await;
        let again = ferrum_app_sign::maintenance(&t.k, acme.id).await;
        assert!(again.as_ref().is_err_and(|e| e.to_string().contains("sealing failed")), "{who}: upkeep tried and failed: {again:?}");
        assert_eq!(t.get(&sys, "SignRequest", id).await["status"], json!("In Progress"));
        assert_eq!(notices(t.notifications(&acme, told).await).len(), 1, "{who}: told once");

        // No path completes it without its seal, not even a write straight to the table.
        let bare = t.sql(&acme, "update e_sign_request set status = 'Completed' where tenant_id = $1 and id = $2", id).await;
        assert!(bare.is_err(), "{who}: the database refuses Completed without a seal");
        let _ = t.k.update(&mira, "SignRequest", id, obj(json!({"status": "Completed"})), None).await;
        assert_eq!(t.get(&sys, "SignRequest", id).await["status"], json!("In Progress"), "{who}: a manager cannot complete it either");

        // Mended: the next upkeep seals it, once, however many run at the same moment.
        t.store.restore(original, kept);
        backdate(id).await;
        let (a, b) = tokio::join!(ferrum_app_sign::maintenance(&t.k, acme.id), ferrum_app_sign::maintenance(&t.k, acme.id));
        a.expect("upkeep");
        b.expect("upkeep");
        t.maintenance(&acme).await;
        let r = t.get(&sys, "SignRequest", id).await;
        assert_eq!(r["status"], json!("Completed"), "{who}: retried and sealed");
        assert!(r["sealed_document"].is_string() && r["sealed_sha256"].is_string() && r["seal_error"].is_null());
        assert_eq!(t.count(&acme, id, "Completed").await, 1, "{who}: sealed exactly once");
        let sealed: i64 = t.scalar(&acme, "select count(*) from outbox where tenant_id = $1 and topic = 'sign.request_completed' and record_id = $2", id).await;
        assert_eq!(sealed, 1, "{who}: completed once on the outbox");
    }
}

/// S11-G1: two apps of the test's own. `quotes` declares that it uses Sign's create; `nosy` declares nothing of Sign's.
const QUOTES: &str = "package: quotes\nversion: 0.1.0\ndepends: [sign]\nuses:\n  - {capability: sign.sign_request.create, version: '^0.3'}\n";
const NOSY: &str = "package: nosy\nversion: 0.1.0\ndepends: [sign]\n";

impl T {
    /// App `app`'s call for `ctx`'s person, in a transaction of its own committed when it is done.
    async fn call(&self, app: &str, ctx: &Ctx, inv: ferrum_kernel::capability::Invocation) -> Result<ferrum_kernel::capability::Receipt, Error> {
        let mut tx = self.k.db.tenant_tx(ctx.tenant).await.unwrap();
        let done = self.k.call_tx(&self.apps[app], ctx, &mut tx, &inv).await;
        if done.is_ok() {
            tx.commit().await.unwrap();
        } else {
            tx.rollback().await.unwrap();
        }
        done
    }
}

#[test]
fn sign_is_reached_through_its_operations() {
    run(operations());
}

/// S11-G1a: another app asks for a signature through the gate, once per key, and is told by events; a request it makes
/// asks its signers for a code sent to their own address; void says so on the outbox; and the gate refuses an app that
/// declared no use and a person without the right.
async fn operations() {
    use ferrum_kernel::capability::Invocation;
    let t = boot_with(KernelSettings::default(), vec![package(&[("package.yaml", QUOTES)]), package(&[("package.yaml", NOSY)])]).await;
    let acme = t.tenant("acme", &["sign", "quotes", "nosy"], "UTC").await;
    let asha = t.user(&acme, "asha@acme.test", "Asha Rao", &["Sign User"]).await;
    let ravi = t.user(&acme, "ravi@acme.test", "Ravi Kumar", &["Sign User"]).await;
    let sys = t.sys(&acme);
    let pdf = contract_pdf("Quote");
    let values = |doc: &str| {
        json!({"title": "Quote Q-1", "document": doc, "send_on_create": true,
        "signers": [{"role": "Customer", "name": "Cora", "email": "cora@client.test"}],
        "fields": [field("Customer", "Signature", "Sign", 1, 72, 600, 200, 50)]})
    };
    let doc = t.upload(&asha, "quote.pdf", &pdf).await;
    let ask = Invocation::new("sign.sign_request.create", json!({"values": values(&doc)})).key("quote:Q-1:1");

    // create_through_call_tx_sends_once_per_key
    let first = t.call("quotes", &asha, ask.clone()).await.expect("the app's use");
    let again = t.call("quotes", &asha, ask.clone()).await.expect("the same key");
    assert!(!first.repeated && again.repeated, "a repeat is answered, not repeated");
    let id = uid(first.result.as_object().unwrap());
    assert_eq!(uid(again.result.as_object().unwrap()), id);
    assert_eq!(t.list(&sys, "SignRequest", json!({})).await.len(), 1, "one request");
    let r = t.get(&sys, "SignRequest", id).await;
    assert_eq!((r["status"].clone(), r["sender_email"].clone()), (json!("Sent"), json!("asha@acme.test")), "sent at once, the person its sender");
    assert_eq!(t.trail(&acme, id).await, vec!["Sent:Customer"], "sent once");

    // requests_from_apps_ask_for_an_email_code_by_default
    assert_eq!(r["verify_identity"], json!("Email Code"));
    let key = t.key(&acme).await;
    let cora = token_for(&key, acme.id, id, uid(&signer_row(&r, "Customer")), "cora@client.test");
    let signs = json!({"consent": true, "signature": {"text": "Cora"}});
    assert!(refused(t.sign(&acme, &cora, None, signs.clone()).await, "forbidden").contains("confirm the code"));
    let mine =
        t.k.insert(
            &asha,
            "SignRequest",
            obj(json!({"title": "Mine", "document": t.upload(&asha, "m.pdf", &pdf).await,
        "signers": [{"role": "C", "name": "Cy", "email": "cy@x.test"}], "fields": [field("C", "Signature", "S", 1, 72, 600, 200, 50)]})),
        )
        .await
        .unwrap();
    assert!(mine["verify_identity"].is_null(), "a request made on the screen asks for no code unless chosen");

    // a_caller_without_the_use_is_refused
    let other = Invocation::new("sign.sign_request.create", json!({"values": values(&t.upload(&asha, "q2.pdf", &pdf).await)})).key("nosy:1");
    refused(t.call("nosy", &asha, other).await, "forbidden");
    let void = Invocation::new("sign.sign_request.void", json!({"id": id, "params": {"reason": "x"}})).key("void:1");
    refused(t.call("quotes", &asha, void).await, "forbidden");
    assert_eq!(t.list(&sys, "SignRequest", json!({})).await.len(), 2, "nothing made by a refused call");

    // a_sign_user_cannot_void_another_users_request
    let theirs = Invocation::new("sign.sign_request.void", json!({"id": id, "params": {"reason": "not mine"}})).key("ravi:void");
    assert!(t.k.invoke(&ravi, theirs).await.is_err(), "Ravi cannot void Asha's request");
    assert_eq!(t.get(&sys, "SignRequest", id).await["status"], json!("Sent"));

    // void_says_so_on_the_outbox, through the gate with a key
    let voided =
        t.k.invoke(&asha, Invocation::new("sign.sign_request.void", json!({"id": id, "params": {"reason": "Price changed"}})).key("asha:void")).await.unwrap();
    assert_eq!(voided.result["status"], json!("Voided"));
    let mut tx = t.k.db.tenant_tx(acme.id).await.unwrap();
    let rows: Vec<Value> = sqlx::query_scalar("select payload from outbox where tenant_id = $1 and topic = 'sign.request_voided' and record_id = $2")
        .bind(acme.id)
        .bind(id)
        .fetch_all(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(rows.len(), 1, "one event");
    assert_eq!((rows[0]["reason"].clone(), rows[0]["status"].clone()), (json!("Price changed"), json!("Voided")));
    let (status, _) = t.view(&acme, &cora, None).await.unwrap();
    assert_eq!(status, 410, "every link refused");
}

#[test]
fn a_link_signer_opens_the_page_only_with_a_session() {
    run(link_delivery());
}

/// S11-G1b: a signer whose link is not mailed (delivery Link) is signed through a page the requesting app opens with
/// `signing_link`: a session bound to that signer and a moment at most an hour away. Their bare link opens nothing; a
/// forged or past session opens nothing; framing needs a site the workspace allows; a person without write on the
/// request gets no link. Every new device that opens a link is in the trail.
async fn link_delivery() {
    use ferrum_kernel::capability::Invocation;
    let t = boot(KernelSettings::default()).await;
    let acme = t.tenant("acme", &["sign"], "UTC").await;
    let asha = t.user(&acme, "asha@acme.test", "Asha Rao", &["Sign User"]).await;
    let ravi = t.user(&acme, "ravi@acme.test", "Ravi Kumar", &["Sign User"]).await;
    let pdf = contract_pdf("Linked");
    let draft = json!({"title": "Linked", "document": t.upload(&asha, "l.pdf", &pdf).await,
        "signers": [{"role": "Guest", "name": "Gil", "email": "gil@guest.test", "delivery": "Link"},
                    {"role": "Host", "name": "Hana", "email": "hana@acme.test"}],
        "fields": [field("Guest", "Signature", "G", 1, 72, 600, 200, 50), field("Host", "Signature", "H", 1, 320, 600, 200, 50)],
        "signing_mode": "Parallel"});
    let id = uid(&t.k.insert(&asha, "SignRequest", obj(draft)).await.unwrap());
    let sent = t.action(&asha, id, "send", json!({})).await;
    let rows = sent["signers"].as_array().unwrap();
    let guest_row = rows.iter().find(|l| l["role"] == json!("Guest")).unwrap();
    assert!(guest_row.get("url").is_none() && guest_row["delivery"] == json!("Link"), "no link handed out for the app's signer: {guest_row}");
    let r = t.get(&asha, "SignRequest", id).await;
    let key = t.key(&acme).await;
    let gil = token_for(&key, acme.id, id, uid(&signer_row(&r, "Guest")), "gil@guest.test");
    refused(t.view(&acme, &gil, None).await, "not found");

    // A person who may not change the request gets no link.
    let ask = |minutes: i64, embed: bool| {
        Invocation::new("sign.sign_request.signing_link", json!({"id": id, "params": {"signer": "Guest", "minutes": minutes, "embed": embed}}))
    };
    assert!(t.k.invoke(&ravi, ask(10, false).key("ravi")).await.is_err(), "Ravi cannot issue Asha's signer a link");
    assert!(
        t.k.invoke(&asha, Invocation::new("sign.sign_request.signing_link", json!({"id": id, "params": {"signer": "Host"}})).key("host")).await.is_err(),
        "a mailed signer's link is not issued this way"
    );
    assert!(t.k.invoke(&asha, ask(61, false).key("long")).await.is_err(), "at most an hour");
    let m = refused(t.k.invoke(&asha, ask(10, true).key("framed")).await, "invalid");
    assert!(m.contains("allow"), "framing needs an allowed site: {m}");

    let given = t.k.invoke(&asha, ask(10, false).key("asha:1")).await.expect("a link").result;
    let url = given["url"].as_str().unwrap().to_string();
    assert!(url.contains(&format!("/sign/s/acme/{gil}?e=")) && !url.contains("embed"), "{url}");
    let session = url.split("?e=").nth(1).unwrap().to_string();
    let open = |session: String| {
        let (t, acme, gil) = (&t, &acme, gil.clone());
        async move { t.site_with(acme, "GET", &format!("/signing/{gil}"), &[("x-sign-session", &session)], vec![]).await }
    };
    assert_eq!(open(session.clone()).await.expect("the page opens").status, 200);
    assert!(t.trail(&acme, id).await.contains(&"Link Issued:Guest".to_string()));
    // Forged, another signer's, or past its moment: nothing.
    let host = uid(&signer_row(&r, "Host"));
    for bad in [
        format!("{}A", &session[..session.len() - 1]),
        session_for(&key, acme.id, host, "gil@guest.test", chrono::Utc::now().timestamp() + 600),
        session_for(&key, acme.id, uid(&signer_row(&r, "Guest")), "gil@guest.test", chrono::Utc::now().timestamp() - 1),
    ] {
        refused(open(bad).await, "not found");
    }

    // Framed, once the workspace allows a site for the signing page.
    t.k.insert(&acme.admin, "Embedding", obj(json!({"page": "sign/s", "origins": "https://partner.example"}))).await.expect("an embedding");
    let framed = t.k.invoke(&asha, ask(5, true).key("asha:2")).await.expect("a framed link").result;
    assert!(framed["url"].as_str().unwrap().ends_with("&embed=1"));

    // Devices: the same browser again is nothing new; another one is in the trail.
    open(session.clone()).await.unwrap();
    assert_eq!(t.count(&acme, id, "Viewed").await, 1);
    t.site_with(&acme, "GET", &format!("/signing/{gil}"), &[("x-sign-session", &session), ("user-agent", "Other/1.0")], vec![]).await.unwrap();
    assert_eq!(t.count(&acme, id, "Viewed").await, 2, "another device is recorded");
    let mut tx = t.k.db.tenant_tx(acme.id).await.unwrap();
    let details: Vec<Option<String>> =
        sqlx::query_scalar("select details from e_sign_event where tenant_id = $1 and request = $2 and event = 'Viewed' order by seq")
            .bind(acme.id)
            .bind(id)
            .fetch_all(&mut *tx)
            .await
            .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(details, [None, Some("another device".to_string())]);

    // No reminder goes to a signer whose page the app opens.
    t.sql(&acme, "update e_sign_request__signers set sent_at = now() - interval '9 days' where tenant_id = $1 and parent_id = $2", id).await.unwrap();
    t.maintenance(&acme).await;
    let reminded = t.trail(&acme, id).await.into_iter().filter(|e| e.starts_with("Reminded")).collect::<Vec<_>>();
    assert_eq!(reminded, ["Reminded:Host"], "the mailed signer only");

    // The session signs.
    let (status, done) = t
        .site_with(
            &acme,
            "POST",
            &format!("/signing/{gil}/sign"),
            &[("x-sign-session", &session)],
            json!({"consent": true, "signature": {"text": "Gil"}}).to_string().into_bytes(),
        )
        .await
        .map(|r| (r.status, serde_json::from_slice::<Value>(&r.body).unwrap()))
        .unwrap();
    assert_eq!((status, done["status"].clone()), (200, json!("Signed")));

    // in_person_needs_a_person_at_the_screen: a host signed in at this screen, who says they checked who signs.
    let draft = json!({"title": "Counter", "document": t.upload(&asha, "c.pdf", &pdf).await,
        "signers": [{"role": "Customer", "name": "Cleo", "email": "cleo@client.test", "delivery": "In Person"}],
        "fields": [field("Customer", "Signature", "C", 1, 72, 600, 200, 50)]});
    let counter = uid(&t.k.insert(&asha, "SignRequest", obj(draft)).await.unwrap());
    t.action(&asha, counter, "send", json!({})).await;
    let host = |checked: bool| json!({"signer": "Customer", "checked_identity": checked});
    // Asha's context as loaded for a job, a key or an app is not at a screen.
    let m = refused(t.k.run_action(&asha, "SignRequest", counter, "signing_link", host(true)).await, "forbidden");
    assert!(m.contains("at this screen"), "{m}");
    let mut at_screen = asha.clone();
    at_screen.made = ferrum_kernel::ctx::Made::Screen;
    assert!(at_screen.at_screen());
    refused(t.k.run_action(&at_screen, "SignRequest", counter, "signing_link", host(false)).await, "invalid");
    let given = t.k.run_action(&at_screen, "SignRequest", counter, "signing_link", host(true)).await.expect("hosted");
    let url = given["url"].as_str().unwrap();
    let (token, session) = url.rsplit('/').next().unwrap().split_once("?e=").unwrap();
    let r = t.get(&asha, "SignRequest", counter).await;
    assert_eq!(signer_row(&r, "Customer")["hosted_by"], json!("asha@acme.test"));
    let (status, _) = t
        .site_with(
            &acme,
            "POST",
            &format!("/signing/{token}/sign"),
            &[("x-sign-session", session)],
            json!({"consent": true, "signature": {"text": "Cleo"}}).to_string().into_bytes(),
        )
        .await
        .map(|r| (r.status, ()))
        .unwrap();
    assert_eq!(status, 200);
    let r = t.get(&asha, "SignRequest", counter).await;
    assert_eq!(r["status"], json!("Completed"));
    let sealed: Uuid = r["sealed_document"].as_str().unwrap()[7..].parse().unwrap();
    let (_, _, bytes) = t.k.read_file(&asha, sealed).await.unwrap();
    let parsed = lopdf::Document::load_mem(&bytes).unwrap();
    let text: String = parsed.get_pages().values().map(|p| String::from_utf8_lossy(&parsed.get_page_content(*p)).to_string()).collect();
    assert!(text.contains(&hex::encode("hosted")), "the certificate names the host");
    let mut tx = t.k.db.tenant_tx(acme.id).await.unwrap();
    let signed: Option<String> = sqlx::query_scalar("select details from e_sign_event where tenant_id = $1 and request = $2 and event = 'Signed'")
        .bind(acme.id)
        .bind(counter)
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(signed.as_deref(), Some("signature typed; signed in person, hosted by asha@acme.test"));
}

/// A session computed from the documented format, independently of Sign's code.
fn session_for(key: &[u8], tenant: Uuid, signer: Uuid, email: &str, expires: i64) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).unwrap();
    for part in [b"ferrum-sign/session/v1".as_slice(), tenant.as_bytes(), signer.as_bytes(), email.as_bytes(), &expires.to_be_bytes()] {
        mac.update(&(part.len() as u32).to_be_bytes());
        mac.update(part);
    }
    let mut raw = expires.to_be_bytes().to_vec();
    raw.extend_from_slice(&mac.finalize().into_bytes()[..16]);
    URL_SAFE_NO_PAD.encode(raw)
}

/// What opens the test's vault: the database password of `url`, with Argon2id kept cheap for tests.
fn opener(url: &str) -> ferrum_kernel::vault::Opener {
    ferrum_kernel::vault::Opener {
        database_url: url.to_string(),
        file: "/nonexistent/root.key".into(),
        kdf: ferrum_kernel::vault::Kdf { m_kib: 64, t: 1, p: 1 },
        ..Default::default()
    }
}

impl T {
    /// A statement without parameters in the workspace's schema, as an upgrade's SQL runs.
    async fn exec(&self, t: &Tenant, sql: &str) {
        let mut tx = self.k.db.tenant_tx(t.id).await.unwrap();
        sqlx::raw_sql(sqlx::AssertSqlSafe(sql.to_string())).execute(&mut *tx).await.unwrap_or_else(|e| panic!("{sql}: {e}"));
        tx.commit().await.unwrap();
    }

    async fn scalar0<O: Send + Unpin + for<'r> sqlx::Decode<'r, sqlx::Postgres> + sqlx::Type<sqlx::Postgres>>(&self, t: &Tenant, sql: &str) -> O {
        let mut tx = self.k.db.tenant_tx(t.id).await.unwrap();
        let v = sqlx::query_scalar(sqlx::AssertSqlSafe(sql.to_string())).fetch_one(&mut *tx).await.unwrap_or_else(|e| panic!("{sql}: {e}"));
        tx.commit().await.unwrap();
        v
    }

    /// Whether the database has checked `sign_completed_is_sealed` over the rows that were there.
    async fn seal_constraint_checked(&self, t: &Tenant) -> bool {
        self.scalar0(t, "select convalidated from pg_constraint where conname = 'sign_completed_is_sealed' and conrelid = 'e_sign_request'::regclass").await
    }

    /// A request with one signer, sent; its draft's file stays the stored original.
    async fn one_signer_request(&self, sender: &Ctx, title: &str, pdf: &[u8]) -> (Uuid, String) {
        let draft = json!({"title": title, "document": self.upload(sender, "one.pdf", pdf).await,
            "signers": [{"role": "Client", "name": "Cleo", "email": "cleo@client.test"}],
            "fields": [field("Client", "Signature", "C", 1, 72, 600, 200, 50)]});
        let id = uid(&self.k.insert(sender, "SignRequest", obj(draft)).await.unwrap());
        let sent = self.action(sender, id, "send", json!({})).await;
        (id, token_of(sent["signers"][0]["url"].as_str().unwrap()))
    }
}

#[test]
fn a_signed_in_customer_is_served_a_signing_link_like_anyone() {
    run(signed_in_customer());
}

/// S4-P4 (20 Nov): a person who carries a site session (a customer signed in on the workspace's site) reaches the
/// package's public routes as the platform hands them over, with that person's context. The link's token is the right
/// on these pages, not the session: every route answers exactly as it does for a caller with no session, whatever the
/// answer is (a page, a refusal, a closed request), and signing leaves the same trail.
async fn signed_in_customer() {
    let t = boot(KernelSettings::default()).await;
    let acme = t.tenant("acme", &["sign"], "UTC").await;
    let asha = t.user(&acme, "asha@acme.test", "Asha Rao", &["Sign User"]).await;
    let pdf = contract_pdf("Customer");
    let (plain, plain_token) = t.one_signer_request(&asha, "Without a session", &pdf).await;
    let (session, session_token) = t.one_signer_request(&asha, "With a session", &pdf).await;
    // What a caller gets, whoever they are: status and body, or the refusal.
    // (The page's CSP nonce is new on every answer: masked.)
    let answer = |r: Result<ferrum_kernel::SiteResponse, Error>| {
        r.map(|r| {
            let mut body = String::from_utf8_lossy(&r.body).into_owned();
            while let Some(at) = body.find("nonce=\"").map(|i| i + 7) {
                let end = body[at..].find('"').map_or(body.len(), |e| at + e);
                body.replace_range(at..end, "N");
                body.replace_range(at - 7..at - 1, "nonce_");
            }
            (r.status, r.content_type, body)
        })
        .map_err(|e| format!("{e:?}"))
    };
    for who in [None, Some(&asha)] {
        let open = answer(t.site_as(&acme, who, "GET", &format!("/s/{plain_token}"), &[], vec![]).await);
        assert!(open.as_ref().is_ok_and(|(status, _, _)| *status == 200), "the page opens for {:?}: {open:?}", who.map(|w| w.email.clone()));
    }
    for (what, path, method) in [
        ("the page", "/s/{t}", "GET"),
        ("the request", "/signing/{t}", "GET"),
        ("the document", "/signing/{t}/document", "GET"),
        ("a link nobody was given", "/signing/not-a-link", "GET"),
        ("a code asked for", "/signing/{t}/code", "POST"),
    ] {
        let path = path.replace("{t}", &plain_token);
        let without = answer(t.site_as(&acme, None, method, &path, &[], vec![]).await);
        let with = answer(t.site_as(&acme, Some(&asha), method, &path, &[], vec![]).await);
        assert_eq!(with, without, "{what}: the same answer with and without a site session");
    }
    // Signing: through the session's request and the plain one, the same outcome and the same trail.
    let body = json!({"consent": true, "signature": {"text": "Cleo Client"}}).to_string().into_bytes();
    let signed_plain = t.site_as(&acme, None, "POST", &format!("/signing/{plain_token}/sign"), &[], body.clone()).await.unwrap();
    let signed_session = t.site_as(&acme, Some(&asha), "POST", &format!("/signing/{session_token}/sign"), &[], body).await.unwrap();
    assert_eq!((signed_session.status, signed_plain.status), (200, 200));
    let (a, b): (Value, Value) = (serde_json::from_slice(&signed_plain.body).unwrap(), serde_json::from_slice(&signed_session.body).unwrap());
    assert_eq!((&a["request_status"], &b["request_status"]), (&json!("Completed"), &json!("Completed")));
    // (The plain request was opened by the page checks above, so its trail has Viewed: the rest is what is compared.)
    let without_views = |trail: Vec<String>| trail.into_iter().filter(|e| !e.starts_with("Viewed")).collect::<Vec<_>>();
    assert_eq!(without_views(t.trail(&acme, plain).await), without_views(t.trail(&acme, session).await), "the session changes nothing in the trail");
    // The person is no one in the trail: the signer is.
    let actors: Vec<String> = {
        let mut tx = t.k.db.tenant_tx(acme.id).await.unwrap();
        let rows = sqlx::query_scalar("select actor from e_sign_event where tenant_id = $1 and request = $2 and event = 'Signed'")
            .bind(acme.id)
            .bind(session)
            .fetch_all(&mut *tx)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        rows
    };
    assert_eq!(actors, vec!["cleo@client.test".to_string()], "the trail names the signer, not the signed-in person");
    // A finished request answers the same too.
    for path in [format!("/signing/{plain_token}"), format!("/signing/{plain_token}/document")] {
        let (without, with) =
            (answer(t.site_as(&acme, None, "GET", &path, &[], vec![]).await), answer(t.site_as(&acme, Some(&asha), "GET", &path, &[], vec![]).await));
        assert_eq!(with, without, "{path} once completed");
    }
}

/// The migration that adds `sign_completed_is_sealed`, as a workspace's upgrade runs it.
const SEAL_CONSTRAINT: &str = include_str!("../packages/sign/sql/002_completed_is_sealed.sql");

#[test]
fn an_old_completed_request_without_its_seal_does_not_stop_the_upgrade() {
    run(legacy_unsealed());
}

/// sign-1-window: a workspace that already holds Completed requests without their seal still upgrades (the constraint
/// is added `not valid`, so no row is checked and no write is held), the database refuses every new completion without a
/// seal from that moment, upkeep seals the old requests (once, whatever runs together; "Sealed" in the trail, the
/// original "Completed" kept, nothing sent again) or, for one it cannot seal, tells a person once; and only when no such
/// row is left does it validate the constraint.
async fn legacy_unsealed() {
    let t = boot(KernelSettings::default()).await;
    let acme = t.tenant("acme", &["sign"], "UTC").await;
    let asha = t.user(&acme, "asha@acme.test", "Asha Rao", &["Sign User"]).await;
    let sys = t.sys(&acme);
    let pdf = contract_pdf("Old");
    let body = json!({"consent": true, "signature": {"text": "Cleo Client"}});
    let mut old = vec![];
    for title in ["Old one", "Old two"] {
        let (id, token) = t.one_signer_request(&asha, title, &pdf).await;
        let (status, signed) = t.sign(&acme, &token, None, body.clone()).await.unwrap();
        assert_eq!((status, signed["request_status"].clone()), (200, json!("Completed")), "{title} was sealed the normal way");
        old.push(id);
    }
    let (kept, stuck) = (old[0], old[1]);
    // The workspace as an older release left it: no constraint, two Completed requests with no seal.
    t.exec(&acme, "alter table e_sign_request drop constraint sign_completed_is_sealed").await;
    for id in &old {
        t.sql(&acme, "update e_sign_request set sealed_document = null, sealed_sha256 = null where tenant_id = $1 and id = $2", *id).await.unwrap();
    }
    let (_pending, _) = t.one_signer_request(&asha, "Still open", &pdf).await;
    let open: Uuid = t.scalar0(&acme, "select id from e_sign_request where status = 'Sent' limit 1").await;

    // The upgrade: its SQL must go through over those rows, leaving the constraint unchecked.
    t.exec(&acme, SEAL_CONSTRAINT).await;
    assert!(!t.seal_constraint_checked(&acme).await, "added not valid: the old rows were not checked");
    let refused = t.sql(&acme, "update e_sign_request set status = 'Completed' where tenant_id = $1 and id = $2", open).await;
    assert!(refused.is_err(), "a new completion without a seal is refused from the first moment");
    assert_eq!(t.scalar0::<i64>(&acme, "select count(*) from e_sign_request where status = 'Completed' and sealed_document is null").await, 2);

    // The stored original of one cannot be read back unchanged: its seal cannot be rebuilt.
    let original: Uuid = t.get(&sys, "SignRequest", stuck).await["document"].as_str().unwrap()[7..].parse().unwrap();
    let before = t.store.tamper(original);
    // Two upkeeps at once: the sealable one is sealed once; the other fails for both and is told once.
    let (a, b) = tokio::join!(ferrum_app_sign::maintenance(&t.k, acme.id), ferrum_app_sign::maintenance(&t.k, acme.id));
    assert!(a.is_err() || b.is_err(), "upkeep says the stuck one could not be sealed");
    let r = t.get(&sys, "SignRequest", kept).await;
    assert!(r["sealed_document"].is_string() && r["sealed_sha256"].is_string(), "the old request is sealed");
    assert_eq!(r["status"], json!("Completed"));
    assert_eq!(t.count(&acme, kept, "Completed").await, 1, "its completion is not recorded again");
    assert_eq!(t.count(&acme, kept, "Sealed").await, 1, "sealed exactly once, however many upkeeps ran");
    assert_eq!(t.action(&asha, kept, "verify", json!({})).await["valid"], json!(true), "the trail still verifies");
    let r = t.get(&sys, "SignRequest", stuck).await;
    assert!(r["sealed_document"].is_null());
    assert_eq!(t.count(&acme, stuck, "Seal Failed").await, 1, "a person is told in the trail once");
    let told = |n: Vec<(String, Option<String>)>| n.into_iter().filter(|(title, _)| title.contains("could not be sealed")).count();
    assert_eq!(told(t.notifications(&acme, &asha).await), 1);
    assert!(ferrum_app_sign::maintenance(&t.k, acme.id).await.is_err());
    assert_eq!((t.count(&acme, stuck, "Seal Failed").await, told(t.notifications(&acme, &asha).await)), (1, 1), "told once, not every minute");
    assert!(!t.seal_constraint_checked(&acme).await, "not validated while an unsealed Completed request is left");

    // Mended: sealed, and only now is the constraint checked, by upkeep, safe to repeat.
    t.store.restore(original, before);
    t.maintenance(&acme).await;
    let r = t.get(&sys, "SignRequest", stuck).await;
    assert!(r["sealed_document"].is_string() && r["sealed_sha256"].is_string());
    assert_eq!((t.count(&acme, stuck, "Completed").await, t.count(&acme, stuck, "Sealed").await), (1, 1));
    assert_eq!(t.action(&asha, stuck, "verify", json!({})).await["valid"], json!(true));
    assert!(t.seal_constraint_checked(&acme).await, "validated once no unsealed Completed request is left");
    t.maintenance(&acme).await;
    assert!(t.seal_constraint_checked(&acme).await);
    let refused = t.sql(&acme, "update e_sign_request set status = 'Completed' where tenant_id = $1 and id = $2", open).await;
    assert!(refused.is_err(), "and still refuses a completion without a seal");
}

#[test]
fn signers_get_the_workspaces_own_wording_and_brand() {
    run(wording());
}

/// S11-G4: a workspace without the Mail app, on a server with a mailer. Links and signed copies are sent from the
/// package's message templates (through the platform's messages, not Comms' records): the workspace's edit of a template
/// is in the next mail; the signing link is a secret value, so the message log holds a placeholder, never the token; the
/// signing page carries the workspace's brand and its own consent sentence.
async fn wording() {
    let mail = Arc::new(TestMailer::default());
    let t = boot_mailed(KernelSettings::default(), vec![], Some(mail.clone())).await;
    let globex = t.tenant("globex", &["sign"], "UTC").await;
    let gina = t.user(&globex, "gina@globex.test", "Gina Ortiz", &["Sign User"]).await;
    let pdf = contract_pdf("Wording");
    let order = |title: &str| {
        json!({"title": title, "document": null, "signers": [{"role": "Supplier", "name": "Sam Supplier", "email": "sam@supplier.test"}],
            "fields": [field("Supplier", "Signature", "Sign", 1, 72, 600, 200, 50)]})
    };
    let make = |title: &'static str| {
        let (t, gina, pdf) = (&t, &gina, &pdf);
        async move {
            let mut draft = order(title);
            draft["document"] = json!(t.upload(gina, "po.pdf", pdf).await);
            uid(&t.k.insert(gina, "SignRequest", obj(draft)).await.unwrap())
        }
    };
    let po_id = make("Purchase Order").await;
    assert!(refused(t.k.run_action(&gina, "SignRequest", po_id, "send", json!({})).await, "invalid").contains("Public URL"), "emailed links must be absolute");
    // The first send created the settings row with the signing key: complete it.
    let settings = t.list(&globex.admin, "SignSettings", json!({})).await;
    assert_eq!(settings.len(), 1);
    let settings_id = uid(&settings[0]);
    assert_eq!(t.get(&globex.admin, "SignSettings", settings_id).await["signing_key"], json!(true), "the key is a secret: reads only say it is set");
    t.k.update(&globex.admin, "SignSettings", settings_id, obj(json!({"public_url": "https://erp.globex.test/"})), None).await.unwrap();
    let sent = t.action(&gina, po_id, "send", json!({})).await;
    assert_eq!((sent["signers"][0]["emailed"].clone(), sent["signers"][0].get("url").is_none()), (json!(true), true), "emailed, so no link in the answer");
    let deliver = || async { while t.k.dispatch_outbox_once().await.unwrap_or(0) > 0 {} };
    deliver().await;
    let sent_mail = || mail.0.lock().unwrap().clone();
    let first = sent_mail();
    assert_eq!(first.len(), 1, "one invitation, with Mail or not");
    assert_eq!(first[0].0, "sam@supplier.test");
    assert!(first[0].1.contains("please sign \"Purchase Order\""), "the package's invitation wording: {}", first[0].1);
    let prefix = "https://erp.globex.test/api/site/globex/sign/s/";
    // (The template's escaping writes a slash as an entity; a browser reads it back.)
    let body = first[0].2.replace("&#x2f;", "/");
    let start = body.find(prefix).unwrap_or_else(|| panic!("an absolute signing link in the email: {body}"));
    let token: String = body[start + prefix.len()..].chars().take(43).collect();
    // The link is a secret of its message: the log keeps a placeholder, not the token.
    let log = t.k.message_log(&gina, Some(("SignRequest", po_id)), None, 20).await.unwrap();
    assert!(!log.is_empty() && !serde_json::to_string(&log).unwrap().contains(&token), "the message log never holds the signing link");

    // The signing page: the workspace's brand and its own consent sentence (none set: null, the page words it).
    let brand = uid(&t.k.insert(&globex.admin, "Brand", obj(json!({"brand_name": "Globex Legal", "primary_color": "#aa3300"}))).await.unwrap());
    let (_, view) = t.view(&globex, &token, None).await.unwrap();
    assert!(view["consent"].is_null(), "no custom sentence: the page words its own");
    t.k.update(
        &globex.admin,
        "SignSettings",
        settings_id,
        obj(json!({"brand": brand.to_string(), "consent_text": "  I accept Globex's purchase terms.  "})),
        None,
    )
    .await
    .unwrap();
    let (_, view) = t.view(&globex, &token, None).await.unwrap();
    assert_eq!(view["brand"]["name"], json!("Globex Legal"), "the signing page carries the settings' brand");
    assert_eq!(view["consent"], json!("I accept Globex's purchase terms."), "a custom consent is shown, trimmed");
    let (_, done) = t.sign(&globex, &token, None, json!({"consent": true, "signature": {"image": drawn(500, 150)}})).await.unwrap();
    assert_eq!(done["request_status"], json!("Completed"));
    let po = t.get(&gina, "SignRequest", po_id).await;
    deliver().await;
    let mails = sent_mail();
    let copy = mails.iter().find(|m| m.1.starts_with("Signed:")).expect("the signer is sent a copy");
    assert_eq!(copy.3, vec!["Purchase Order - signed.pdf".to_string()], "the sealed PDF is attached");
    assert!(copy.2.contains(po["sealed_sha256"].as_str().unwrap()));
    assert_eq!(
        t.get(&gina, "SignRequest", po_id).await["sealed_document"].as_str().map(|d| d.len()),
        po["sealed_document"].as_str().map(|d| d.len()),
        "sealing is unchanged"
    );

    // An edit of the reminder template is in the next reminder.
    let read = t.k.message_template(&globex.admin, "sign.reminder").await.unwrap();
    let mut def: ferrum_meta::MessageDef = serde_json::from_value(read["template"].clone()).unwrap();
    def.languages.get_mut("en").unwrap().email.as_mut().unwrap().subject = "Nudge: {{ doc.title }}".into();
    t.k.save_message_template(&globex.admin, &def).await.unwrap();
    let second = make("Second Order").await;
    t.action(&gina, second, "send", json!({})).await;
    t.action(&gina, second, "remind", json!({})).await;
    deliver().await;
    assert!(sent_mail().iter().any(|m| m.1 == "Nudge: Second Order"), "the workspace's wording: {:?}", sent_mail().iter().map(|m| &m.1).collect::<Vec<_>>());
}

#[test]
fn choices_numbers_and_attachments_are_filled_in_and_checked() {
    run(richer_fields());
}

fn choice(role: &str, kind: &str, label: &str, y: i64, extra: Value) -> Value {
    let mut f = field(role, kind, label, 1, 72, y, 150, 20);
    for (k, v) in extra.as_object().unwrap() {
        f[k] = v.clone();
    }
    f
}

/// S11-G2: Dropdown, Radio, Number and Attachment fields. A draft refuses a field without what it needs; the signer's
/// answer must be one of the choices, a number within its places and bounds; an upload is checked by its bytes against
/// the field's kinds, is the signer's own and no one else's, at most three times, and a stranger's link stores nothing.
async fn richer_fields() {
    let t = boot(KernelSettings::default()).await;
    let acme = t.tenant("acme", &["sign"], "UTC").await;
    let asha = t.user(&acme, "asha@acme.test", "Asha Rao", &["Sign User"]).await;
    let pdf = contract_pdf("Fields");
    let png: Vec<u8> = b"\x89PNG\r\n\x1a\n-not really an image-".to_vec();
    let draft = |fields: Vec<Value>| {
        json!({"title": "Fields", "signing_mode": "Parallel", "fields": fields,
            "signers": [{"role": "Client", "name": "Cleo", "email": "cleo@client.test"}, {"role": "Other", "name": "Omar", "email": "omar@client.test"}]})
    };
    // The values of a submission, by field id.
    let values = |pairs: &[(&String, Value)]| Value::Object(pairs.iter().map(|(k, v)| ((*k).clone(), v.clone())).collect());
    let plan = choice("Client", "Dropdown", "Plan", 500, json!({"options": ["Basic", "Pro"]}));
    let colour = choice("Client", "Radio", "Colour", 460, json!({"options": ["Red", "Blue"], "option_boxes": [[72, 460, 14, 14], [120, 460, 14, 14]]}));
    let seats = choice("Client", "Number", "Seats", 420, json!({"min_value": "1", "max_value": "10", "decimals": 0}));
    let card = choice("Client", "Attachment", "ID card", 380, json!({"accept": "pdf,png"}));
    let theirs = choice("Other", "Attachment", "Their ID", 340, json!({}));
    let mark = field("Client", "Signature", "C", 1, 72, 600, 200, 50);

    // A draft refuses a field without what it needs.
    for (bad, why) in [
        (choice("Client", "Dropdown", "Plan", 500, json!({})), "choices"),
        (choice("Client", "Dropdown", "Plan", 500, json!({"options": ["A", "a"]})), "same"),
        (choice("Client", "Radio", "Colour", 460, json!({"options": ["Red", "Blue"], "option_boxes": [[72, 460, 14, 14]]})), "one box"),
        (choice("Client", "Radio", "Colour", 460, json!({"options": ["Red"], "option_boxes": [[72, 2000, 14, 14]]})), "inside the page"),
        (choice("Client", "Number", "Seats", 420, json!({"min_value": "5", "max_value": "2"})), "larger"),
        (choice("Client", "Number", "Seats", 420, json!({"min_value": "five"})), "not a number"),
        (choice("Client", "Attachment", "ID", 380, json!({"accept": "pdf,exe"})), "list kinds"),
    ] {
        let mut d = draft(vec![bad]);
        d["document"] = json!(t.upload(&asha, "f.pdf", &pdf).await);
        let m = refused(t.k.insert(&asha, "SignRequest", obj(d)).await, "invalid");
        assert!(m.contains(why), "{why}: {m}");
    }
    let mut d = draft(vec![plan, colour, seats, card, theirs, mark, field("Other", "Signature", "O", 1, 320, 600, 200, 50)]);
    d["document"] = json!(t.upload(&asha, "f.pdf", &pdf).await);
    let id = uid(&t.k.insert(&asha, "SignRequest", obj(d)).await.unwrap());
    let sent = t.action(&asha, id, "send", json!({})).await;
    let r = t.get(&asha, "SignRequest", id).await;
    let link = |role: &str| token_of(sent["signers"].as_array().unwrap().iter().find(|l| l["role"] == json!(role)).unwrap()["url"].as_str().unwrap());
    let (mine, other) = (&link("Client"), &link("Other"));
    let (plan_id, colour_id, seats_id, card_id, theirs_id) =
        (field_id(&r, "Plan"), field_id(&r, "Colour"), field_id(&r, "Seats"), field_id(&r, "ID card"), field_id(&r, "Their ID"));

    // What the page is told of each field.
    let (_, view) = t.view(&acme, mine, None).await.unwrap();
    let f = |label: &str| view["fields"].as_array().unwrap().iter().find(|f| f["label"] == json!(label)).unwrap().clone();
    assert_eq!(f("Plan")["options"], json!(["Basic", "Pro"]));
    assert_eq!(f("Colour")["option_boxes"].as_array().unwrap().len(), 2);
    assert_eq!((f("Seats")["max_value"].clone(), f("Seats")["decimals"].clone()), (json!("10"), json!(0)));
    assert_eq!((f("ID card")["accept"].clone(), f("ID card")["uploaded"].clone()), (json!(["pdf", "png"]), Value::Null));
    assert!(view["fields"].as_array().unwrap().iter().all(|f| f["label"] != json!("Their ID")), "another role's field is not shown");

    // Answers: each wrong one is refused with its reason, nothing is signed.
    let ink = json!({"text": "Cleo Client"});
    let answer = |values: Value| json!({"consent": true, "signature": ink, "values": values});
    let tries = [
        (json!({}), "choose one"),
        (values(&[(&plan_id, json!("Gold")), (&colour_id, json!("Red")), (&seats_id, json!("5"))]), "not one of the choices"),
        (values(&[(&plan_id, json!("Pro")), (&colour_id, json!("Red")), (&seats_id, json!("11"))]), "at most 10"),
        (values(&[(&plan_id, json!("Pro")), (&colour_id, json!("Red")), (&seats_id, json!("2.5"))]), "whole number"),
        (values(&[(&plan_id, json!("Pro")), (&colour_id, json!("Red")), (&seats_id, json!("many"))]), "must be a number"),
        (values(&[(&plan_id, json!("Pro")), (&colour_id, json!("Red")), (&seats_id, json!("5"))]), "upload a file"),
    ];
    for (values, why) in tries {
        let m = refused(t.sign(&acme, mine, None, answer(values)).await, "invalid");
        assert!(m.contains(why), "{why}: {m}");
    }

    // Uploads: the bytes decide, not the name; the field's kinds; the signer's own; three at most; a stranger stores nothing.
    let upload = |token: &str, field: &str, name: &'static str, bytes: Vec<u8>| {
        let (t, acme) = (&t, &acme);
        let (token, field) = (token.to_string(), field.to_string());
        async move { t.site_with(acme, "POST", &format!("/signing/{token}/attachments/{field}"), &[("x-file-name", name)], bytes).await }
    };
    let files = || t.scalar0::<i64>(&acme, "select count(*) from file_object");
    let before = files().await;
    refused(upload(mine, &card_id, "id.pdf", b"MZ\x90\x00 an executable named .pdf".to_vec()).await, "invalid");
    refused(upload(mine, &card_id, "id.jpg", b"\xFF\xD8\xFF\xE0 a jpeg the field does not take".to_vec()).await, "invalid");
    refused(upload(&"A".repeat(43), &card_id, "id.png", png.clone()).await, "not found");
    refused(upload(mine, &plan_id, "id.png", png.clone()).await, "not found");
    assert_eq!(files().await, before, "a refused upload leaves no file behind");
    let ok = upload(mine, &card_id, "../my id.png", png.clone()).await.unwrap();
    assert_eq!(ok.status, 200);
    let first: Value = serde_json::from_slice(&ok.body).unwrap();
    assert!(!first["name"].as_str().unwrap().contains('/'), "path characters are not kept: {first}");
    // Another signer's upload is theirs: this signer cannot sign with it.
    let ok = upload(other, &theirs_id, "theirs.png", png.clone()).await.unwrap();
    let theirs_file: Value = serde_json::from_slice(&ok.body).unwrap();
    let m = refused(
        t.sign(
            &acme,
            mine,
            None,
            answer(values(&[(&plan_id, json!("Pro")), (&colour_id, json!("Red")), (&seats_id, json!("5")), (&card_id, theirs_file["file"].clone())])),
        )
        .await,
        "invalid",
    );
    assert!(m.contains("not the file you uploaded"), "{m}");
    // Replacing it twice more is allowed; a fourth upload is not; the replaced files are gone.
    upload(mine, &card_id, "second.png", png.clone()).await.unwrap();
    let third = upload(mine, &card_id, "third.png", png.clone()).await.unwrap();
    let third: Value = serde_json::from_slice(&third.body).unwrap();
    let m = refused(upload(mine, &card_id, "fourth.png", png.clone()).await, "invalid");
    assert!(m.contains("too many"), "{m}");
    assert_eq!(files().await, before + 2, "one file per field is kept: the signer's last and the other signer's");
    assert_eq!(t.count(&acme, id, "Attachment Added").await, 4);

    // Signing with the right answers completes the request; the values are kept as given.
    let (status, done) = t
        .sign(
            &acme,
            mine,
            None,
            answer(values(&[(&plan_id, json!("Pro")), (&colour_id, json!("Blue")), (&seats_id, json!("07")), (&card_id, third["file"].clone())])),
        )
        .await
        .unwrap();
    assert_eq!(status, 200, "{done}");
    let r = t.get(&asha, "SignRequest", id).await;
    let value = |label: &str| r["fields"].as_array().unwrap().iter().find(|f| f["label"] == json!(label)).unwrap()["value"].clone();
    assert_eq!((value("Plan"), value("Colour"), value("Seats")), (json!("Pro"), json!("Blue"), json!("7")), "a number is kept without leading zeros");
    assert_eq!(value("ID card"), third["file"]);
    let (status, _) = t
        .sign(&acme, other, None, json!({"consent": true, "signature": {"text": "Omar Other"}, "values": values(&[(&theirs_id, theirs_file["file"].clone())])}))
        .await
        .unwrap();
    assert_eq!(status, 200);
    let r = t.get(&asha, "SignRequest", id).await;
    assert_eq!(r["status"], json!("Completed"), "{}", r["seal_error"]);
}

#[test]
fn several_documents_are_signed_and_sealed_in_one_request() {
    run(several_documents());
}

/// S11-G3: one request, three documents. A box names the document it is on; the signer reads each one; sealing stamps each
/// with its own values (one certificate, on the main document, listing all three hashes); every document is checked by
/// `verify` and by the public check, and the original of none can be changed after sending.
async fn several_documents() {
    let t = boot(KernelSettings::default()).await;
    let acme = t.tenant("acme", &["sign"], "UTC").await;
    let asha = t.user(&acme, "asha@acme.test", "Asha Rao", &["Sign User"]).await;
    let sys = t.sys(&acme);
    let (one, two, three) = (contract_pdf("One"), contract_pdf("Two"), contract_pdf("Three"));
    let on = |n: i64, mut f: Value| {
        f["document_no"] = json!(n);
        f
    };
    let (tr, ar, o, tw, th) = (&t, &asha, &one, &two, &three);
    let draft = move |fields: Vec<Value>| async move {
        json!({"title": "Three documents", "document": tr.upload(ar, "one.pdf", o).await,
            "more_documents": [{"document": tr.upload(ar, "two.pdf", tw).await}, {"document": tr.upload(ar, "three.pdf", th).await}],
            "signers": [{"role": "Client", "name": "Cleo", "email": "cleo@client.test"}], "fields": fields})
    };
    // A box on a document the request does not have is refused.
    let m = refused(t.k.insert(&asha, "SignRequest", obj(draft(vec![on(4, field("Client", "Signature", "S", 1, 72, 600, 200, 50))]).await)).await, "invalid");
    assert!(m.contains("3 documents"), "{m}");
    let fields = vec![
        field("Client", "Signature", "S1", 1, 72, 600, 200, 50),
        on(2, field("Client", "Text", "Note", 1, 72, 500, 200, 22)),
        on(3, field("Client", "Initials", "Ini", 2, 100, 100, 56, 32)),
    ];
    let id = uid(&t.k.insert(&asha, "SignRequest", obj(draft(fields).await)).await.unwrap());
    let sent = t.action(&asha, id, "send", json!({})).await;
    let token = token_of(sent["signers"][0]["url"].as_str().unwrap());
    let r = t.get(&asha, "SignRequest", id).await;
    assert_eq!(r["more_documents"].as_array().unwrap().len(), 2);
    assert!(r["more_documents"][0]["document_sha256"].is_string() && r["more_documents"][1]["page_count"] == json!(2), "each document is measured");

    // The signer is shown three documents and reads each.
    let (_, view) = t.view(&acme, &token, None).await.unwrap();
    assert_eq!(view["request"]["documents"].as_array().unwrap().len(), 3);
    let on_docs: Vec<i64> = view["fields"].as_array().unwrap().iter().map(|f| f["document_no"].as_i64().unwrap()).collect();
    assert_eq!(on_docs, vec![1, 2, 3], "each box says which document it is on");
    for (n, bytes) in [(1, &one), (2, &two), (3, &three)] {
        let got = t.site(&acme, "GET", &format!("/signing/{token}/document?n={n}"), None, vec![]).await.unwrap();
        assert_eq!(sha(&got.body), sha(bytes), "document {n} is the one sent");
    }
    refused(t.site(&acme, "GET", &format!("/signing/{token}/document?n=4"), None, vec![]).await, "not found");

    let note = field_id(&r, "Note");
    let (status, done) =
        t.sign(&acme, &token, None, json!({"consent": true, "signature": {"text": "Cleo Client"}, "values": {note.clone(): "Seen"}})).await.unwrap();
    assert_eq!((status, done["request_status"].clone()), (200, json!("Completed")), "{done}");
    let r = t.get(&asha, "SignRequest", id).await;
    let sealed: Vec<(String, String)> = std::iter::once(&r)
        .chain(r["more_documents"].as_array().unwrap().iter().map(|d| d.as_object().unwrap()).collect::<Vec<_>>())
        .map(|d| (d["sealed_document"].as_str().unwrap().to_string(), d["sealed_sha256"].as_str().unwrap().to_string()))
        .collect();
    assert_eq!(sealed.iter().map(|x| &x.1).collect::<std::collections::HashSet<_>>().len(), 3, "three different sealed files");
    assert_eq!(t.count(&acme, id, "Completed").await, 1);
    let payload: Value = t.scalar(&acme, "select payload from outbox where tenant_id = $1 and topic = 'sign.request_completed' and record_id = $2", id).await;
    assert_eq!(payload["documents"].as_array().unwrap().len(), 3, "the outbox names every sealed document");

    // Checked: the request's own verify, and a presented second document.
    let verdict = t.action(&asha, id, "verify", json!({})).await;
    assert_eq!(verdict["valid"], json!(true), "{verdict}");
    assert_eq!(verdict["documents"].as_array().unwrap().len(), 2);
    let file_of = |p: &str| -> Uuid { p[7..].parse().unwrap() };
    let second = t.k.read_file(&sys, file_of(&sealed[1].0)).await.unwrap().2;
    let presented = t.verify_pdf(&acme, second.clone()).await;
    assert_eq!((presented["valid"].clone(), presented["request"]["number"].clone()), (json!(true), r["number"].clone()), "{presented}");
    // One changed byte in the third sealed file: the request no longer verifies, and says which.
    let third = file_of(&sealed[2].0);
    let kept = t.store.tamper(third);
    let verdict = t.action(&asha, id, "verify", json!({})).await;
    assert_eq!(verdict["valid"], json!(false));
    assert_eq!(verdict["documents"][1]["sealed"]["ok"], json!(false), "document 3 is the one named");
    assert_eq!(verdict["documents"][0]["sealed"]["ok"], json!(true));
    t.store.restore(third, kept);
    assert_eq!(t.action(&asha, id, "verify", json!({})).await["valid"], json!(true));
}

#[test]
fn the_signing_page_may_be_framed_only_where_the_manager_allowed() {
    run(embedding());
}

/// S4-S1: `allow_embedding` writes the workspace's Embedding for `sign/s` (once, replaced by the next call); the platform
/// then frames the page for those sites only. An origin that is a path or plain http, or no site at all, is refused; a
/// Sign User cannot.
async fn embedding() {
    let t = boot(KernelSettings::default()).await;
    let acme = t.tenant("acme", &["sign"], "UTC").await;
    let mira = t.user(&acme, "mira@acme.test", "Mira Chen", &["Sign Manager"]).await;
    let asha = t.user(&acme, "asha@acme.test", "Asha Rao", &["Sign User"]).await;
    let settings = uid(&t.k.insert(&mira, "SignSettings", obj(json!({"sender_name": "Acme Corp"}))).await.unwrap());
    let allow = |who: &Ctx, origins: &str| {
        let (k, who, origins) = (t.k.clone(), who.clone(), origins.to_string());
        async move { k.run_action(&who, "SignSettings", settings, "allow_embedding", json!({"origins": origins})).await }
    };
    assert!(t.k.embedders(acme.id, "sign/s").await.unwrap().is_empty(), "nowhere before");
    let done = allow(&mira, "https://www.example.com\nhttps://shop.example.org").await.unwrap();
    assert_eq!(done["page"], json!("sign/s"));
    assert_eq!(t.k.embedders(acme.id, "sign/s").await.unwrap(), vec!["https://www.example.com".to_string(), "https://shop.example.org".to_string()]);
    allow(&mira, "https://only.example.net").await.unwrap();
    assert_eq!(t.k.embedders(acme.id, "sign/s").await.unwrap(), vec!["https://only.example.net".to_string()], "replaced, not added to");
    assert_eq!(t.scalar0::<i64>(&acme, "select count(*) from e_embedding where page = 'sign/s'").await, 1, "one record, however often it is set");
    for bad in ["https://www.example.com/path", "http://evil.test", "   ", "not a site"] {
        refused(allow(&mira, bad).await, "invalid");
    }
    assert_eq!(t.k.embedders(acme.id, "sign/s").await.unwrap(), vec!["https://only.example.net".to_string()], "a refused call changes nothing");
    refused(allow(&asha, "https://www.example.com").await, "forbidden");
}

#[test]
fn a_bulk_send_makes_one_request_per_recipient() {
    run(bulk());
}

/// S11-G7: a bulk send is checked whole at `start`, then sent by upkeep a row at a time, each row once however many
/// upkeeps run together (the bulk's and the row's locks, the request and the row's mark in one transaction); a row that
/// fails is recorded and the others go on; the daily limit pauses sending; people are forgotten once their request is made
/// (and a failed row's after a month); cancelling forgets the rest.
async fn bulk() {
    let t = boot(KernelSettings::default()).await;
    let acme = t.tenant("acme", &["sign"], "UTC").await;
    let mira = t.user(&acme, "mira@acme.test", "Mira Chen", &["Sign Manager"]).await;
    let asha = t.user(&acme, "asha@acme.test", "Asha Rao", &["Sign User"]).await;
    let ravi = t.user(&acme, "ravi@acme.test", "Ravi Kumar", &["Sign User"]).await;
    let pdf = contract_pdf("Bulk");
    let template = {
        let draft = json!({"template_name": "Welcome", "title": "Welcome pack", "document": t.upload(&mira, "w.pdf", &pdf).await,
            "roles": [{"role": "Client", "signing_order": 1}], "fields": [field("Client", "Signature", "S", 1, 72, 600, 200, 50)]});
        uid(&t.k.insert(&mira, "SignTemplate", obj(draft)).await.unwrap())
    };
    let table = ferrum_kernel::ddl::q(&acme.admin.schema.entity("SignBulkSend").unwrap().children["recipients"].table);
    let person = |i: usize| json!({"Client": {"name": format!("Person {i}"), "email": format!("p{i}@client.test")}}).to_string();
    let bulk_of = |who: &Ctx, rows: Vec<String>| {
        let (k, who) = (t.k.clone(), who.clone());
        async move {
            let recipients: Vec<Value> = rows.into_iter().map(|s| json!({"signers": s})).collect();
            uid(&k.insert(&who, "SignBulkSend", obj(json!({"title": "Welcome", "template": template.to_string(), "recipients": recipients}))).await.unwrap())
        }
    };
    let start = |who: &Ctx, id: Uuid| {
        let (k, who) = (t.k.clone(), who.clone());
        async move { k.run_action(&who, "SignBulkSend", id, "start", json!({})).await }
    };
    let made = |bulk: Uuid| t.scalar::<i64>(&acme, "select count(*) from e_sign_request where tenant_id = $1 and bulk_send = $2", bulk);

    // Checked whole at start: a role the template lacks, an address that is no address; nothing starts.
    let wrong = bulk_of(&asha, vec![person(1), json!({"Boss": {"name": "B", "email": "b@x.test"}}).to_string()]).await;
    assert!(refused(start(&asha, wrong).await, "invalid").contains("no role"));
    let wrong_mail = bulk_of(&asha, vec![json!({"Client": {"name": "B", "email": "not-an-address"}}).to_string()]).await;
    assert!(refused(start(&asha, wrong_mail).await, "invalid").contains("email address"));
    assert_eq!(t.get(&asha, "SignBulkSend", wrong).await["status"], json!("Draft"));

    // Five recipients; one row is spoiled after it was checked, as a failure at sending would be.
    let id = bulk_of(&asha, (1..=5).map(person).collect()).await;
    assert!(start(&ravi, id).await.is_err(), "another Sign User cannot start it");
    assert!(t.list(&ravi, "SignBulkSend", json!({})).await.is_empty(), "a Sign User sees only their own bulk sends");
    let started = start(&asha, id).await.unwrap();
    assert_eq!((started["status"].clone(), started["rows"].clone()), (json!("Sending"), json!(5)));
    t.sql(&acme, &format!("update {table} set signers = 'not json' where tenant_id = $1 and parent_id = $2 and row_no = 3"), id).await.unwrap();

    // Two upkeeps at the same moment: each row is made once.
    let (a, b) = tokio::join!(ferrum_app_sign::maintenance(&t.k, acme.id), ferrum_app_sign::maintenance(&t.k, acme.id));
    a.expect("upkeep");
    b.expect("upkeep");
    assert_eq!(made(id).await, 4, "four requests, one per recipient that could be made");
    let b = t.get(&asha, "SignBulkSend", id).await;
    assert_eq!((b["status"].clone(), b["sent"].clone(), b["failed"].clone()), (json!("Done With Failures"), json!(4), json!(1)));
    let rows = b["recipients"].as_array().unwrap();
    assert_eq!(rows.iter().map(|r| r["status"].as_str().unwrap()).collect::<Vec<_>>(), vec!["Sent", "Sent", "Failed", "Sent", "Sent"]);
    assert!(rows[2]["error"].as_str().unwrap().contains("JSON"), "the reason is kept: {}", rows[2]["error"]);
    assert!(rows.iter().filter(|r| r["status"] == json!("Sent")).all(|r| r["signers"].is_null() && r["request"].is_string()), "sent rows forget their people");
    assert!(rows[2]["signers"].is_string(), "a failed row keeps its people to be fixed");
    let first = t.get(&asha, "SignRequest", rows[0]["request"].as_str().unwrap().parse().unwrap()).await;
    assert_eq!((first["sender_email"].clone(), first["status"].clone(), first["title"].clone()), (json!("asha@acme.test"), json!("Sent"), json!("Welcome")));
    // A failed row's people are forgotten a month after the bulk send last changed.
    t.sql(&acme, "update e_sign_bulk_send set updated_at = now() - interval '31 days' where tenant_id = $1 and id = $2", id).await.unwrap();
    t.maintenance(&acme).await;
    assert!(t.get(&asha, "SignBulkSend", id).await["recipients"][2]["signers"].is_null());

    // The daily limit pauses sending, and room made later lets it go on.
    let settings = uid(&t.k.insert(&mira, "SignSettings", obj(json!({"sender_name": "Acme", "bulk_per_day": 2}))).await.unwrap());
    let capped = bulk_of(&asha, (6..=9).map(person).collect()).await;
    start(&asha, capped).await.unwrap();
    t.maintenance(&acme).await;
    assert_eq!(
        (made(capped).await, t.get(&asha, "SignBulkSend", capped).await["status"].clone()),
        (0, json!("Sending")),
        "four were made today: the limit of 2 is reached"
    );
    t.k.update(&mira, "SignSettings", settings, obj(json!({"bulk_per_day": 6})), None).await.unwrap();
    t.maintenance(&acme).await;
    assert_eq!(made(capped).await, 2, "room for two more");
    t.k.update(&mira, "SignSettings", settings, obj(json!({"bulk_per_day": 100})), None).await.unwrap();
    t.maintenance(&acme).await;
    assert_eq!((made(capped).await, t.get(&asha, "SignBulkSend", capped).await["status"].clone()), (4, json!("Done")));

    // Stopping forgets the rows not yet sent and sends no more.
    let stopped = bulk_of(&asha, (10..=12).map(person).collect()).await;
    start(&asha, stopped).await.unwrap();
    t.k.run_action(&asha, "SignBulkSend", stopped, "stop", json!({})).await.unwrap();
    t.maintenance(&acme).await;
    let b = t.get(&asha, "SignBulkSend", stopped).await;
    assert_eq!((b["status"].clone(), made(stopped).await), (json!("Cancelled"), 0));
    assert!(b["recipients"].as_array().unwrap().iter().all(|r| r["signers"].is_null() && r["status"] == json!("Waiting")));
}

#[test]
fn a_finished_document_carries_the_workspaces_digital_seal() {
    run(digital_seal());
}

/// S11-G5: every finished document is signed digitally by the workspace's key (one key per workspace, made once however
/// many seals start at the same moment); the request's verify and the public check find the seal valid, name who it says it
/// is, and find it broken by one changed byte; the audit trail travels inside the file; a seal key that does not match its
/// certificate, or is an RSA key, is refused when saved; choosing no seal gives a file without one.
async fn digital_seal() {
    use p256::pkcs8::{EncodePrivateKey, LineEnding};
    let t = boot(KernelSettings::default()).await;
    let acme = t.tenant("acme", &["sign"], "UTC").await;
    let asha = t.user(&acme, "asha@acme.test", "Asha Rao", &["Sign User"]).await;
    let mira = t.user(&acme, "mira@acme.test", "Mira Chen", &["Sign Manager"]).await;
    let sys = t.sys(&acme);
    let pdf = contract_pdf("Sealed");

    // One key, made once: two first seals at the same moment get the same certificate.
    let (a, b) = tokio::join!(ferrum_app_sign::workspace_seal(&t.k, acme.id), ferrum_app_sign::workspace_seal(&t.k, acme.id));
    let (a, b) = (a.unwrap().expect("a seal"), b.unwrap().expect("a seal"));
    assert_eq!(a, b, "one key serves both");
    assert_eq!(ferrum_app_sign::workspace_seal(&t.k, acme.id).await.unwrap(), Some(a.clone()), "and the next call");

    let (id, token) = t.one_signer_request(&asha, "Sealed", &pdf).await;
    let (status, done) = t.sign(&acme, &token, None, json!({"consent": true, "signature": {"text": "Cleo Client"}})).await.unwrap();
    assert_eq!((status, done["request_status"].clone()), (200, json!("Completed")), "{done}");
    let r = t.get(&asha, "SignRequest", id).await;
    let file: Uuid = r["sealed_document"].as_str().unwrap()[7..].parse().unwrap();
    let bytes = t.k.read_file(&sys, file).await.unwrap().2;
    let verdict = t.action(&asha, id, "verify", json!({})).await;
    assert_eq!(verdict["valid"], json!(true), "{verdict}");
    assert_eq!(verdict["seal"]["valid"], json!(true), "{verdict}");
    assert_eq!(verdict["seal"]["certificate_sha256"], json!(a), "the file was sealed with the workspace's key");
    assert!(verdict["seal"]["subject"].as_str().unwrap().contains("Sign seal"));
    let presented = t.verify_pdf(&acme, bytes.clone()).await;
    assert_eq!((presented["valid"].clone(), presented["seal"]["valid"].clone()), (json!(true), json!(true)), "{presented}");

    // The audit trail travels inside the file, as the chain has it.
    let doc = lopdf::Document::load_mem(&bytes).expect("the sealed file reads");
    let names = doc
        .catalog()
        .unwrap()
        .get(b"Names")
        .and_then(|n| doc.dereference(n))
        .unwrap()
        .1
        .as_dict()
        .unwrap()
        .get(b"EmbeddedFiles")
        .and_then(|n| doc.dereference(n))
        .unwrap()
        .1
        .as_dict()
        .unwrap()
        .get(b"Names")
        .unwrap()
        .as_array()
        .unwrap()
        .clone();
    let spec = doc
        .dereference(&names[1])
        .unwrap()
        .1
        .as_dict()
        .unwrap()
        .get(b"EF")
        .and_then(|e| doc.dereference(e))
        .unwrap()
        .1
        .as_dict()
        .unwrap()
        .get(b"F")
        .and_then(|f| doc.dereference(f))
        .unwrap()
        .1
        .as_stream()
        .unwrap()
        .clone();
    let embedded: Value = serde_json::from_slice(&spec.content).expect("the trail is JSON");
    let events = embedded["events"].as_array().unwrap();
    let kept: Vec<String> = {
        let mut tx = t.k.db.tenant_tx(acme.id).await.unwrap();
        let rows = sqlx::query_scalar("select event_hash from e_sign_event where tenant_id = $1 and request = $2 order by seq")
            .bind(acme.id)
            .bind(id)
            .fetch_all(&mut *tx)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        rows
    };
    assert_eq!(
        events.iter().map(|e| e["event_hash"].as_str().unwrap().to_string()).collect::<Vec<_>>(),
        kept[..kept.len() - 1].to_vec(),
        "every event up to the completion, as chained"
    );

    // One changed byte: the request no longer verifies, the file presented is refused.
    let mut changed = bytes.clone();
    let at = changed.len() / 3;
    changed[at] ^= 0x01;
    assert_eq!(t.verify_pdf(&acme, changed).await["valid"], json!(false));
    let kept_bytes = t.store.tamper(file);
    let verdict = t.action(&asha, id, "verify", json!({})).await;
    assert_eq!((verdict["valid"].clone(), verdict["seal"]["valid"].clone()), (json!(false), json!(false)), "{verdict}");
    t.store.restore(file, kept_bytes);

    // A key set in Sign Settings: it must be one of the two curves and match its certificate; nothing changes when refused.
    let settings = uid(&t.list(&mira, "SignSettings", json!({})).await[0]);
    let certificate = t.get(&mira, "SignSettings", settings).await["seal_certificate"].as_str().unwrap().to_string();
    let foreign = p256::SecretKey::random(&mut p256::elliptic_curve::rand_core::OsRng).to_pkcs8_pem(LineEnding::LF).unwrap().to_string();
    let m = refused(t.k.update(&mira, "SignSettings", settings, obj(json!({"seal_key": foreign, "seal_certificate": certificate})), None).await, "invalid");
    assert!(m.contains("not for the seal key"), "{m}");
    let rsa = "-----BEGIN PRIVATE KEY-----\nMIIBVQIBADANBgkqhkiG9w0BAQEFAASCAT8wggE7AgEAAkEAu7vjTXgGDKrVZyeX\n-----END PRIVATE KEY-----\n";
    let m = refused(t.k.update(&mira, "SignSettings", settings, obj(json!({"seal_key": rsa, "seal_certificate": certificate})), None).await, "invalid");
    assert!(m.contains("RSA keys are not accepted"), "{m}");
    refused(t.k.update(&mira, "SignSettings", settings, obj(json!({"seal_key": foreign})), None).await, "invalid");
    assert_eq!(ferrum_app_sign::workspace_seal(&t.k, acme.id).await.unwrap(), Some(a), "the seal is as it was");

    // No seal chosen: the file is as it was before, signed by nobody.
    t.k.update(&mira, "SignSettings", settings, obj(json!({"seal": "None"})), None).await.unwrap();
    assert_eq!(ferrum_app_sign::workspace_seal(&t.k, acme.id).await.unwrap(), None);
    let (plain, token) = t.one_signer_request(&asha, "Not sealed", &pdf).await;
    t.sign(&acme, &token, None, json!({"consent": true, "signature": {"text": "Cleo Client"}})).await.unwrap();
    let verdict = t.action(&asha, plain, "verify", json!({})).await;
    assert_eq!((verdict["valid"].clone(), verdict["seal"].clone()), (json!(true), Value::Null), "{verdict}");
}
