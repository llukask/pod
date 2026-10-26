//! Local rows in the conformance fixtures' JSON encoding
//! (`conformance/README.md`), for the runners that seed a store from a
//! fixture and compare it afterwards.
//!
//! The encoding differs from SQLite's in three ways: timestamps are RFC 3339
//! strings instead of epoch ms, flags are booleans instead of 0 / 1, and the
//! JSON columns hold the JSON value itself instead of its text.

use color_eyre::eyre::{Result, WrapErr, bail};
use pod_protocol::Timestamp;
use rusqlite::Connection;
use rusqlite::types::{Value as SqlValue, ValueRef};
use serde_json::{Map, Value};

use super::Table;

/// Integer columns that hold a flag.
const FLAG_COLUMNS: &[&str] = &[
    "subscribed",
    "played",
    "archived",
    "complete",
    "metered_ok",
    "auto_on_metered",
    "notify",
];

/// Text columns that hold JSON, as `(table, column)`.
const JSON_COLUMNS: &[(&str, &str)] = &[
    ("setting", "value"),
    ("device_setting", "value"),
    ("outbox", "payload"),
    ("rejected_mutation", "payload"),
];

/// Every `*_at` column holds epoch ms, and so does `unwanted_since`.
fn is_time(column: &str) -> bool {
    column.ends_with("_at") || column == "unwanted_since"
}

fn is_json(table: &str, column: &str) -> bool {
    JSON_COLUMNS.contains(&(table, column))
}

/// The SQLite value for a fixture's `table.column`.
pub(crate) fn to_sql(table: &str, column: &str, value: &Value) -> Result<SqlValue> {
    Ok(match value {
        _ if is_json(table, column) => SqlValue::Text(value.to_string()),
        Value::Null => SqlValue::Null,
        Value::String(text) if is_time(column) => SqlValue::Integer(
            text.parse::<Timestamp>()
                .with_context(|| format!("read {table}.{column} = {text:?} as RFC 3339"))?
                .as_epoch_ms(),
        ),
        Value::Bool(flag) if FLAG_COLUMNS.contains(&column) => SqlValue::Integer(i64::from(*flag)),
        Value::String(text) => SqlValue::Text(text.clone()),
        Value::Number(number) => match (number.as_i64(), number.as_f64()) {
            (Some(integer), _) => SqlValue::Integer(integer),
            (None, Some(real)) => SqlValue::Real(real),
            (None, None) => bail!("{table}.{column} = {number} is out of range"),
        },
        _ => bail!("{table}.{column} = {value} has no SQL form"),
    })
}

/// The fixture's JSON for a stored `table.column`.
pub(crate) fn to_json(table: &str, column: &str, value: ValueRef<'_>) -> Result<Value> {
    Ok(match value {
        ValueRef::Null => Value::Null,
        ValueRef::Text(text) if is_json(table, column) => serde_json::from_slice(text)
            .with_context(|| format!("parse {table}.{column} as JSON"))?,
        ValueRef::Integer(ms) if is_time(column) => Value::String(
            Timestamp::from_epoch_ms(ms)
                .with_context(|| format!("read {table}.{column} = {ms} as a timestamp"))?
                .to_string(),
        ),
        ValueRef::Integer(flag) if FLAG_COLUMNS.contains(&column) => Value::Bool(flag != 0),
        ValueRef::Integer(number) => Value::from(number),
        ValueRef::Real(number) => Value::from(number),
        ValueRef::Text(text) => Value::String(String::from_utf8_lossy(text).into_owned()),
        ValueRef::Blob(_) => bail!("{table}.{column} holds a blob"),
    })
}

/// Inserts one fixture row into `table`.
pub(crate) fn insert(connection: &Connection, table: &str, row: &Map<String, Value>) -> Result<()> {
    let columns: Vec<&str> = row.keys().map(String::as_str).collect();
    let placeholders: Vec<String> = (1..=columns.len()).map(|n| format!("?{n}")).collect();
    let values = row
        .iter()
        .map(|(column, value)| to_sql(table, column, value))
        .collect::<Result<Vec<_>>>()
        .context("encode the row")?;
    connection
        .execute(
            &format!(
                "insert into {table} ({}) values ({})",
                columns.join(", "),
                placeholders.join(", ")
            ),
            rusqlite::params_from_iter(values),
        )
        .with_context(|| format!("insert {row:?} into {table}"))?;
    Ok(())
}

/// Every row of every local table, as `{ "<table>": [ { "<column>": ... } ] }`.
pub(crate) fn dump(connection: &Connection) -> Result<Map<String, Value>> {
    let mut tables = Map::new();
    for table in Table::ALL {
        let name = table.name();
        let mut statement = connection
            .prepare(&format!("select * from {name}"))
            .with_context(|| format!("prepare the dump of {name}"))?;
        let columns: Vec<String> = statement
            .column_names()
            .into_iter()
            .map(str::to_owned)
            .collect();
        let mut rows = Vec::new();
        let mut cursor = statement
            .query([])
            .with_context(|| format!("query {name}"))?;
        while let Some(row) = cursor.next().with_context(|| format!("read {name}"))? {
            let mut object = Map::new();
            for (index, column) in columns.iter().enumerate() {
                let value = row
                    .get_ref(index)
                    .with_context(|| format!("read {name}.{column}"))?;
                let value = to_json(name, column, value).context("decode a column")?;
                object.insert(column.clone(), value);
            }
            rows.push(Value::Object(object));
        }
        tables.insert(name.to_owned(), Value::Array(rows));
    }
    Ok(tables)
}
