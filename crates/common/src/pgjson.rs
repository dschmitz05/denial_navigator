//! Turning an arbitrary `PgRow` into JSON.
//!
//! The route handlers return `SELECT *` results straight to the existing
//! frontend, so every column has to survive the trip whatever its Postgres
//! type. `try_get` is type-checked, so this walks the plausible Rust types in
//! order and takes the first that decodes.
//!
//! `NUMERIC` is the awkward one: sqlx will not decode it into `f64`, only into
//! `BigDecimal`, so money columns (`DECIMAL(12,2)` and friends) came back as
//! `null` until this handled them explicitly.

use sqlx::{Column, Row};
use sqlx::types::BigDecimal;

/// Render one column value as JSON, or `Null` if nothing decodes.
pub fn value_at(row: &sqlx::postgres::PgRow, name: &str) -> serde_json::Value {
    use serde_json::Value;

    if let Ok(v) = row.try_get::<Option<String>, _>(name) {
        return v.map(Value::String).unwrap_or(Value::Null);
    }
    if let Ok(v) = row.try_get::<Option<bool>, _>(name) {
        return v.map(Value::from).unwrap_or(Value::Null);
    }
    if let Ok(v) = row.try_get::<Option<i16>, _>(name) {
        return v.map(Value::from).unwrap_or(Value::Null);
    }
    if let Ok(v) = row.try_get::<Option<i32>, _>(name) {
        return v.map(Value::from).unwrap_or(Value::Null);
    }
    if let Ok(v) = row.try_get::<Option<i64>, _>(name) {
        return v.map(Value::from).unwrap_or(Value::Null);
    }
    if let Ok(v) = row.try_get::<Option<f64>, _>(name) {
        return v.map(Value::from).unwrap_or(Value::Null);
    }
    if let Ok(v) = row.try_get::<Option<BigDecimal>, _>(name) {
        // Serialise as a JSON number so the frontend keeps doing arithmetic on
        // it; fall back to a string if it somehow will not fit an f64.
        return match v {
            Some(d) => d
                .to_string()
                .parse::<f64>()
                .ok()
                .and_then(serde_json::Number::from_f64)
                .map(Value::Number)
                .unwrap_or_else(|| Value::String(d.to_string())),
            None => Value::Null,
        };
    }
    if let Ok(v) = row.try_get::<Option<uuid::Uuid>, _>(name) {
        return v.map(|u| Value::String(u.to_string())).unwrap_or(Value::Null);
    }
    if let Ok(v) = row.try_get::<Option<chrono::DateTime<chrono::Utc>>, _>(name) {
        return v.map(|t| Value::String(t.to_rfc3339())).unwrap_or(Value::Null);
    }
    if let Ok(v) = row.try_get::<Option<chrono::NaiveDate>, _>(name) {
        return v.map(|d| Value::String(d.to_string())).unwrap_or(Value::Null);
    }
    if let Ok(v) = row.try_get::<Option<chrono::NaiveDateTime>, _>(name) {
        return v.map(|t| Value::String(t.to_string())).unwrap_or(Value::Null);
    }
    if let Ok(v) = row.try_get::<Option<Vec<String>>, _>(name) {
        return v
            .map(|xs| Value::Array(xs.into_iter().map(Value::String).collect()))
            .unwrap_or(Value::Null);
    }
    if let Ok(v) = row.try_get::<Option<serde_json::Value>, _>(name) {
        return v.unwrap_or(Value::Null);
    }
    Value::Null
}

/// Render a whole row as a JSON object keyed by column name.
pub fn row_to_json(row: &sqlx::postgres::PgRow) -> serde_json::Value {
    let mut map = serde_json::Map::with_capacity(row.columns().len());
    for col in row.columns() {
        map.insert(col.name().to_string(), value_at(row, col.name()));
    }
    serde_json::Value::Object(map)
}
