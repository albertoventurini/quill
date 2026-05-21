//! Postgres schema introspection.
//!
//! Reads `pg_database`, `pg_namespace`, `pg_class`, and `pg_proc` directly.
//! No `information_schema`; no `psql` shell-outs.
//!
//! All functions take a borrowed `&tokio_postgres::Client` so callers can
//! pass `&*guard` from a `SlotGuard<'_, PgConnector>`.  The introspection
//! module deliberately knows nothing about the slot manager or the local
//! SQLite cache — those compositional concerns belong in `commands` (M2.3).
//!
//! The output of `introspect_database` is the canonical contents of
//! `store::SchemaCacheRow.payload_json` and is versioned via the
//! `PAYLOAD_VERSION` constant.  Bumping the version is M4's problem (it
//! will add column metadata); for M2 v1 is fixed at 1.

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio_postgres::Client;

/// Canonical version of the schema-cache payload.  Bumped only when the
/// `SchemaPayload` wire shape changes in a way old payloads cannot satisfy.
pub const PAYLOAD_VERSION: u32 = 1;

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

#[derive(Debug, Error)]
pub enum IntrospectError {
    #[error("Postgres error: {0}")]
    Pg(#[from] tokio_postgres::Error),

    #[error("unknown relkind '{0}' returned by pg_class")]
    UnknownRelKind(String),

    #[error("unknown prokind '{0}' returned by pg_proc")]
    UnknownProKind(String),
}

// ---------------------------------------------------------------------------
// Value types — the canonical wire shape of payload_json
// ---------------------------------------------------------------------------

/// A single database visible on the connected server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DatabaseInfo {
    pub name: String,
}

/// Top-level payload — one of these per `(server_id, database)` cache row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SchemaPayload {
    /// Wire-shape version.  Matches `PAYLOAD_VERSION` at write time.
    pub v: u32,
    pub schemas: Vec<SchemaInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SchemaInfo {
    pub name: String,
    pub relations: Vec<RelationInfo>,
    pub functions: Vec<FunctionInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RelationInfo {
    pub name: String,
    pub kind: RelationKind,
}

/// Mirrors `pg_class.relkind` for the four kinds Quill exposes in v1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RelationKind {
    Table,
    View,
    Matview,
    PartitionedTable,
}

impl RelationKind {
    fn from_pg(c: char) -> Result<Self, IntrospectError> {
        match c {
            'r' => Ok(Self::Table),
            'v' => Ok(Self::View),
            'm' => Ok(Self::Matview),
            'p' => Ok(Self::PartitionedTable),
            other => Err(IntrospectError::UnknownRelKind(other.to_string())),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FunctionInfo {
    pub name: String,
    pub kind: FunctionKind,
}

/// Mirrors `pg_proc.prokind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FunctionKind {
    Function,
    Procedure,
    Aggregate,
    Window,
}

impl FunctionKind {
    fn from_pg(c: char) -> Result<Self, IntrospectError> {
        match c {
            'f' => Ok(Self::Function),
            'p' => Ok(Self::Procedure),
            'a' => Ok(Self::Aggregate),
            'w' => Ok(Self::Window),
            other => Err(IntrospectError::UnknownProKind(other.to_string())),
        }
    }
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// List every connectable, non-template database on the server.
///
/// Ordered by `datname`.  Excludes `template0` (datallowconn=false) and
/// every datistemplate=true row including `template1`.
///
/// # Safety
///
/// The `pg_database` catalog does not return NULL datnames, so the bare
/// `.get` form is safe here.
pub async fn list_databases(client: &Client) -> Result<Vec<DatabaseInfo>, IntrospectError> {
    let rows = client
        .query(
            "SELECT datname \
             FROM pg_database \
             WHERE datallowconn AND NOT datistemplate \
             ORDER BY datname",
            &[],
        )
        .await?;

    Ok(rows
        .into_iter()
        .map(|r| DatabaseInfo {
            name: r.get::<_, &str>("datname").to_string(),
        })
        .collect())
}

/// Fetch the full schema payload for the currently-connected database.
///
/// Issues three independent SQL queries serially over the same connection
/// (schemas, relations, functions) and stitches them into a single
/// `SchemaPayload`.  Schemas with no relations and no functions still
/// appear (the user expects to see an empty schema as an empty folder).
pub async fn introspect_database(client: &Client) -> Result<SchemaPayload, IntrospectError> {
    let schemas = list_schema_names(client).await?;
    let relations = list_all_relations(client).await?;
    let functions = list_all_functions(client).await?;

    let mut by_schema: std::collections::BTreeMap<String, SchemaInfo> = schemas
        .into_iter()
        .map(|name| {
            (
                name.clone(),
                SchemaInfo {
                    name,
                    relations: Vec::new(),
                    functions: Vec::new(),
                },
            )
        })
        .collect();

    for (schema, rel) in relations {
        by_schema
            .entry(schema.clone())
            .or_insert_with(|| SchemaInfo {
                name: schema,
                relations: Vec::new(),
                functions: Vec::new(),
            })
            .relations
            .push(rel);
    }
    for (schema, func) in functions {
        by_schema
            .entry(schema.clone())
            .or_insert_with(|| SchemaInfo {
                name: schema,
                relations: Vec::new(),
                functions: Vec::new(),
            })
            .functions
            .push(func);
    }

    Ok(SchemaPayload {
        v: PAYLOAD_VERSION,
        schemas: by_schema.into_values().collect(),
    })
}

// ---------------------------------------------------------------------------
// Internals — one query per system catalog
// ---------------------------------------------------------------------------

async fn list_schema_names(client: &Client) -> Result<Vec<String>, IntrospectError> {
    let rows = client
        .query(
            r"SELECT nspname
          FROM pg_namespace
          WHERE nspname NOT LIKE 'pg\_%' ESCAPE '\'
            AND nspname <> 'information_schema'
          ORDER BY nspname",
            &[],
        )
        .await?;

    Ok(rows
        .into_iter()
        .map(|r| r.get::<_, &str>("nspname").to_string())
        .collect())
}

async fn list_all_relations(
    client: &Client,
) -> Result<Vec<(String, RelationInfo)>, IntrospectError> {
    let rows = client
        .query(
            r"SELECT n.nspname AS schema,
                 c.relname AS name,
                 c.relkind::text AS kind
          FROM pg_class c
          JOIN pg_namespace n ON c.relnamespace = n.oid
          WHERE c.relkind IN ('r', 'v', 'm', 'p')
            AND n.nspname NOT LIKE 'pg\_%' ESCAPE '\'
            AND n.nspname <> 'information_schema'
          ORDER BY n.nspname, c.relname",
            &[],
        )
        .await?;

    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let schema: String = row.try_get::<_, &str>("schema")?.to_string();
        let name: String = row.try_get::<_, &str>("name")?.to_string();
        let kind_str: String = row.try_get::<_, &str>("kind")?.to_string();
        let kind_char = kind_str
            .chars()
            .next()
            .ok_or_else(|| IntrospectError::UnknownRelKind(String::from("")))?;
        let kind = RelationKind::from_pg(kind_char)?;
        out.push((schema, RelationInfo { name, kind }));
    }
    Ok(out)
}

async fn list_all_functions(
    client: &Client,
) -> Result<Vec<(String, FunctionInfo)>, IntrospectError> {
    let rows = client
        .query(
            r"SELECT n.nspname AS schema,
                 p.proname AS name,
                 p.prokind::text AS kind
          FROM pg_proc p
          JOIN pg_namespace n ON p.pronamespace = n.oid
          WHERE n.nspname NOT LIKE 'pg\_%' ESCAPE '\'
            AND n.nspname <> 'information_schema'
          ORDER BY n.nspname, p.proname",
            &[],
        )
        .await?;

    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let schema: String = row.try_get::<_, &str>("schema")?.to_string();
        let name: String = row.try_get::<_, &str>("name")?.to_string();
        let kind_str: String = row.try_get::<_, &str>("kind")?.to_string();
        let kind_char = kind_str
            .chars()
            .next()
            .ok_or_else(|| IntrospectError::UnknownProKind(String::from("")))?;
        let kind = FunctionKind::from_pg(kind_char)?;
        out.push((schema, FunctionInfo { name, kind }));
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Tests (payload (de)serialization only — DB tests live in tests/)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_round_trips_through_json() {
        let payload = SchemaPayload {
            v: PAYLOAD_VERSION,
            schemas: vec![SchemaInfo {
                name: "public".into(),
                relations: vec![
                    RelationInfo {
                        name: "users".into(),
                        kind: RelationKind::Table,
                    },
                    RelationInfo {
                        name: "user_emails".into(),
                        kind: RelationKind::View,
                    },
                ],
                functions: vec![FunctionInfo {
                    name: "uuid_generate_v4".into(),
                    kind: FunctionKind::Function,
                }],
            }],
        };

        let s = serde_json::to_string(&payload).expect("serialize");
        let back: SchemaPayload = serde_json::from_str(&s).expect("deserialize");
        assert_eq!(payload, back);
    }

    #[test]
    fn relation_kind_serializes_as_snake_case() {
        assert_eq!(
            serde_json::to_value(RelationKind::PartitionedTable).unwrap(),
            serde_json::json!("partitioned_table")
        );
        assert_eq!(
            serde_json::to_value(RelationKind::Matview).unwrap(),
            serde_json::json!("matview")
        );
    }

    #[test]
    fn function_kind_serializes_as_snake_case() {
        assert_eq!(
            serde_json::to_value(FunctionKind::Window).unwrap(),
            serde_json::json!("window")
        );
    }

    #[test]
    fn relkind_from_pg_rejects_unknowns() {
        assert!(RelationKind::from_pg('S').is_err()); // sequence
        assert!(RelationKind::from_pg('i').is_err()); // index
        assert!(RelationKind::from_pg('t').is_err()); // toast
    }
}
