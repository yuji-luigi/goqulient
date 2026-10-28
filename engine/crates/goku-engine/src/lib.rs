//! The Goku database engine (docs/ARCHITECTURE.md). Rules that hold for everything in this crate:
//! - every network wait goes through `deadline::with_deadline`, whose `on_timeout` closes the socket or cancels the query;
//! - results go through `limits::RowCollector` (row cap + cell clipping);
//! - secrets never reach a log, an error or an endpoint summary (`redact`);
//! - no stdout and no runtime of its own: the host (the stdio server, later the app) owns the one tokio runtime.
pub mod deadline;
pub mod drivers;
pub mod er;
pub mod error;
pub mod limits;
pub mod model;
pub mod profile;
pub mod redact;
pub mod session;

pub use error::{DbError, Result};

/// The engine's version, reported by `initialize`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
