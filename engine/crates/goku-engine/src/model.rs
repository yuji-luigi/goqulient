//! Engine-neutral types shared by every driver. JSON field names match GokuCore's `Codable` types (`SchemaModel.swift`,
//! `Catalog.swift`, `QueryResult.swift`) so the parity fixtures and, later, the app read the same shapes.
use crate::profile::DatabaseKind;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ObjectKind {
    Table,
    View,
    MaterializedView,
    Collection,
    Function,
    Sequence,
}

/// Addresses a table/collection. `schema` is the PostgreSQL schema; empty for MySQL/MongoDB.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ObjectRef {
    pub database: String,
    #[serde(default)]
    pub schema: String,
    pub name: String,
    pub kind: ObjectKind,
}

impl ObjectRef {
    pub fn qualified_name(&self) -> String {
        if self.schema.is_empty() { self.name.clone() } else { format!("{}.{}", self.schema, self.name) }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerInfo {
    pub product: String,
    pub version: String,
    pub connected_host: String,
    pub current_user: String,
    pub is_read_only_node: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResultColumn {
    pub name: String,
    pub type_name: String,
}

/// One grid page or statement result. Rows went through `RowCollector` (capped, cells clipped).
#[derive(Debug, Clone, PartialEq, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueryResult {
    pub columns: Vec<ResultColumn>,
    pub rows: Vec<Vec<Option<String>>>,
    /// More rows existed than were returned (the row cap or the page limit was hit).
    pub truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub affected_rows: Option<u64>,
    /// MongoDB: the documents as shell-style text, one per row, for a document view.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub documents: Option<Vec<String>>,
    pub elapsed_ms: u64,
}

/// Grid sort for `browse`: a column name the driver quotes itself (never raw SQL from the client).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sort {
    pub column: String,
    #[serde(default)]
    pub descending: bool,
}

// MARK: - SchemaModel (ER diagrams)

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SchemaModel {
    pub database: String,
    pub engine: DatabaseKindName,
    pub entities: Vec<Entity>,
    pub relationships: Vec<Relationship>,
    #[serde(default)]
    pub warnings: Vec<String>,
}

/// `DatabaseKind` as a plain string for `SchemaModel` (it only needs to round-trip, not decode tolerantly).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DatabaseKindName(pub String);

impl From<DatabaseKind> for DatabaseKindName {
    fn from(k: DatabaseKind) -> Self {
        Self(k.raw().to_owned())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Entity {
    /// Qualified name, unique inside a model ("public.orders", "orders").
    pub id: String,
    pub schema: String,
    pub name: String,
    pub kind: ObjectKind,
    pub attributes: Vec<Attribute>,
    #[serde(default)]
    pub indexes: Vec<IndexInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub row_estimate: Option<i64>,
}

impl Entity {
    pub fn new(schema: &str, name: &str, kind: ObjectKind, attributes: Vec<Attribute>) -> Self {
        let id = if schema.is_empty() { name.to_owned() } else { format!("{schema}.{name}") };
        Self {
            id,
            schema: schema.to_owned(),
            name: name.to_owned(),
            kind,
            attributes,
            indexes: Vec::new(),
            comment: None,
            row_estimate: None,
        }
    }

    pub fn primary_key(&self) -> Vec<&str> {
        self.attributes.iter().filter(|a| a.is_primary_key).map(|a| a.name.as_str()).collect()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Attribute {
    pub name: String,
    #[serde(rename = "type")]
    pub type_name: String,
    pub nullable: bool,
    #[serde(default)]
    pub is_primary_key: bool,
    #[serde(default)]
    pub is_foreign_key: bool,
    #[serde(default)]
    pub is_unique: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_value: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
    /// MongoDB inference: share of sampled documents containing the field (0...1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presence: Option<f64>,
}

impl Attribute {
    pub fn new(name: &str, type_name: &str, nullable: bool) -> Self {
        Self {
            name: name.to_owned(),
            type_name: type_name.to_owned(),
            nullable,
            is_primary_key: false,
            is_foreign_key: false,
            is_unique: false,
            default_value: None,
            comment: None,
            presence: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexInfo {
    pub name: String,
    pub columns: Vec<String>,
    pub unique: bool,
    pub primary: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Cardinality {
    /// Many child rows reference one parent row (a normal foreign key).
    ManyToOne,
    /// The FK columns are unique, so at most one child row per parent.
    OneToOne,
}

/// A reference from `from_entity(from_columns)` (child) to `to_entity(to_columns)` (parent).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Relationship {
    pub id: String,
    pub name: String,
    pub from_entity: String,
    pub from_columns: Vec<String>,
    pub to_entity: String,
    pub to_columns: Vec<String>,
    pub cardinality: Cardinality,
    /// Child side may have no parent (nullable FK columns).
    pub optional: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_delete: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_update: Option<String>,
    /// True when guessed (MongoDB), false for declared foreign keys.
    #[serde(default)]
    pub inferred: bool,
}

impl Relationship {
    /// Same id format as Swift: `from(cols,…)->to(cols,…)#name`, frozen at creation (rebuild when columns change).
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        name: &str,
        from_entity: &str,
        from_columns: Vec<String>,
        to_entity: &str,
        to_columns: Vec<String>,
        cardinality: Cardinality,
        optional: bool,
        on_delete: Option<String>,
        on_update: Option<String>,
        inferred: bool,
    ) -> Self {
        let id = format!("{from_entity}({})->{to_entity}({})#{name}", from_columns.join(","), to_columns.join(","));
        Self {
            id,
            name: name.to_owned(),
            from_entity: from_entity.to_owned(),
            from_columns,
            to_entity: to_entity.to_owned(),
            to_columns,
            cardinality,
            optional,
            on_delete,
            on_update,
            inferred,
        }
    }
}

impl SchemaModel {
    pub fn new(database: &str, engine: DatabaseKind) -> Self {
        Self {
            database: database.to_owned(),
            engine: engine.into(),
            entities: Vec::new(),
            relationships: Vec::new(),
            warnings: Vec::new(),
        }
    }

    pub fn entity(&self, id: &str) -> Option<&Entity> {
        self.entities.iter().find(|e| e.id == id)
    }

    /// Port of `finalizeRelationships()`; every driver calls it last. Drops edges whose ends aren't entities, upgrades to
    /// one-to-one (never for inferred) when the FK columns are the PK, a unique index or one unique column, sets
    /// `optional` from nullability, marks FK attributes and sorts by `(fromEntity, name)`.
    pub fn finalize_relationships(&mut self) {
        let by_id: HashMap<String, usize> = self.entities.iter().enumerate().map(|(i, e)| (e.id.clone(), i)).collect();
        self.relationships.retain(|r| by_id.contains_key(&r.from_entity) && by_id.contains_key(&r.to_entity));
        for rel in &mut self.relationships {
            let ci = by_id[&rel.from_entity];
            let child = &self.entities[ci];
            let cols: HashSet<&str> = rel.from_columns.iter().map(String::as_str).collect();
            let pk: HashSet<&str> = child.primary_key().into_iter().collect();
            let unique_index = child
                .indexes
                .iter()
                .any(|i| i.unique && i.columns.iter().map(String::as_str).collect::<HashSet<_>>() == cols);
            let single_unique = rel.from_columns.len() == 1
                && child.attributes.iter().find(|a| a.name == rel.from_columns[0]).is_some_and(|a| a.is_unique);
            if !rel.inferred && ((cols == pk && !pk.is_empty()) || unique_index || single_unique) {
                rel.cardinality = Cardinality::OneToOne;
            }
            rel.optional = child.attributes.iter().any(|a| cols.contains(a.name.as_str()) && a.nullable);
            let names: HashSet<String> = rel.from_columns.iter().cloned().collect();
            for a in &mut self.entities[ci].attributes {
                if names.contains(&a.name) {
                    a.is_foreign_key = true;
                }
            }
        }
        self.relationships.sort_by(|a, b| (&a.from_entity, &a.name).cmp(&(&b.from_entity, &b.name)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entity(name: &str, cols: &[(&str, bool, bool)]) -> Entity {
        Entity::new(
            "",
            name,
            ObjectKind::Table,
            cols.iter()
                .map(|(n, nullable, pk)| {
                    let mut a = Attribute::new(n, "int", *nullable);
                    a.is_primary_key = *pk;
                    a
                })
                .collect(),
        )
    }

    fn rel(name: &str, from: &str, cols: &[&str], to: &str, inferred: bool) -> Relationship {
        let cols: Vec<String> = cols.iter().map(|c| c.to_string()).collect();
        Relationship::new(name, from, cols, to, vec!["id".into()], Cardinality::ManyToOne, true, None, None, inferred)
    }

    #[test]
    fn finalize_matches_gokucore_rules() {
        let mut m = SchemaModel::new("db", DatabaseKind::Postgres);
        m.entities.push(entity("parent", &[("id", false, true)]));
        m.entities.push(entity("child", &[("id", false, true), ("parent_id", false, false), ("maybe", true, false)]));
        m.entities.push(entity("profile", &[("id", false, true)]));
        m.relationships.push(rel("z_fk", "child", &["parent_id"], "parent", false));
        m.relationships.push(rel("a_fk", "child", &["maybe"], "parent", false));
        m.relationships.push(rel("pk_fk", "profile", &["id"], "parent", false));
        m.relationships.push(rel("guess", "profile", &["id"], "parent", true));
        m.relationships.push(rel("gone", "child", &["parent_id"], "elsewhere", false));
        m.finalize_relationships();
        let names: Vec<_> = m.relationships.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, ["a_fk", "z_fk", "guess", "pk_fk"]);
        let by = |n: &str| m.relationships.iter().find(|r| r.name == n).unwrap();
        assert_eq!((by("z_fk").cardinality, by("z_fk").optional), (Cardinality::ManyToOne, false));
        assert!(by("a_fk").optional);
        assert_eq!(by("pk_fk").cardinality, Cardinality::OneToOne);
        assert_eq!(by("guess").cardinality, Cardinality::ManyToOne, "inferred relationships stay many-to-one");
        assert!(m.entity("child").unwrap().attributes.iter().filter(|a| a.is_foreign_key).count() == 2);
        assert_eq!(by("z_fk").id, "child(parent_id)->parent(id)#z_fk");
    }

    #[test]
    fn json_shape_matches_swift() {
        let mut a = Attribute::new("x", "text", true);
        a.presence = Some(0.5);
        let v = serde_json::to_value(&a).unwrap();
        assert_eq!(v["type"], "text");
        assert_eq!(v["isPrimaryKey"], false);
        assert!(v.get("defaultValue").is_none(), "nil fields are absent, like Swift's encoder");
        let k = serde_json::to_value(ObjectKind::MaterializedView).unwrap();
        assert_eq!(k, "materializedView");
    }
}
