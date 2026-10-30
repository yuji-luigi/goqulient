//! Query text for the editor: the SQL splitter and read-only checks (`sql`) and the danger check (`danger`), ports of
//! GokuCore's checked against `parity/`. The Mongo shell parser is `drivers::mongo::shell`.
pub mod danger;
pub mod sql;

pub use danger::Finding;
