//! Sidebar catalog and ER introspection from `information_schema` (port of `MySQLIntrospection.swift`). One database;
//! `schemas` is ignored and entity ids are bare table names. Names go in as parameters, never spliced into SQL.
use super::MySqlSession;
use crate::error::Result;
use crate::model::{Attribute, Cardinality, Entity, IndexInfo, ObjectKind, ObjectRef, Relationship, SchemaModel};
use crate::profile::DatabaseKind;
use std::collections::HashMap;
use tokio_util::sync::CancellationToken;

type Row = Vec<Option<String>>;

fn text(r: &Row, i: usize) -> Option<&str> {
    r.get(i).and_then(|v| v.as_deref())
}

/// Tables and views by name (Swift also lists routines; the engine's tree has no use for them yet).
pub(super) async fn objects(s: &MySqlSession, database: &str, cancel: &CancellationToken) -> Result<Vec<ObjectRef>> {
    let sql = "SELECT TABLE_NAME, TABLE_TYPE FROM information_schema.TABLES WHERE TABLE_SCHEMA = ? ORDER BY TABLE_NAME";
    Ok(s.rows(sql, vec![database], cancel)
        .await?
        .iter()
        .filter_map(|r| {
            let view = matches!(text(r, 1), Some("VIEW" | "SYSTEM VIEW"));
            Some(ObjectRef {
                database: database.to_owned(),
                schema: String::new(),
                name: text(r, 0)?.to_owned(),
                kind: if view { ObjectKind::View } else { ObjectKind::Table },
            })
        })
        .collect())
}

const COLUMNS: &str = "COLUMN_NAME, COLUMN_TYPE, IS_NULLABLE, COLUMN_DEFAULT, COLUMN_KEY, COLUMN_COMMENT, EXTRA";

/// `COLUMNS` row from `offset` on, as Introspection builds it: `EXTRA` is appended to the type
/// (`int auto_increment`, `datetime DEFAULT_GENERATED`).
fn attribute(r: &Row, offset: usize) -> Option<Attribute> {
    let mut a =
        Attribute::new(text(r, offset)?, text(r, offset + 1).unwrap_or("?"), text(r, offset + 2) == Some("YES"));
    a.default_value = text(r, offset + 3).map(str::to_owned);
    a.is_primary_key = text(r, offset + 4) == Some("PRI");
    a.comment = text(r, offset + 5).filter(|c| !c.is_empty()).map(str::to_owned);
    if let Some(extra) = text(r, offset + 6).filter(|e| !e.is_empty()) {
        a.type_name += &format!(" {extra}");
    }
    Some(a)
}

/// Unique-index columns of a table, by index, in key order.
fn unique_indexes(rows: &[Row]) -> Vec<(String, Vec<String>)> {
    let mut out: Vec<(String, Vec<String>)> = Vec::new();
    for r in rows {
        let (Some(name), Some(col)) = (text(r, 0), text(r, 1)) else { continue };
        match out.last_mut() {
            Some((n, cols)) if n == name => cols.push(col.to_owned()),
            _ => out.push((name.to_owned(), vec![col.to_owned()])),
        }
    }
    out
}

pub(super) async fn columns(
    s: &MySqlSession,
    object: &ObjectRef,
    cancel: &CancellationToken,
) -> Result<Vec<Attribute>> {
    let (db, table) = (object.database.as_str(), object.name.as_str());
    let sql = format!(
        "SELECT {COLUMNS} FROM information_schema.COLUMNS WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ? ORDER BY ORDINAL_POSITION"
    );
    let mut attrs: Vec<Attribute> =
        s.rows(&sql, vec![db, table], cancel).await?.iter().filter_map(|r| attribute(r, 0)).collect();
    let idx = s
        .rows(
            "SELECT INDEX_NAME, COLUMN_NAME FROM information_schema.STATISTICS \
             WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ? AND NON_UNIQUE = 0 ORDER BY INDEX_NAME, SEQ_IN_INDEX",
            vec![db, table],
            cancel,
        )
        .await?;
    for (_, cols) in unique_indexes(&idx).into_iter().filter(|(_, c)| c.len() == 1) {
        if let Some(a) = attrs.iter_mut().find(|a| a.name == cols[0]) {
            a.is_unique = true;
        }
    }
    Ok(attrs)
}

pub(super) async fn introspect(s: &MySqlSession, database: &str, cancel: &CancellationToken) -> Result<SchemaModel> {
    let db = vec![database];
    let tables = s
        .rows(
            "SELECT TABLE_NAME, TABLE_TYPE, TABLE_ROWS, TABLE_COMMENT FROM information_schema.TABLES \
             WHERE TABLE_SCHEMA = ? ORDER BY TABLE_NAME",
            db.clone(),
            cancel,
        )
        .await?;
    let mut entities: Vec<Entity> = Vec::new();
    let mut by_name: HashMap<String, usize> = HashMap::new();
    for t in &tables {
        let Some(name) = text(t, 0) else { continue };
        let kind = if text(t, 1) == Some("VIEW") { ObjectKind::View } else { ObjectKind::Table };
        let mut e = Entity::new("", name, kind, Vec::new());
        e.row_estimate = text(t, 2).and_then(|v| v.parse().ok());
        // Views carry TABLE_COMMENT "VIEW"; GokuCore keeps it as the comment.
        e.comment = text(t, 3).filter(|c| !c.is_empty()).map(str::to_owned);
        by_name.insert(name.to_owned(), entities.len());
        entities.push(e);
    }
    let sql = format!(
        "SELECT TABLE_NAME, {COLUMNS} FROM information_schema.COLUMNS WHERE TABLE_SCHEMA = ? \
         ORDER BY TABLE_NAME, ORDINAL_POSITION"
    );
    for c in s.rows(&sql, db.clone(), cancel).await? {
        let (Some(&i), Some(a)) = (text(&c, 0).and_then(|t| by_name.get(t)), attribute(&c, 1)) else { continue };
        entities[i].attributes.push(a);
    }
    let idx = s
        .rows(
            "SELECT TABLE_NAME, INDEX_NAME, NON_UNIQUE, COLUMN_NAME FROM information_schema.STATISTICS \
             WHERE TABLE_SCHEMA = ? ORDER BY TABLE_NAME, INDEX_NAME, SEQ_IN_INDEX",
            db.clone(),
            cancel,
        )
        .await?;
    // (table, index) in first-seen order, like Swift's `indexOrder`.
    let mut indexes: Vec<(String, IndexInfo)> = Vec::new();
    for r in &idx {
        let (Some(t), Some(n), Some(col)) = (text(r, 0), text(r, 1), text(r, 3)) else { continue };
        match indexes.iter_mut().find(|(tt, i)| tt == t && i.name == n) {
            Some((_, i)) => i.columns.push(col.to_owned()),
            None => indexes.push((
                t.to_owned(),
                IndexInfo {
                    name: n.to_owned(),
                    columns: vec![col.to_owned()],
                    unique: text(r, 2) == Some("0"),
                    primary: n == "PRIMARY",
                },
            )),
        }
    }
    for (t, index) in indexes {
        let Some(&i) = by_name.get(&t) else { continue };
        let e = &mut entities[i];
        if index.unique
            && index.columns.len() == 1
            && let Some(a) = e.attributes.iter_mut().find(|a| a.name == index.columns[0])
        {
            a.is_unique = true;
        }
        e.indexes.push(index);
    }
    let fks = s
        .rows(
            "SELECT k.CONSTRAINT_NAME, k.TABLE_NAME, k.COLUMN_NAME, k.REFERENCED_TABLE_SCHEMA, k.REFERENCED_TABLE_NAME, \
                    k.REFERENCED_COLUMN_NAME, r.UPDATE_RULE, r.DELETE_RULE \
             FROM information_schema.KEY_COLUMN_USAGE k \
             JOIN information_schema.REFERENTIAL_CONSTRAINTS r \
               ON r.CONSTRAINT_SCHEMA = k.CONSTRAINT_SCHEMA AND r.CONSTRAINT_NAME = k.CONSTRAINT_NAME \
              AND r.TABLE_NAME = k.TABLE_NAME \
             WHERE k.TABLE_SCHEMA = ? AND k.REFERENCED_TABLE_NAME IS NOT NULL \
             ORDER BY k.TABLE_NAME, k.CONSTRAINT_NAME, k.ORDINAL_POSITION",
            db,
            cancel,
        )
        .await?;
    struct Fk {
        name: String,
        from: String,
        to: String,
        from_columns: Vec<String>,
        to_columns: Vec<String>,
        on_update: Option<String>,
        on_delete: Option<String>,
    }
    let mut rels: Vec<Fk> = Vec::new();
    let mut warnings = Vec::new();
    for r in &fks {
        let (Some(name), Some(t), Some(col), Some(rs), Some(rt), Some(rc)) =
            (text(r, 0), text(r, 1), text(r, 2), text(r, 3), text(r, 4), text(r, 5))
        else {
            continue;
        };
        let fk = match rels.iter_mut().position(|f| f.from == t && f.name == name) {
            Some(i) => &mut rels[i],
            None => {
                if rs != database {
                    warnings.push(format!("{t}.{name} references {rs}.{rt} in another database."));
                }
                rels.push(Fk {
                    name: name.to_owned(),
                    from: t.to_owned(),
                    to: if rs == database { rt.to_owned() } else { format!("{rs}.{rt}") },
                    from_columns: Vec::new(),
                    to_columns: Vec::new(),
                    on_update: rule(text(r, 6)),
                    on_delete: rule(text(r, 7)),
                });
                rels.last_mut().expect("pushed")
            }
        };
        fk.from_columns.push(col.to_owned());
        fk.to_columns.push(rc.to_owned());
    }
    let mut model = SchemaModel::new(database, DatabaseKind::Mysql);
    model.entities = entities;
    model.relationships = rels
        .into_iter()
        .map(|f| {
            let (n, from, to) = (f.name, f.from, f.to);
            Relationship::new(
                &n,
                &from,
                f.from_columns,
                &to,
                f.to_columns,
                Cardinality::ManyToOne,
                true,
                f.on_delete,
                f.on_update,
                false,
            )
        })
        .collect();
    model.warnings = warnings;
    model.finalize_relationships();
    Ok(model)
}

fn rule(r: Option<&str>) -> Option<String> {
    r.filter(|r| *r != "NO ACTION").map(str::to_owned)
}
