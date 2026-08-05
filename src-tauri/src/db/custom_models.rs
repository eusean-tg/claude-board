use crate::db::DbPool;
use rusqlite::params;
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct CustomModel {
    pub id: i64,
    pub model_id: String,
    pub label: String,
    pub color: Option<String>,
    pub input_cost_per_mtok: Option<f64>,
    pub output_cost_per_mtok: Option<f64>,
    pub sort_order: i64,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
}

pub fn list(db: &DbPool) -> Vec<CustomModel> {
    let conn = db.lock();
    let mut stmt = match conn.prepare(
        "SELECT id, model_id, label, color, input_cost_per_mtok, output_cost_per_mtok, sort_order, created_at, updated_at
         FROM custom_models ORDER BY sort_order ASC, id ASC",
    ) {
        Ok(s) => s,
        Err(e) => { log::error!("custom_models list prepare: {}", e); return vec![]; }
    };
    let rows = stmt.query_map([], |row| {
        Ok(CustomModel {
            id: row.get(0)?,
            model_id: row.get(1)?,
            label: row.get(2)?,
            color: row.get(3).ok(),
            input_cost_per_mtok: row.get(4).ok(),
            output_cost_per_mtok: row.get(5).ok(),
            sort_order: row.get::<_, i64>(6).unwrap_or(0),
            created_at: row.get(7).ok(),
            updated_at: row.get(8).ok(),
        })
    });
    match rows {
        Ok(it) => it.flatten().collect(),
        Err(e) => {
            log::error!("custom_models query: {}", e);
            vec![]
        }
    }
}

pub fn create(
    db: &DbPool,
    model_id: &str,
    label: &str,
    color: Option<&str>,
    input_cost: Option<f64>,
    output_cost: Option<f64>,
    sort_order: i64,
) -> Result<i64, String> {
    let conn = db.lock();
    conn.execute(
        "INSERT INTO custom_models (model_id, label, color, input_cost_per_mtok, output_cost_per_mtok, sort_order)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![model_id, label, color, input_cost, output_cost, sort_order],
    ).map_err(|e| e.to_string())?;
    Ok(conn.last_insert_rowid())
}

#[allow(clippy::too_many_arguments)]
pub fn update(
    db: &DbPool,
    id: i64,
    model_id: &str,
    label: &str,
    color: Option<&str>,
    input_cost: Option<f64>,
    output_cost: Option<f64>,
    sort_order: i64,
) -> Result<(), String> {
    let conn = db.lock();
    conn.execute(
        "UPDATE custom_models SET model_id=?1, label=?2, color=?3, input_cost_per_mtok=?4, output_cost_per_mtok=?5, sort_order=?6, updated_at=datetime('now','localtime') WHERE id=?7",
        params![model_id, label, color, input_cost, output_cost, sort_order, id],
    ).map_err(|e| e.to_string())?;
    Ok(())
}

pub fn delete(db: &DbPool, id: i64) -> Result<(), String> {
    let conn = db.lock();
    conn.execute("DELETE FROM custom_models WHERE id=?1", params![id])
        .map_err(|e| e.to_string())?;
    Ok(())
}
