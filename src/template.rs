//! Sign templates: a reusable PDF with signer roles and field placements. `create_request` makes a
//! draft Sign Request from one: the roles become signers (prefilled from the template's defaults
//! or the action's `signers`), the fields are copied, and the PDF is copied to the request.

use crate::document::{self, role_key};
use crate::request::{check_reference, obj};
use crate::{doc_id, elevated, rows, s};
use async_trait::async_trait;
use ferrum_kernel::{Action, Doc, DocHook, Error, FieldError, HookCtx, Result};
use serde_json::{Map, Value, json};
use std::collections::HashMap;

pub struct TemplateHook;

#[async_trait]
impl DocHook for TemplateHook {
    async fn before_validate(&self, hc: &mut HookCtx<'_>, doc: &mut Doc) -> Result<()> {
        document::derive(hc, doc).await
    }

    async fn validate(&self, _hc: &mut HookCtx<'_>, doc: &Doc) -> Result<()> {
        let mut errors = Vec::new();
        let roles = document::check_roles(doc, "roles", &mut errors);
        document::check_fields(doc, &roles, &mut errors);
        if errors.is_empty() { Ok(()) } else { Err(Error::Validation(errors)) }
    }

    async fn action(&self, hc: &mut HookCtx<'_>, name: &str, doc: &Doc, params: &Value) -> Result<Option<Value>> {
        if name != "create_request" {
            return Ok(None);
        }
        create_request(hc, doc, params).await.map(Some)
    }
}

/// Params (all optional): `title`, `message`, `signing_mode`, `expires_on`, `reference_entity`,
/// `reference_id`, `signers: [{role, name, email, access_code}]`. Returns the new draft.
async fn create_request(hc: &mut HookCtx<'_>, doc: &Doc, params: &Value) -> Result<Value> {
    let ctx = hc.ctx;
    ctx.require("SignRequest", Action::Create)?;
    if crate::b(doc, "disabled") {
        return Err(Error::invalid("this template is disabled"));
    }
    let p = match params {
        Value::Null => Map::new(),
        Value::Object(m) => m.clone(),
        _ => return Err(Error::invalid("the parameters must be a JSON object")),
    };
    let roles = rows(doc, "roles");
    let mut given: HashMap<String, Map<String, Value>> = HashMap::new();
    let mut errors = Vec::new();
    match p.get("signers") {
        None | Some(Value::Null) => {}
        Some(Value::Array(list)) => {
            for (i, v) in list.iter().enumerate() {
                let Some(m) = v.as_object() else {
                    errors.push(FieldError::new("signers", "each signer is an object").in_row("signers", i));
                    continue;
                };
                let role = m.get("role").and_then(Value::as_str).map(role_key).unwrap_or_default();
                if !roles.iter().any(|r| role_key(s(r, "role").unwrap_or_default()) == role) {
                    errors.push(FieldError::new("role", format!("the template has no role {role}")).in_row("signers", i));
                } else if given.insert(role, m.clone()).is_some() {
                    errors.push(FieldError::new("role", "the role is given twice").in_row("signers", i));
                }
            }
        }
        Some(_) => errors.push(FieldError::new("signers", "must be a list of {role, name, email}")),
    }
    let text = |k: &str| p.get(k).filter(|v| !v.is_null()).cloned();
    let mut input = Doc::new();
    for k in ["reference_entity", "reference_id"] {
        if let Some(v) = text(k) {
            input.insert(k.into(), v);
        }
    }
    // The person creating the request must be able to read what it is linked to.
    check_reference(hc.kernel, ctx, hc.tx, &input, &mut errors).await;
    if !errors.is_empty() {
        return Err(Error::Validation(errors));
    }
    let signers: Vec<Value> = roles
        .iter()
        .map(|r| {
            let g = given.get(&role_key(s(r, "role").unwrap_or_default()));
            let pick = |field: &str, default: &str| {
                g.and_then(|m| m.get(field)).filter(|v| !v.is_null()).cloned().unwrap_or_else(|| r.get(default).cloned().unwrap_or(Value::Null))
            };
            json!({
                "role": s(r, "role"),
                "name": pick("name", "default_name"),
                "email": pick("email", "default_email"),
                "signing_order": r.get("signing_order").cloned().unwrap_or(json!(1)),
                "access_code": g.and_then(|m| m.get("access_code")).cloned().unwrap_or(Value::Null),
            })
        })
        .collect();
    let fields: Vec<Value> = rows(doc, "fields")
        .iter()
        .map(|f| {
            let mut row = Map::new();
            for k in ["role", "field_type", "label", "page", "x", "y", "width", "height", "required"] {
                row.insert(k.into(), f.get(k).cloned().unwrap_or(Value::Null));
            }
            Value::Object(row)
        })
        .collect();
    input.insert("title".into(), text("title").unwrap_or_else(|| json!(s(doc, "title").or(s(doc, "template_name")))));
    input.insert("message".into(), text("message").unwrap_or_else(|| doc.get("message").cloned().unwrap_or(Value::Null)));
    input.insert("signing_mode".into(), text("signing_mode").unwrap_or_else(|| doc.get("signing_mode").cloned().unwrap_or(json!("Sequential"))));
    if let Some(v) = text("expires_on") {
        input.insert("expires_on".into(), v);
    }
    input.insert("document".into(), doc.get("document").cloned().unwrap_or(Value::Null));
    input.insert("template".into(), json!(doc_id(doc)?));
    input.insert("signers".into(), Value::Array(signers));
    input.insert("fields".into(), Value::Array(fields));
    // Elevated: the template link is read-only and the template's PDF is copied to the request;
    // the caller's create permission was checked above and the request is theirs.
    let created = hc.kernel.insert_tx(&elevated(ctx), hc.tx, "SignRequest", obj(Value::Object(input))).await?;
    Ok(json!({"entity": "SignRequest", "id": created.get("id"), "number": created.get("number")}))
}
