//! Not implemented yet (see the VSX tickets in docs/vscode-extension.md).
use crate::error::{DbError, Result};
use crate::profile::{ConnectionProfile, Credentials};
use crate::session::DatabaseSession;
use tokio_util::sync::CancellationToken;

pub async fn connect(
    _profile: ConnectionProfile,
    _credentials: Credentials,
    _cancel: &CancellationToken,
) -> Result<Box<dyn DatabaseSession>> {
    Err(DbError::Unsupported("MySQL isn't implemented in the engine yet".into()))
}
