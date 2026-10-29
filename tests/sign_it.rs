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

// ───────────────────────────── harness ─────────────────────────────

struct T {
    k: Arc<Kernel>,
    store: Arc<MemStore>,
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
        let hexed = self.k.secret(&mut tx, t.id, "SignSettings", id, "signing_key").await.unwrap().expect("signing key");
        tx.commit().await.unwrap();
        hex::decode(hexed).unwrap()
    }

    /// One call to the public site, as the server hands it to the package.
    async fn site(&self, t: &Tenant, method: &str, path: &str, code: Option<&str>, body: Vec<u8>) -> Result<SiteResponse, Error> {
        let mut headers = Map::new();
        headers.insert("user-agent".into(), json!("Mozilla/5.0 (it) Firefox/140.0"));
        if let Some(c) = code {
            headers.insert("x-access-code".into(), json!(c));
        }
        let req = SiteRequest {
            tenant: t.id,
            user: None,
            method: method.into(),
            path: path.into(),
            query: Map::new(),
            headers,
            body,
            client_ip: "203.0.113.7".into(),
        };
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

    async fn maintenance(&self) {
        ferrum_app_sign::maintenance(&self.k).await.expect("maintenance");
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

#[test]
fn sign_end_to_end() {
    // Same runtime shape as the server: unoptimized async hook chains need more than 2 MiB.
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().thread_stack_size(16 << 20).build().unwrap();
    rt.block_on(async { tokio::spawn(scenario()).await.unwrap() });
}

async fn scenario() {
    let url = std::env::var("FERRUM_SIGN_TEST_DATABASE_URL").unwrap_or_else(|_| "postgres://ferrum:ferrum@localhost:5432/ferrum_sign_test".into());
    let db = Db::connect(DbConfig { url, max_connections: 16, ..Default::default() }).await.expect("connect");
    sqlx::raw_sql("drop schema if exists public cascade; create schema public;").execute(&db.pool).await.expect("reset");
    let mut packages = ferrum_domain::packages().expect("platform packages");
    packages.extend(ferrum_app_sign::packages().expect("app packages"));
    packages.extend(ferrum_app_comms::packages().expect("mail packages"));
    let catalog = Catalog::from_packages(packages).expect("catalog");
    let mut hooks = HookRegistry::default();
    ferrum_domain::register(&mut hooks);
    ferrum_app_sign::register(&mut hooks);
    ferrum_app_comms::register(&mut hooks);
    for source in ferrum_app_sign::reports() {
        ferrum_domain::reports::add_source(source);
    }
    let settings = KernelSettings { secret_key: Some(ferrum_kernel::secret::SecretKey::new([9u8; 32])), ..Default::default() };
    let k = Kernel::boot(db, catalog, hooks, settings, None).await.expect("boot");
    let store = Arc::new(MemStore::default());
    k.set_file_store(store.clone());
    let t = T { k: k.clone(), store };

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
    t.maintenance().await;
    t.maintenance().await;
    assert_eq!(t.get(&asha, "SignRequest", quote_id).await["status"], json!("Expired"));
    assert_eq!(t.trail(&acme, quote_id).await.iter().filter(|e| *e == "Expired").count(), 1, "expired once");
    assert!(t.notifications(&acme, &asha).await.iter().any(|(title, _)| title.ends_with("expired before everyone signed")));

    // ───────────── reminders on cadence, each exactly once ─────────────
    let deal =
        t.k.insert(&asha, "SignRequest", obj(one("Deal", "Parallel", two.clone(), two_fields.clone(), t.upload(&asha, "deal.pdf", &pdf).await))).await.unwrap();
    let deal_id = uid(&deal);
    t.action(&asha, deal_id, "send", json!({})).await;
    t.maintenance().await;
    assert_eq!(t.count(&acme, deal_id, "Reminded").await, 0, "not due yet");
    t.sql(&acme, &format!("update {table} set sent_at = now() - interval '25 hours' where tenant_id = $1 and parent_id = $2"), deal_id).await.unwrap();
    let (r1, r2) = tokio::join!(ferrum_app_sign::maintenance(&t.k), ferrum_app_sign::maintenance(&t.k));
    r1.unwrap();
    r2.unwrap();
    t.maintenance().await;
    assert_eq!(t.count(&acme, deal_id, "Reminded").await, 2, "one reminder for each of the two signers, even with concurrent runs");
    let deal = t.get(&asha, "SignRequest", deal_id).await;
    assert_eq!((signer_row(&deal, "A")["reminders_sent"].clone(), signer_row(&deal, "B")["reminders_sent"].clone()), (json!(1), json!(1)));
    assert!(t.notifications(&acme, &asha).await.iter().any(|(title, _)| title == "Ann Lee has not signed yet" || title.starts_with("Ann Lee has not signed")));
    t.sql(&acme, &format!("update {table} set last_reminded_at = now() - interval '25 hours' where tenant_id = $1 and parent_id = $2"), deal_id).await.unwrap();
    t.maintenance().await;
    t.maintenance().await;
    assert_eq!(t.count(&acme, deal_id, "Reminded").await, 4, "the second round");
    t.sql(&acme, &format!("update {table} set last_reminded_at = now() - interval '25 hours' where tenant_id = $1 and parent_id = $2"), deal_id).await.unwrap();
    t.maintenance().await;
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

    // ───────────── a workspace with Mail: links and sealed copies are emailed ─────────────
    let globex = t.tenant("globex", &["sign", "comms"], "UTC").await;
    let gina = t.user(&globex, "gina@globex.test", "Gina Ortiz", &["Sign User"]).await;
    let gdoc = t.upload(&gina, "po.pdf", &pdf).await;
    let po = t
        .k
        .insert(&gina, "SignRequest", obj(json!({"title": "Purchase Order", "document": gdoc, "signers": [{"role": "Supplier", "name": "Sam Supplier", "email": "sam@supplier.test"}], "fields": [field("Supplier", "Signature", "Sign", 1, 72, 600, 200, 50)]})))
        .await
        .unwrap();
    let po_id = uid(&po);
    assert!(refused(t.k.run_action(&gina, "SignRequest", po_id, "send", json!({})).await, "invalid").contains("Public URL"), "emailed links must be absolute");
    // The first send created the settings row with the signing key: complete it.
    let gsettings = t.list(&globex.admin, "SignSettings", json!({})).await;
    assert_eq!(gsettings.len(), 1);
    let stored = t.get(&globex.admin, "SignSettings", uid(&gsettings[0])).await;
    assert_eq!(stored["signing_key"], json!(true), "the key is a secret: reads only say it is set");
    t.k.update(&globex.admin, "SignSettings", uid(&gsettings[0]), obj(json!({"public_url": "https://erp.globex.test/"})), None).await.unwrap();
    let sent = t.action(&gina, po_id, "send", json!({})).await;
    assert_eq!((sent["signers"][0]["emailed"].clone(), sent["signers"][0].get("url").is_none()), (json!(true), true), "emailed, so no link in the answer");
    let gsys = t.sys(&globex);
    let mails = t.list(&gsys, "Communication", json!({"reference_entity": "SignRequest", "reference_id": po_id.to_string()})).await;
    assert_eq!(mails.len(), 1);
    assert_eq!((mails[0]["status"].clone(), mails[0]["channel"].clone()), (json!("Queued"), json!("Email")));
    assert!(mails[0]["recipients"].as_str().unwrap().contains("sam@supplier.test"));
    let content = mails[0]["content"].as_str().unwrap();
    let start = content.find("https://erp.globex.test/api/site/globex/sign/s/").expect("absolute signing link in the email");
    let po_token: String = content[start..].trim_start_matches("https://erp.globex.test/api/site/globex/sign/s/").chars().take(43).collect();
    refused(t.view(&acme, &po_token, None).await, "not found");
    let (_, done) = t.sign(&globex, &po_token, None, json!({"consent": true, "signature": {"image": drawn(500, 150)}})).await.unwrap();
    assert_eq!(done["request_status"], json!("Completed"));
    let po = t.get(&gina, "SignRequest", po_id).await;
    let mails = t.list(&gsys, "Communication", json!({"reference_entity": "SignRequest", "reference_id": po_id.to_string()})).await;
    let copy = mails.iter().find(|m| m["subject"].as_str().unwrap().starts_with("Signed:")).expect("the signer is sent a copy");
    assert_eq!(copy["attachments"][0]["id"].as_str().unwrap(), &po["sealed_document"].as_str().unwrap()[7..], "the sealed PDF is attached");
    assert!(copy["content"].as_str().unwrap().contains(po["sealed_sha256"].as_str().unwrap()));
}
