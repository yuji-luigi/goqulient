//! Port of GokuCore's `withDeadline` (AGENTS.md, "Safety"): every network wait in the engine goes through
//! `with_deadline`.
//! Dropping a Rust future stops it, but the socket or server query it started may live on (a spawned connection task, a
//! running statement), so `on_timeout` must close the socket or cancel the server query.
use crate::error::{DbError, Result};
use std::future::Future;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

/// Runs `operation`; fails with `DbError::Timeout` after `seconds`, or `DbError::Cancelled` when `cancel` fires. Either
/// way `on_timeout` runs exactly once and the operation is dropped. A result that is ready wins over an expired timer.
pub async fn with_deadline<T, F, C>(
    seconds: f64,
    what: &str,
    cancel: &CancellationToken,
    on_timeout: C,
    operation: F,
) -> Result<T>
where
    F: Future<Output = Result<T>>,
    C: FnOnce(),
{
    if cancel.is_cancelled() {
        on_timeout();
        return Err(DbError::Cancelled);
    }
    let timer = tokio::time::sleep(Duration::from_secs_f64(seconds.max(0.001)));
    tokio::select! {
        biased;
        result = operation => result,
        _ = cancel.cancelled() => { on_timeout(); Err(DbError::Cancelled) }
        _ = timer => {
            on_timeout();
            Err(DbError::Timeout(format!("{what} exceeded {}s", seconds as i64)))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn counter() -> (Arc<AtomicUsize>, impl FnOnce()) {
        let n = Arc::new(AtomicUsize::new(0));
        let m = n.clone();
        (n, move || {
            m.fetch_add(1, Ordering::SeqCst);
        })
    }

    #[tokio::test(start_paused = true)]
    async fn result_before_deadline_skips_on_timeout() {
        let (n, hook) = counter();
        let r = with_deadline(1.0, "op", &CancellationToken::new(), hook, async { Ok(7) }).await;
        assert_eq!(r, Ok(7));
        assert_eq!(n.load(Ordering::SeqCst), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn stuck_operation_times_out_and_closes_once() {
        let (n, hook) = counter();
        let r: Result<()> =
            with_deadline(2.5, "connect", &CancellationToken::new(), hook, std::future::pending()).await;
        assert_eq!(r, Err(DbError::Timeout("connect exceeded 2s".into())));
        assert_eq!(r.unwrap_err().to_string(), "Timed out: connect exceeded 2s");
        assert_eq!(n.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn cancel_while_waiting() {
        let (n, hook) = counter();
        let token = CancellationToken::new();
        let t = token.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(10)).await;
            t.cancel();
        });
        let r: Result<()> = with_deadline(60.0, "query", &token, hook, std::future::pending()).await;
        assert_eq!(r, Err(DbError::Cancelled));
        assert_eq!(n.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn already_cancelled_never_starts_the_operation() {
        let (n, hook) = counter();
        let token = CancellationToken::new();
        token.cancel();
        let started = Arc::new(AtomicUsize::new(0));
        let s = started.clone();
        let r = with_deadline(60.0, "query", &token, hook, async move {
            s.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
        .await;
        assert_eq!(r, Err(DbError::Cancelled));
        assert_eq!((n.load(Ordering::SeqCst), started.load(Ordering::SeqCst)), (1, 0));
    }

    #[tokio::test(start_paused = true)]
    async fn operation_error_passes_through() {
        let (n, hook) = counter();
        let r: Result<()> =
            with_deadline(1.0, "op", &CancellationToken::new(), hook, async { Err(DbError::Query("boom".into())) })
                .await;
        assert_eq!(r, Err(DbError::Query("boom".into())));
        assert_eq!(n.load(Ordering::SeqCst), 0);
    }
}
