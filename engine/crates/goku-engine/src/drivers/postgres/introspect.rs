//! Catalog queries and the ER model (port of `PostgresIntrospection.swift`; checked by tests/parity_schema.rs). Client
//! text only ever reaches the server as bind parameters.
use super::Shared;
use crate::error::Result;
use crate::model::Cardinality;
use crate::model::{Attribute, Entity, IndexInfo, ObjectKind, ObjectRef, Relationship, SchemaModel};
use crate::profile::DatabaseKind;
use std::collections::HashMap;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

pub(super) async fn schemas(s: &Arc<Shared>, database: &str, cancel: &CancellationToken) -> Result<Vec<String>> {
    // `left(…) <> 'pg_'` instead of Swift's `NOT LIKE 'pg\_%'`: same set, without depending on standard_conforming_strings.
    let sql = "SELECT nspname FROM pg_namespace WHERE left(nspname, 3) <> 'pg_' AND nspname <> 'information_schema'
               ORDER BY nspname = 'public' DESC, 1";
    Ok(s.rows(database, sql, &[], cancel).await?.into_iter().filter_map(|mut r| r.remove(0)).collect())
}

fn kind(relkind: Option<&str>) -> ObjectKind {
    match relkind {
        Some("v") => ObjectKind::View,
        Some("m") => ObjectKind::MaterializedView,
        _ => ObjectKind::Table,
    }
}

fn json_strings(text: Option<&str>) -> Vec<String> {
    text.and_then(|t| serde_json::from_str::<Vec<serde_json::Value>>(t).ok())
        .map(|v| v.into_iter().filter_map(|x| x.as_str().map(str::to_owned)).collect())
        .unwrap_or_default()
}

fn fk_action(code: Option<&str>) -> Option<String> {
    Some(
        match code? {
            "c" => "CASCADE",
            "n" => "SET NULL",
            "d" => "SET DEFAULT",
            "r" => "RESTRICT",
            _ => return None, // 'a' = NO ACTION (the default)
        }
        .to_owned(),
    )
}

/// The tree's columns of one relation, with the key flags `introspect` would give (PK, single-column unique, FK).
pub(super) async fn columns(s: &Arc<Shared>, o: &ObjectRef, cancel: &CancellationToken) -> Result<Vec<Attribute>> {
    let sql =
        "SELECT a.attname, format_type(a.atttypid, a.atttypmod), a.attnotnull::text, pg_get_expr(d.adbin, d.adrelid),
                      col_description(c.oid, a.attnum),
                      EXISTS(SELECT 1 FROM pg_index i WHERE i.indrelid = c.oid AND i.indisprimary
                             AND a.attnum = ANY(i.indkey::int2[]))::text,
                      EXISTS(SELECT 1 FROM pg_index i WHERE i.indrelid = c.oid AND i.indisunique
                             AND i.indnatts = 1 AND i.indkey[0] = a.attnum)::text,
                      EXISTS(SELECT 1 FROM pg_constraint f WHERE f.conrelid = c.oid AND f.contype = 'f'
                             AND a.attnum = ANY(f.conkey))::text
               FROM pg_class c
               JOIN pg_namespace n ON n.oid = c.relnamespace
               JOIN pg_attribute a ON a.attrelid = c.oid AND a.attnum > 0 AND NOT a.attisdropped
               LEFT JOIN pg_attrdef d ON d.adrelid = c.oid AND d.adnum = a.attnum
               WHERE n.nspname = $1 AND c.relname = $2
               ORDER BY a.attnum";
    let schema = if o.schema.is_empty() { "public" } else { o.schema.as_str() };
    let rows = s.rows(&o.database, sql, &[&schema, &o.name], cancel).await?;
    Ok(rows
        .into_iter()
        .filter_map(|r| {
            let mut a =
                Attribute::new(r[0].as_deref()?, r[1].as_deref().unwrap_or("?"), r[2].as_deref() != Some("true"));
            a.default_value = r[3].clone();
            a.comment = r[4].clone();
            a.is_primary_key = r[5].as_deref() == Some("true");
            a.is_unique = r[6].as_deref() == Some("true");
            a.is_foreign_key = r[7].as_deref() == Some("true");
            Some(a)
        })
        .collect())
}

pub(super) async fn introspect(
    s: &Arc<Shared>,
    database: &str,
    schemas_in: &[String],
    cancel: &CancellationToken,
) -> Result<SchemaModel> {
    let wanted = if schemas_in.is_empty() { schemas(s, database, cancel).await? } else { schemas_in.to_vec() };
    let mut model = SchemaModel::new(database, DatabaseKind::Postgres);
    if wanted.is_empty() {
        return Ok(model);
    }
    let column_rows = s
        .rows(
            database,
            "SELECT n.nspname, c.relname, c.relkind::text, a.attname, format_type(a.atttypid, a.atttypmod),
                    a.attnotnull::text, pg_get_expr(d.adbin, d.adrelid), col_description(c.oid, a.attnum),
                    obj_description(c.oid, 'pg_class'), CASE WHEN c.reltuples >= 0 THEN c.reltuples::bigint::text END
             FROM pg_class c
             JOIN pg_namespace n ON n.oid = c.relnamespace
             JOIN pg_attribute a ON a.attrelid = c.oid AND a.attnum > 0 AND NOT a.attisdropped
             LEFT JOIN pg_attrdef d ON d.adrelid = c.oid AND d.adnum = a.attnum
             WHERE c.relkind IN ('r','p','v','m','f') AND n.nspname = ANY($1::text[])
               AND NOT c.relispartition
             ORDER BY n.nspname, c.relname, a.attnum",
            &[&wanted],
            cancel,
        )
        .await?;
    let mut order: Vec<String> = Vec::new();
    let mut entities: HashMap<String, Entity> = HashMap::new();
    for r in &column_rows {
        let (Some(schema), Some(table), Some(col)) = (&r[0], &r[1], &r[3]) else { continue };
        let id = format!("{schema}.{table}");
        let e = entities.entry(id.clone()).or_insert_with(|| {
            order.push(id.clone());
            let mut e = Entity::new(schema, table, kind(r[2].as_deref()), Vec::new());
            e.comment = r[8].clone();
            e.row_estimate = r[9].as_deref().and_then(|v| v.parse().ok());
            e
        });
        let mut a = Attribute::new(col, r[4].as_deref().unwrap_or("?"), r[5].as_deref() != Some("true"));
        a.default_value = r[6].clone();
        a.comment = r[7].clone();
        e.attributes.push(a);
    }

    // Indexes (includes PK / unique constraints, which are backed by indexes).
    let index_rows = s
        .rows(
            database,
            "SELECT n.nspname, t.relname, i.relname, ix.indisunique::text, ix.indisprimary::text,
                    (SELECT json_agg(a.attname ORDER BY k.ord) FROM unnest(ix.indkey::int2[]) WITH ORDINALITY k(attnum, ord)
                     JOIN pg_attribute a ON a.attrelid = t.oid AND a.attnum = k.attnum)::text
             FROM pg_index ix
             JOIN pg_class t ON t.oid = ix.indrelid
             JOIN pg_class i ON i.oid = ix.indexrelid
             JOIN pg_namespace n ON n.oid = t.relnamespace
             WHERE n.nspname = ANY($1::text[])
             ORDER BY 1, 2, 3",
            &[&wanted],
            cancel,
        )
        .await?;
    for r in &index_rows {
        let (Some(schema), Some(table), Some(name)) = (&r[0], &r[1], &r[2]) else { continue };
        let Some(e) = entities.get_mut(&format!("{schema}.{table}")) else { continue };
        let cols = json_strings(r[5].as_deref());
        let (unique, primary) = (r[3].as_deref() == Some("true"), r[4].as_deref() == Some("true"));
        for a in &mut e.attributes {
            if primary && cols.contains(&a.name) {
                a.is_primary_key = true;
            }
            if unique && cols.len() == 1 && cols[0] == a.name {
                a.is_unique = true;
            }
        }
        e.indexes.push(IndexInfo { name: name.clone(), columns: cols, unique, primary });
    }

    // Foreign keys. The referenced side may be outside the selected schemas; those are reported as warnings.
    let fk_rows = s
        .rows(
            database,
            "SELECT con.conname, n.nspname, c.relname,
                    (SELECT json_agg(a.attname ORDER BY k.ord) FROM unnest(con.conkey) WITH ORDINALITY k(attnum, ord)
                     JOIN pg_attribute a ON a.attrelid = con.conrelid AND a.attnum = k.attnum)::text,
                    fn.nspname, fc.relname,
                    (SELECT json_agg(a.attname ORDER BY k.ord) FROM unnest(con.confkey) WITH ORDINALITY k(attnum, ord)
                     JOIN pg_attribute a ON a.attrelid = con.confrelid AND a.attnum = k.attnum)::text,
                    con.confdeltype::text, con.confupdtype::text
             FROM pg_constraint con
             JOIN pg_class c ON c.oid = con.conrelid
             JOIN pg_namespace n ON n.oid = c.relnamespace
             JOIN pg_class fc ON fc.oid = con.confrelid
             JOIN pg_namespace fn ON fn.oid = fc.relnamespace
             WHERE con.contype = 'f' AND n.nspname = ANY($1::text[])
             ORDER BY 1",
            &[&wanted],
            cancel,
        )
        .await?;
    for r in &fk_rows {
        let (Some(name), Some(cs), Some(ct), Some(ps), Some(pt)) = (&r[0], &r[1], &r[2], &r[4], &r[5]) else {
            continue;
        };
        let (from, to) = (format!("{cs}.{ct}"), format!("{ps}.{pt}"));
        if !entities.contains_key(&to) {
            model.warnings.push(format!("{from}.{name} references {to}, which is outside the selected schemas."));
        }
        model.relationships.push(Relationship::new(
            name,
            &from,
            json_strings(r[3].as_deref()),
            &to,
            json_strings(r[6].as_deref()),
            Cardinality::ManyToOne,
            true,
            fk_action(r[7].as_deref()),
            fk_action(r[8].as_deref()),
            false,
        ));
    }
    model.entities = order.into_iter().filter_map(|id| entities.remove(&id)).collect();
    model.finalize_relationships();
    Ok(model)
}
