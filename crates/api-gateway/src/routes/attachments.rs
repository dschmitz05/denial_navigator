//! Denial attachments and appeal packet assembly (FB-15).
//!
//! Files attached to a denial (visit notes, an authorization, a remittance
//! excerpt) are stored through the shared object-storage backend, keyed by
//! `denial_attachments.id`, same pattern as a knowledge document's source
//! file. Every read goes through the request-audit middleware like any other
//! route, so a download is already logged; nothing extra is needed for that.
//!
//! The appeal packet assembles the draft letter, a claim/remittance summary
//! and an index of the denial's attachments into one PDF (`crate::pdf`),
//! stored the same way and replaced (not versioned) on regeneration. It
//! starts `draft`; approving it only marks the content reviewed, it does not
//! submit anything to the payer — that is recorded separately via
//! `PATCH /appeals/{id}` (`submission_method`, `payer_confirmation_number`).

use axum::extract::{Multipart, Path, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use denial_auth::rbac::Principal;
use denial_common::error::AppError;
use denial_storage::ObjectKey;
use serde_json::Value;
use sqlx::Row;
use uuid::Uuid;

use super::scope::organization_id;
use crate::pdf::{self, PacketBlock};
use crate::state::AppState;

const MAX_ATTACHMENT_BYTES: usize = 25 * 1024 * 1024;

fn user_id(principal: &Principal) -> Option<Uuid> {
    principal
        .user_id
        .as_deref()
        .and_then(|id| Uuid::parse_str(id).ok())
}

/// The denial's id if it belongs to the caller's organization, else 404 —
/// checked once up front so every handler below fails the same way for a
/// cross-organization or nonexistent denial.
async fn owned_denial(
    pool: &sqlx::PgPool,
    denial_id: Uuid,
    organization_id: Uuid,
) -> Result<(), AppError> {
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM denials d JOIN claims c ON c.id = d.claim_id \
         WHERE d.id = $1 AND c.organization_id = $2)",
    )
    .bind(denial_id)
    .bind(organization_id)
    .fetch_one(pool)
    .await
    .map_err(AppError::Db)?;
    if exists {
        Ok(())
    } else {
        Err(AppError::NotFound)
    }
}

pub async fn list_attachments(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(denial_id): Path<Uuid>,
) -> Result<Json<Vec<Value>>, AppError> {
    let organization_id = organization_id(&principal)?;
    owned_denial(&state.pool, denial_id, organization_id).await?;
    let rows = sqlx::query(
        "SELECT da.id, da.filename, da.content_type, da.size_bytes, da.created_at, \
                u.full_name AS uploaded_by_name \
         FROM denial_attachments da \
         LEFT JOIN users u ON u.id = da.uploaded_by \
         WHERE da.denial_id = $1 AND da.organization_id = $2 \
         ORDER BY da.created_at DESC",
    )
    .bind(denial_id)
    .bind(organization_id)
    .fetch_all(&state.pool)
    .await
    .map_err(AppError::Db)?;
    Ok(Json(
        rows.iter()
            .map(|r| {
                serde_json::json!({
                    "id": r.get::<Uuid, _>("id").to_string(),
                    "filename": r.get::<String, _>("filename"),
                    "content_type": r.get::<String, _>("content_type"),
                    "size_bytes": r.get::<i64, _>("size_bytes"),
                    "created_at": r.get::<chrono::DateTime<chrono::Utc>, _>("created_at").to_rfc3339(),
                    "uploaded_by_name": r.get::<Option<String>, _>("uploaded_by_name"),
                })
            })
            .collect(),
    ))
}

pub async fn upload_attachment(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(denial_id): Path<Uuid>,
    mut multipart: Multipart,
) -> Result<Response, AppError> {
    let organization_id = organization_id(&principal)?;
    owned_denial(&state.pool, denial_id, organization_id).await?;

    let mut raw: Vec<u8> = Vec::new();
    let mut filename: Option<String> = None;
    let mut content_type = "application/octet-stream".to_string();
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|_| AppError::BadRequest("Invalid multipart form data".into()))?
    {
        if field.file_name().is_none() && field.name() != Some("file") {
            continue;
        }
        if filename.is_none() {
            if let Some(name) = field.file_name() {
                filename = Some(name.to_string());
            }
            if let Some(ct) = field.content_type() {
                content_type = ct.to_string();
            }
        }
        let chunk = field
            .bytes()
            .await
            .map_err(|e| AppError::Internal(e.to_string()))?;
        raw.extend_from_slice(&chunk);
        if raw.len() > MAX_ATTACHMENT_BYTES {
            return Err(AppError::BadRequest(format!(
                "File too large: maximum {} MB",
                MAX_ATTACHMENT_BYTES / (1024 * 1024)
            )));
        }
    }
    if raw.is_empty() {
        return Err(AppError::BadRequest("File is empty".into()));
    }
    let filename = filename.unwrap_or_else(|| "attachment".to_string());
    let size = raw.len() as i64;

    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO denial_attachments \
            (organization_id, denial_id, filename, content_type, size_bytes, storage_key, uploaded_by) \
         VALUES ($1, $2, $3, $4, $5, '', $6) RETURNING id",
    )
    .bind(organization_id)
    .bind(denial_id)
    .bind(&filename)
    .bind(&content_type)
    .bind(size)
    .bind(user_id(&principal))
    .fetch_one(&state.pool)
    .await
    .map_err(AppError::Db)?;

    let storage_key = ObjectKey::parse(format!("denial-attachments/{denial_id}/{id}"))
        .map_err(|e| AppError::Internal(e.to_string()))?;
    state
        .object_storage
        .put(&storage_key, &raw)
        .map_err(|e| AppError::Internal(format!("Could not store attachment: {e}")))?;
    sqlx::query("UPDATE denial_attachments SET storage_key = $2 WHERE id = $1")
        .bind(id)
        .bind(storage_key.as_str())
        .execute(&state.pool)
        .await
        .map_err(AppError::Db)?;

    Ok((
        StatusCode::CREATED,
        Json(serde_json::json!({
            "id": id.to_string(),
            "filename": filename,
            "content_type": content_type,
            "size_bytes": size,
        })),
    )
        .into_response())
}

pub async fn download_attachment(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path((denial_id, attachment_id)): Path<(Uuid, Uuid)>,
) -> Result<Response, AppError> {
    let organization_id = organization_id(&principal)?;
    let row = sqlx::query(
        "SELECT filename, content_type, storage_key FROM denial_attachments \
         WHERE id = $1 AND denial_id = $2 AND organization_id = $3",
    )
    .bind(attachment_id)
    .bind(denial_id)
    .bind(organization_id)
    .fetch_optional(&state.pool)
    .await
    .map_err(AppError::Db)?
    .ok_or(AppError::NotFound)?;
    let filename: String = row.get("filename");
    let content_type: String = row.get("content_type");
    let storage_key = ObjectKey::parse(row.get::<String, _>("storage_key"))
        .map_err(|e| AppError::Internal(e.to_string()))?;
    let bytes = state
        .object_storage
        .get(&storage_key)
        .map_err(|_| AppError::NotFound)?;

    Ok((
        [
            (header::CONTENT_TYPE, content_type),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{}\"", filename.replace('"', "")),
            ),
        ],
        bytes,
    )
        .into_response())
}

pub async fn delete_attachment(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path((denial_id, attachment_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<Value>, AppError> {
    let organization_id = organization_id(&principal)?;
    let row = sqlx::query(
        "DELETE FROM denial_attachments WHERE id = $1 AND denial_id = $2 AND organization_id = $3 \
         RETURNING storage_key",
    )
    .bind(attachment_id)
    .bind(denial_id)
    .bind(organization_id)
    .fetch_optional(&state.pool)
    .await
    .map_err(AppError::Db)?
    .ok_or(AppError::NotFound)?;
    if let Ok(key) = ObjectKey::parse(row.get::<String, _>("storage_key")) {
        let _ = state.object_storage.delete(&key);
    }
    Ok(Json(
        serde_json::json!({ "deleted": attachment_id.to_string() }),
    ))
}

/// Builds the packet content: claim/remittance summary, the draft letter (if
/// one exists), and the attachment index. Never fails for missing pieces —
/// an appeal queued before an analysis ran still gets a packet, just a
/// shorter one, because the point is what a human can review and mail, not a
/// complete data set.
async fn build_packet_blocks(
    pool: &sqlx::PgPool,
    appeal_id: Uuid,
    organization_id: Uuid,
) -> Result<(String, Vec<PacketBlock>), AppError> {
    let row = sqlx::query(
        "SELECT c.claim_number, c.patient_name, c.payer_name, c.total_charge::float8, \
                c.service_from, d.cpt_code, d.carc_code, d.charge_amount::float8, \
                cc.description AS carc_description, \
                aq.resolution_type, aa.draft_appeal_letter, aa.explanation \
         FROM appeals_queue aq \
         JOIN denials d ON d.id = aq.denial_id \
         JOIN claims c ON c.id = d.claim_id \
         LEFT JOIN carc_codes cc ON cc.code = d.carc_code \
         LEFT JOIN ai_analyses aa ON aa.id = aq.ai_analysis_id \
         WHERE aq.id = $1 AND c.organization_id = $2",
    )
    .bind(appeal_id)
    .bind(organization_id)
    .fetch_optional(pool)
    .await
    .map_err(AppError::Db)?
    .ok_or(AppError::NotFound)?;

    let claim_number: String = row.try_get("claim_number").unwrap_or_default();
    let mut blocks = vec![
        PacketBlock::Heading("Claim Summary".into()),
        PacketBlock::Field("Claim number".into(), claim_number.clone()),
        PacketBlock::Field(
            "Patient".into(),
            row.try_get::<Option<String>, _>("patient_name")
                .ok()
                .flatten()
                .unwrap_or_else(|| "-".into()),
        ),
        PacketBlock::Field(
            "Payer".into(),
            row.try_get::<Option<String>, _>("payer_name")
                .ok()
                .flatten()
                .unwrap_or_else(|| "-".into()),
        ),
        PacketBlock::Field(
            "Date of service".into(),
            row.try_get::<Option<chrono::NaiveDate>, _>("service_from")
                .ok()
                .flatten()
                .map(|d| d.to_string())
                .unwrap_or_else(|| "-".into()),
        ),
        PacketBlock::Field(
            "CPT code".into(),
            row.try_get::<Option<String>, _>("cpt_code")
                .ok()
                .flatten()
                .unwrap_or_else(|| "-".into()),
        ),
        PacketBlock::Field(
            "Denied amount".into(),
            row.try_get::<Option<f64>, _>("charge_amount")
                .ok()
                .flatten()
                .map(|a| format!("${a:.2}"))
                .unwrap_or_else(|| "-".into()),
        ),
    ];
    let carc = row.try_get::<Option<String>, _>("carc_code").ok().flatten();
    let carc_desc = row
        .try_get::<Option<String>, _>("carc_description")
        .ok()
        .flatten();
    if let Some(carc) = carc {
        blocks.push(PacketBlock::Field(
            "Denial reason (CARC)".into(),
            match carc_desc {
                Some(d) => format!("{carc} - {d}"),
                None => carc,
            },
        ));
    }
    blocks.push(PacketBlock::Spacer);

    if let Some(letter) = row
        .try_get::<Option<String>, _>("draft_appeal_letter")
        .ok()
        .flatten()
        .filter(|s| !s.trim().is_empty())
    {
        blocks.push(PacketBlock::Heading("Draft Appeal Letter".into()));
        blocks.push(PacketBlock::Paragraph(letter));
        blocks.push(PacketBlock::Spacer);
    } else if let Some(explanation) = row
        .try_get::<Option<String>, _>("explanation")
        .ok()
        .flatten()
    {
        blocks.push(PacketBlock::Heading("Analysis".into()));
        blocks.push(PacketBlock::Paragraph(explanation));
        blocks.push(PacketBlock::Spacer);
    }

    let denial_id: Uuid = sqlx::query_scalar("SELECT denial_id FROM appeals_queue WHERE id = $1")
        .bind(appeal_id)
        .fetch_one(pool)
        .await
        .map_err(AppError::Db)?;
    let attachment_names: Vec<String> = sqlx::query_scalar(
        "SELECT filename FROM denial_attachments WHERE denial_id = $1 AND organization_id = $2 \
         ORDER BY created_at",
    )
    .bind(denial_id)
    .bind(organization_id)
    .fetch_all(pool)
    .await
    .map_err(AppError::Db)?;
    blocks.push(PacketBlock::Heading("Attachment Index".into()));
    if attachment_names.is_empty() {
        blocks.push(PacketBlock::Paragraph(
            "No supporting documents are attached to this denial.".into(),
        ));
    } else {
        blocks.push(PacketBlock::List(attachment_names));
    }

    Ok((claim_number, blocks))
}

/// (Re)generates the packet. Replaces any previous one for this appeal —
/// a stale draft naming the wrong attachments is worse than losing the old
/// file — and always resets status to draft, since regenerating changes the
/// content a previous approval reviewed.
pub async fn generate_packet(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(appeal_id): Path<Uuid>,
) -> Result<Json<Value>, AppError> {
    let organization_id = organization_id(&principal)?;
    let (claim_number, blocks) =
        build_packet_blocks(&state.pool, appeal_id, organization_id).await?;
    let title = format!("Appeal Packet - {claim_number}");
    let pdf_bytes = pdf::render(&title, &blocks).map_err(AppError::Internal)?;

    let storage_key = ObjectKey::parse(format!("appeal-packets/{appeal_id}"))
        .map_err(|e| AppError::Internal(e.to_string()))?;
    state
        .object_storage
        .put(&storage_key, &pdf_bytes)
        .map_err(|e| AppError::Internal(format!("Could not store packet: {e}")))?;

    let row = sqlx::query(
        "INSERT INTO appeal_packets (organization_id, appeal_id, storage_key, generated_by) \
         VALUES ($1, $2, $3, $4) \
         ON CONFLICT (appeal_id) DO UPDATE SET \
             storage_key = EXCLUDED.storage_key, status = 'draft', \
             generated_by = EXCLUDED.generated_by, generated_at = NOW(), \
             approved_by = NULL, approved_at = NULL \
         RETURNING id, status, generated_at",
    )
    .bind(organization_id)
    .bind(appeal_id)
    .bind(storage_key.as_str())
    .bind(user_id(&principal))
    .fetch_one(&state.pool)
    .await
    .map_err(AppError::Db)?;

    Ok(Json(serde_json::json!({
        "id": row.get::<Uuid, _>("id").to_string(),
        "status": row.get::<String, _>("status"),
        "generated_at": row.get::<chrono::DateTime<chrono::Utc>, _>("generated_at").to_rfc3339(),
        "size_bytes": pdf_bytes.len(),
    })))
}

pub async fn get_packet(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(appeal_id): Path<Uuid>,
) -> Result<Json<Value>, AppError> {
    let organization_id = organization_id(&principal)?;
    let row = sqlx::query(
        "SELECT status, generated_at, approved_at FROM appeal_packets \
         WHERE appeal_id = $1 AND organization_id = $2",
    )
    .bind(appeal_id)
    .bind(organization_id)
    .fetch_optional(&state.pool)
    .await
    .map_err(AppError::Db)?
    .ok_or(AppError::NotFound)?;
    Ok(Json(serde_json::json!({
        "status": row.get::<String, _>("status"),
        "generated_at": row.get::<chrono::DateTime<chrono::Utc>, _>("generated_at").to_rfc3339(),
        "approved_at": row.try_get::<Option<chrono::DateTime<chrono::Utc>>, _>("approved_at").ok().flatten().map(|t| t.to_rfc3339()),
    })))
}

pub async fn download_packet(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(appeal_id): Path<Uuid>,
) -> Result<Response, AppError> {
    let organization_id = organization_id(&principal)?;
    let storage_key: String = sqlx::query_scalar(
        "SELECT storage_key FROM appeal_packets WHERE appeal_id = $1 AND organization_id = $2",
    )
    .bind(appeal_id)
    .bind(organization_id)
    .fetch_optional(&state.pool)
    .await
    .map_err(AppError::Db)?
    .ok_or(AppError::NotFound)?;
    let key = ObjectKey::parse(storage_key).map_err(|e| AppError::Internal(e.to_string()))?;
    let bytes = state
        .object_storage
        .get(&key)
        .map_err(|_| AppError::NotFound)?;
    Ok((
        [
            (header::CONTENT_TYPE, "application/pdf".to_string()),
            (
                header::CONTENT_DISPOSITION,
                format!("inline; filename=\"appeal-packet-{appeal_id}.pdf\""),
            ),
        ],
        bytes,
    )
        .into_response())
}

/// Marks the packet content reviewed. Not a submission record — see
/// `PATCH /appeals/{id}` for `submission_method` and
/// `payer_confirmation_number`, which track what actually happened with the
/// payer.
pub async fn approve_packet(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(appeal_id): Path<Uuid>,
) -> Result<Json<Value>, AppError> {
    let organization_id = organization_id(&principal)?;
    let row = sqlx::query(
        "UPDATE appeal_packets SET status = 'approved', approved_by = $1, approved_at = NOW() \
         WHERE appeal_id = $2 AND organization_id = $3 RETURNING id",
    )
    .bind(user_id(&principal))
    .bind(appeal_id)
    .bind(organization_id)
    .fetch_optional(&state.pool)
    .await
    .map_err(AppError::Db)?
    .ok_or(AppError::NotFound)?;
    Ok(Json(serde_json::json!({
        "id": row.get::<Uuid, _>("id").to_string(),
        "status": "approved",
    })))
}
