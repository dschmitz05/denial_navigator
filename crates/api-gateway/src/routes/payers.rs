//! Payers and the spellings and IDs that resolve to them (FB-11).
//!
//! Claims and knowledge documents name payers however their source spelled
//! them, and retrieval used to match names exactly, so a claim from
//! "BlueCross BlueShield" never found policies filed under "BLUECROSS
//! BLUESHIELD OF ILLINOIS". Mapping both as aliases of one payer fixes that;
//! the search filter expands aliases (see rag-engine `push_scope`).

use axum::extract::{Path, State};
use axum::routing::{delete, get, post};
use axum::{Extension, Json, Router};
use denial_auth::rbac::Principal;
use denial_common::error::AppError;
use serde::Deserialize;
use serde_json::Value;
use sqlx::Row;
use uuid::Uuid;

use crate::routes::appeals::record_audit;
use crate::state::AppState;

fn organization_id(principal: &Principal) -> Result<Uuid, AppError> {
    principal
        .organization_id
        .as_deref()
        .and_then(|id| Uuid::parse_str(id).ok())
        .ok_or(AppError::Forbidden)
}

fn is_unique_violation(e: &sqlx::Error) -> bool {
    matches!(e, sqlx::Error::Database(db) if db.code().is_some_and(|c| c == "23505"))
}

/// Payers with their aliases, and the payer names in claims and documents
/// that no alias covers yet.
pub async fn list(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
) -> Result<Json<Value>, AppError> {
    let organization_id = organization_id(&principal)?;
    let rows = sqlx::query(
        "SELECT p.id, p.name, \
                COALESCE(json_agg(json_build_object('id', a.id, 'alias', a.alias, 'kind', a.kind) \
                                  ORDER BY a.kind, a.alias) FILTER (WHERE a.id IS NOT NULL), '[]') AS aliases \
         FROM payers p LEFT JOIN payer_aliases a ON a.payer_id = p.id \
         WHERE p.organization_id = $1 GROUP BY p.id, p.name ORDER BY p.name",
    )
    .bind(organization_id)
    .fetch_all(&state.pool)
    .await
    .map_err(AppError::Db)?;
    let payers: Vec<Value> = rows
        .iter()
        .map(|r| {
            serde_json::json!({
                "id": r.get::<Uuid, _>("id").to_string(),
                "name": r.get::<String, _>("name"),
                "aliases": r.get::<Value, _>("aliases"),
            })
        })
        .collect();
    let unmapped = sqlx::query(
        "SELECT name, source, COUNT(*) AS count FROM ( \
             SELECT payer_name AS name, 'claims' AS source FROM claims \
             WHERE organization_id = $1 AND payer_name IS NOT NULL \
             UNION ALL \
             SELECT payer_name, 'documents' FROM knowledge_documents \
             WHERE organization_id = $1 AND payer_name IS NOT NULL AND status <> 'archived' \
         ) n \
         WHERE NOT EXISTS (SELECT 1 FROM payer_aliases a \
                           WHERE a.organization_id = $1 \
                             AND a.alias_normalized = normalize_payer_name(n.name)) \
         GROUP BY name, source ORDER BY count DESC, name",
    )
    .bind(organization_id)
    .fetch_all(&state.pool)
    .await
    .map_err(AppError::Db)?;
    Ok(Json(serde_json::json!({
        "payers": payers,
        "unmapped": unmapped.iter().map(|r| serde_json::json!({
            "name": r.get::<String, _>("name"),
            "source": r.get::<String, _>("source"),
            "count": r.get::<i64, _>("count"),
        })).collect::<Vec<_>>(),
    })))
}

#[derive(Deserialize)]
pub struct PayerInput {
    pub name: String,
}

pub async fn create(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Json(input): Json<PayerInput>,
) -> Result<Json<Value>, AppError> {
    let name = input.name.trim();
    if name.is_empty() || name.len() > 255 {
        return Err(AppError::BadRequest("name is required".into()));
    }
    let organization_id = organization_id(&principal)?;
    let mut tx = state.pool.begin().await.map_err(AppError::Db)?;
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO payers (organization_id, name) VALUES ($1, $2) RETURNING id",
    )
    .bind(organization_id)
    .bind(name)
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| {
        if is_unique_violation(&e) {
            AppError::Conflict(format!("A payer named '{name}' already exists"))
        } else {
            AppError::Db(e)
        }
    })?;
    // The payer's own name is always one of its aliases.
    insert_alias(&mut tx, organization_id, id, name, "name").await?;
    tx.commit().await.map_err(AppError::Db)?;
    record_audit(
        &state.pool,
        &principal,
        "payer_created",
        "payer",
        Some(&id.to_string()),
        &serde_json::json!({ "username": principal.username, "name": name }),
    )
    .await;
    Ok(Json(
        serde_json::json!({ "id": id.to_string(), "name": name }),
    ))
}

async fn insert_alias(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    organization_id: Uuid,
    payer_id: Uuid,
    alias: &str,
    kind: &str,
) -> Result<Uuid, AppError> {
    sqlx::query_scalar(
        "INSERT INTO payer_aliases (organization_id, payer_id, alias, alias_normalized, kind) \
         VALUES ($1, $2, $3, normalize_payer_name($3), $4) RETURNING id",
    )
    .bind(organization_id)
    .bind(payer_id)
    .bind(alias)
    .bind(kind)
    .fetch_one(&mut **tx)
    .await
    .map_err(|e| {
        if is_unique_violation(&e) {
            AppError::Conflict(format!("'{alias}' is already an alias of a payer"))
        } else {
            AppError::Db(e)
        }
    })
}

#[derive(Deserialize)]
pub struct AliasInput {
    pub alias: String,
    #[serde(default = "default_kind")]
    pub kind: String,
}

fn default_kind() -> String {
    "name".into()
}

pub async fn add_alias(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(payer_id): Path<Uuid>,
    Json(input): Json<AliasInput>,
) -> Result<Json<Value>, AppError> {
    let alias = input.alias.trim();
    if alias.is_empty() || alias.len() > 255 {
        return Err(AppError::BadRequest("alias is required".into()));
    }
    if !["name", "payer_id"].contains(&input.kind.as_str()) {
        return Err(AppError::Unprocessable(
            "kind must be name or payer_id".into(),
        ));
    }
    let organization_id = organization_id(&principal)?;
    let mut tx = state.pool.begin().await.map_err(AppError::Db)?;
    let owned: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM payers WHERE id = $1 AND organization_id = $2)",
    )
    .bind(payer_id)
    .bind(organization_id)
    .fetch_one(&mut *tx)
    .await
    .map_err(AppError::Db)?;
    if !owned {
        return Err(AppError::NotFound);
    }
    let id = insert_alias(&mut tx, organization_id, payer_id, alias, &input.kind).await?;
    tx.commit().await.map_err(AppError::Db)?;
    record_audit(
        &state.pool,
        &principal,
        "payer_alias_added",
        "payer",
        Some(&payer_id.to_string()),
        &serde_json::json!({ "username": principal.username, "alias": alias, "kind": input.kind }),
    )
    .await;
    Ok(Json(
        serde_json::json!({ "id": id.to_string(), "alias": alias }),
    ))
}

pub async fn remove_alias(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path((payer_id, alias_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<Value>, AppError> {
    let deleted = sqlx::query(
        "DELETE FROM payer_aliases WHERE id = $1 AND payer_id = $2 AND organization_id = $3 RETURNING id",
    )
    .bind(alias_id)
    .bind(payer_id)
    .bind(organization_id(&principal)?)
    .fetch_optional(&state.pool)
    .await
    .map_err(AppError::Db)?;
    if deleted.is_none() {
        return Err(AppError::NotFound);
    }
    record_audit(
        &state.pool,
        &principal,
        "payer_alias_removed",
        "payer",
        Some(&payer_id.to_string()),
        &serde_json::json!({ "username": principal.username }),
    )
    .await;
    Ok(Json(serde_json::json!({ "deleted": alias_id.to_string() })))
}

pub async fn remove(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(payer_id): Path<Uuid>,
) -> Result<Json<Value>, AppError> {
    let deleted =
        sqlx::query("DELETE FROM payers WHERE id = $1 AND organization_id = $2 RETURNING id")
            .bind(payer_id)
            .bind(organization_id(&principal)?)
            .fetch_optional(&state.pool)
            .await
            .map_err(AppError::Db)?;
    if deleted.is_none() {
        return Err(AppError::NotFound);
    }
    record_audit(
        &state.pool,
        &principal,
        "payer_deleted",
        "payer",
        Some(&payer_id.to_string()),
        &serde_json::json!({ "username": principal.username }),
    )
    .await;
    Ok(Json(serde_json::json!({ "deleted": payer_id.to_string() })))
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", get(list).post(create))
        .route("/{payer_id}", delete(remove))
        .route("/{payer_id}/aliases", post(add_alias))
        .route("/{payer_id}/aliases/{alias_id}", delete(remove_alias))
}
