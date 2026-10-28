//! Port of GokuCore's `MongoSchemaInferrer` + `MongoSession.introspect`: fields from sampled
//! documents, relationships from DBRefs, name matches (`customerId` → `customers`) and bounded value probes.
use super::MongoSession;
use super::format::type_name;
use super::swallow;
use crate::error::{DbError, Result};
use crate::model::{Attribute, Cardinality, Entity, IndexInfo, ObjectKind, ObjectRef, Relationship, SchemaModel};
use crate::profile::DatabaseKind;
use bson::{Bson, Document, doc};
use std::collections::{BTreeSet, HashMap, HashSet};
use tokio_util::sync::CancellationToken;
use unicode_segmentation::UnicodeSegmentation;

/// Documents sampled per collection.
pub const SAMPLE_SIZE: usize = 100;
/// Hard cap on analysed collections so huge databases stay responsive.
pub const MAX_COLLECTIONS: usize = 300;
/// Nested fields deeper than this are summarised as their parent type.
const MAX_DEPTH: usize = 3;
/// Array elements examined per array.
const MAX_ELEMENTS: usize = 20;
/// Value probes (`find {_id: {$in: samples}}`) per introspection.
const MAX_PROBES: usize = 200;

#[derive(Default)]
pub struct FieldStats {
    pub count: usize,
    pub types: BTreeSet<&'static str>,
    pub object_id_samples: Vec<Bson>,
    /// Small scalar values (ints, short strings) kept to confirm non-ObjectId references.
    pub scalar_samples: Vec<Bson>,
    pub dbref_targets: BTreeSet<String>,
}

/// "path → stats" over many documents, paths in first-seen order. Array element fields are written `items[].productId`.
#[derive(Default)]
pub struct Fields {
    pub stats: HashMap<String, FieldStats>,
    pub order: Vec<String>,
    pub documents: usize,
}

impl Fields {
    pub fn from_documents(docs: &[Document]) -> Self {
        let mut f = Fields::default();
        for d in docs {
            let mut seen = HashSet::new();
            f.walk(d, "", 0, &mut seen);
        }
        f.documents = docs.len();
        if let Some(i) = f.order.iter().position(|p| p == "_id")
            && i != 0
        {
            let id = f.order.remove(i);
            f.order.insert(0, id);
        }
        f
    }

    fn walk(&mut self, doc: &Document, prefix: &str, depth: usize, seen: &mut HashSet<String>) {
        for (k, v) in doc {
            let path = if prefix.is_empty() { k.clone() } else { format!("{prefix}.{k}") };
            self.record(v, path, depth, seen);
        }
    }

    fn record(&mut self, value: &Bson, path: String, depth: usize, seen: &mut HashSet<String>) {
        if !self.stats.contains_key(&path) {
            self.order.push(path.clone());
        }
        let s = self.stats.entry(path.clone()).or_default();
        if seen.insert(path.clone()) {
            s.count += 1;
        }
        s.types.insert(type_name(value));
        if matches!(value, Bson::ObjectId(_)) && s.object_id_samples.len() < 10 {
            s.object_id_samples.push(value.clone());
        }
        let small = match value {
            Bson::Int32(_) | Bson::Int64(_) => true,
            Bson::String(t) => t.graphemes(true).count() <= 64,
            _ => false,
        };
        if small && s.scalar_samples.len() < 10 {
            s.scalar_samples.push(value.clone());
        }
        match value {
            Bson::Document(sub) => {
                if let (Ok(target), true) = (sub.get_str("$ref"), sub.contains_key("$id")) {
                    s.dbref_targets.insert(target.to_owned());
                    return;
                }
                if depth < MAX_DEPTH {
                    self.walk(sub, &path, depth + 1, seen);
                }
            }
            Bson::Array(items) if depth < MAX_DEPTH => {
                let element_path = format!("{path}[]");
                for e in items.iter().take(MAX_ELEMENTS) {
                    match e {
                        Bson::Document(inner) => self.walk(inner, &element_path, depth + 1, seen),
                        Bson::ObjectId(_) => self.record(e, element_path.clone(), depth + 1, seen),
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }

    /// Attributes in path order: type = union of types (null removed), `nullable = presence < 1 || null seen`.
    pub fn attributes(&self) -> Vec<Attribute> {
        self.order
            .iter()
            .map(|path| {
                let s = &self.stats[path];
                let presence = if self.documents == 0 { 0.0 } else { s.count as f64 / self.documents as f64 };
                let has_null = s.types.contains("null");
                let types: Vec<&str> = s.types.iter().copied().filter(|t| *t != "null").collect();
                let type_name = if types.is_empty() { "null".to_owned() } else { types.join(" | ") };
                let mut a = Attribute::new(path, &type_name, presence < 1.0 || has_null);
                a.presence = Some(presence);
                a.is_primary_key = path == "_id";
                a
            })
            .collect()
    }
}

/// Normalised name used to match `customerId` → `customers`.
pub fn stem(s: &str) -> String {
    let mut t: String = s.to_lowercase().chars().filter(|c| *c != '_' && *c != '-').collect();
    if let Some(base) = t.strip_suffix("ies") {
        t = format!("{base}y");
    } else if t.ends_with("ses") {
        t.truncate(t.len() - 2);
    } else if t.ends_with('s') && !t.ends_with("ss") {
        t.pop();
    }
    t
}

/// `customerId`, `customer_id`, `productIds`, `items[].productId` → "customer"/"product".
pub fn reference_prefix(path: &str) -> Option<String> {
    let last = path.split('.').rfind(|p| !p.is_empty()).unwrap_or(path).replace("[]", "");
    if last == "_id" {
        return None;
    }
    ["_ids", "Ids", "IDs", "_id", "Id", "ID"]
        .iter()
        .find(|suffix| last.ends_with(*suffix) && last.chars().count() > suffix.len())
        .map(|suffix| last[..last.len() - suffix.len()].to_owned())
}

fn relationship(entity: &str, path: &str, target: &str) -> Relationship {
    Relationship::new(
        &format!("{entity}.{path}"),
        entity,
        vec![path.to_owned()],
        target,
        vec!["_id".into()],
        Cardinality::ManyToOne,
        true,
        None,
        None,
        true,
    )
}

impl MongoSession {
    pub(super) async fn introspect_database(&self, database: &str, cancel: &CancellationToken) -> Result<SchemaModel> {
        let collections: Vec<ObjectRef> =
            self.list_objects(database, cancel).await?.into_iter().filter(|o| !o.name.starts_with("system.")).collect();
        let mut model = SchemaModel::new(database, DatabaseKind::Mongodb);
        if collections.len() > MAX_COLLECTIONS {
            model
                .warnings
                .push(format!("Only the first {MAX_COLLECTIONS} of {} collections were analysed.", collections.len()));
        }
        let mut object_id_fields: Vec<(String, String, Vec<Bson>)> = Vec::new();
        let mut named_fields: Vec<(String, String, Vec<Bson>)> = Vec::new();
        let mut dbrefs: Vec<(String, String, String)> = Vec::new();
        for r in collections.iter().take(MAX_COLLECTIONS) {
            if cancel.is_cancelled() {
                return Err(DbError::Cancelled);
            }
            let count = swallow(self.count(database, &r.name, cancel).await)?.flatten();
            let docs = self.sample(database, &r.name, SAMPLE_SIZE, count, cancel).await?;
            let fields = Fields::from_documents(&docs);
            let mut entity = Entity::new("", &r.name, r.kind, fields.attributes());
            for path in &fields.order {
                let s = &fields.stats[path];
                if !s.object_id_samples.is_empty() && path != "_id" {
                    object_id_fields.push((r.name.clone(), path.clone(), s.object_id_samples.clone()));
                } else if !s.scalar_samples.is_empty() && reference_prefix(path).is_some() {
                    named_fields.push((r.name.clone(), path.clone(), s.scalar_samples.clone()));
                }
                dbrefs.extend(s.dbref_targets.iter().map(|t| (r.name.clone(), path.clone(), t.clone())));
            }
            let indexes = swallow(self.list_indexes(database, &r.name, cancel).await)?.unwrap_or_default();
            for d in indexes {
                let Ok(key) = d.get_document("key") else { continue };
                let unique = d.get_bool("unique").unwrap_or(false);
                let name = d.get_str("name").unwrap_or("?").to_owned();
                let columns: Vec<String> = key.keys().cloned().collect();
                if unique
                    && columns.len() == 1
                    && let Some(a) = entity.attributes.iter_mut().find(|a| a.name == columns[0])
                {
                    a.is_unique = true;
                }
                entity.indexes.push(IndexInfo {
                    unique: unique || name == "_id_",
                    primary: name == "_id_",
                    name,
                    columns,
                });
            }
            entity.row_estimate = count;
            model.entities.push(entity);
        }

        // DBRefs are explicit; ObjectId fields are matched by name, then confirmed by value.
        let names: HashSet<&str> = model.entities.iter().map(|e| e.name.as_str()).collect();
        let mut by_stem: HashMap<String, String> = HashMap::new();
        for e in &model.entities {
            by_stem.entry(stem(&e.name)).or_insert_with(|| e.name.clone());
        }
        let mut relationships: Vec<Relationship> = dbrefs
            .iter()
            .filter(|(_, _, target)| names.contains(target.as_str()))
            .map(|(entity, path, target)| relationship(entity, path, target))
            .collect();
        let collections: Vec<&str> =
            model.entities.iter().filter(|e| e.kind == ObjectKind::Collection).map(|e| e.name.as_str()).collect();
        let mut probes = 0;
        for (entity, path, samples) in &object_id_fields {
            let mut target = reference_prefix(path).and_then(|p| by_stem.get(&stem(&p)).cloned());
            if target.is_none() && probes < MAX_PROBES {
                // Value match: which collection contains these ids? Bounded number of probes.
                for other in collections.iter().filter(|c| *c != entity) {
                    probes += 1;
                    if self.contains_ids(database, other, samples, cancel).await? {
                        target = Some((*other).to_owned());
                        break;
                    }
                    if probes >= MAX_PROBES {
                        break;
                    }
                }
            }
            match target {
                Some(t) if t != *entity || path.contains("parent") => {
                    relationships.push(relationship(entity, path, &t))
                }
                _ => {}
            }
        }
        // Non-ObjectId references (e.g. numeric ids): require a name match *and* a value match.
        for (entity, path, samples) in &named_fields {
            let Some(t) = reference_prefix(path).and_then(|p| by_stem.get(&stem(&p)).cloned()) else { continue };
            if t != *entity && self.contains_ids(database, &t, samples, cancel).await? {
                relationships.push(relationship(entity, path, &t));
            }
        }
        model.relationships = relationships;
        model.warnings.push(format!(
            "MongoDB has no declared schema: fields and relationships are inferred from up to {SAMPLE_SIZE} sampled documents per collection."
        ));
        model.finalize_relationships();
        Ok(model)
    }

    /// Whether `collection` has a document whose `_id` is one of `samples`. Errors count as no (GokuCore's `try?`).
    async fn contains_ids(
        &self,
        database: &str,
        collection: &str,
        samples: &[Bson],
        cancel: &CancellationToken,
    ) -> Result<bool> {
        let ids = samples.to_vec();
        let cmd = doc! {
            "find": collection, "filter": {"_id": {"$in": ids}}, "projection": {"_id": 1},
            "limit": 1, "singleBatch": true, "maxTimeMS": 2000,
        };
        let reply = swallow(self.command(database, cmd, cancel).await)?;
        Ok(reply.is_some_and(|r| {
            r.get_document("cursor").ok().and_then(|c| c.get_array("firstBatch").ok()).is_some_and(|b| !b.is_empty())
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bson::oid::ObjectId;

    #[test]
    fn stems_and_prefixes() {
        assert_eq!(stem("Categories"), "category");
        assert_eq!(stem("addresses"), "address");
        assert_eq!(stem("order_items"), "orderitem");
        assert_eq!(stem("class"), "class");
        assert_eq!(reference_prefix("customerId").as_deref(), Some("customer"));
        assert_eq!(reference_prefix("items[].productId").as_deref(), Some("product"));
        assert_eq!(reference_prefix("tagIds[]").as_deref(), Some("tag"));
        assert_eq!(reference_prefix("legacy_id").as_deref(), Some("legacy"));
        assert_eq!(reference_prefix("owner._id"), None);
        assert_eq!(reference_prefix("Id"), None);
        assert_eq!(reference_prefix("name"), None);
    }

    #[test]
    fn walk_limits_depth_and_arrays() {
        let oid = ObjectId::new();
        let many: Vec<Bson> = (0..25).map(|i| Bson::Document(doc! {"i": i})).collect();
        let docs = [
            doc! {"x": 1, "_id": 1, "n": {"a": {"b": {"c": {"d": 1}}}}, "many": many, "ids": [oid, oid], "r": {"$ref": "u", "$id": 1}},
            doc! {"_id": 2, "x": null, "late": [{"i": 1}]},
        ];
        let f = Fields::from_documents(&docs);
        assert_eq!(f.order[0], "_id");
        assert!(f.stats.contains_key("n.a.b.c") && !f.stats.contains_key("n.a.b.c.d"));
        assert_eq!(f.stats["many[].i"].count, 1, "counted once per document");
        assert_eq!(f.stats["ids[]"].object_id_samples.len(), 2);
        assert_eq!(f.stats["r"].dbref_targets.iter().collect::<Vec<_>>(), ["u"]);
        assert!(!f.stats.contains_key("r.$id"));
        let attrs = f.attributes();
        let x = attrs.iter().find(|a| a.name == "x").unwrap();
        assert_eq!((x.type_name.as_str(), x.nullable, x.presence), ("int", true, Some(1.0)));
        let late = attrs.iter().find(|a| a.name == "late[].i").unwrap();
        assert_eq!(late.presence, Some(0.5));
    }
}
