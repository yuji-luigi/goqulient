//! One module per engine; each owns its connection setup, catalog, browse and introspection.
pub mod mongo;
pub mod mysql;
pub mod postgres;

/// `Mutex::lock` for the drivers' short, non-async critical sections. A poisoned lock means another task panicked
/// halfway through an update, so its state can't be trusted: that is a bug, not an error to handle.
#[expect(clippy::unwrap_used, reason = "lock poisoning is a bug elsewhere")]
pub(crate) fn lock<T>(m: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap()
}
