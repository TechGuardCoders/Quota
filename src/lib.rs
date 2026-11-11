//! Quota: per-tenant token/request budgets with scheduled reset.
//!
//! Rust port of the Python `quota.py` reference implementation. Same
//! semantics: fixed window per tenant, 80% warn flag, structured `Denial`
//! with `retry_after`, window reset on rollover, injectable clock, full
//! tenant isolation. Never blocks, never panics on user input.

use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Structured rejection returned when a tenant is over budget.
#[derive(Debug, Clone, PartialEq)]
pub struct Denial {
    pub tenant: String,
    pub requested: u64,
    pub remaining: u64,
    pub retry_after: Duration,
    pub reason: String,
}

/// Result of `Quota::allow`: `Allowed` (truthy) or `Denied` (structured).
#[derive(Debug, Clone, PartialEq)]
pub enum Decision {
    Allowed,
    Denied(Denial),
}

impl Decision {
    pub fn is_allowed(&self) -> bool {
        matches!(self, Decision::Allowed)
    }
}

struct Window {
    started_at: Instant,
    used: u64,
}

/// Injectable clock so tests advance time without sleeping.
pub trait Clock {
    fn now(&self) -> Instant;
}

/// Real monotonic clock (default).
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
}

/// Per-tenant budget with scheduled reset.
pub struct Quota<C: Clock = SystemClock> {
    limit: u64,
    window_s: Duration,
    clock: C,
    windows: HashMap<String, Window>,
}

impl Quota<SystemClock> {
    pub fn new(limit: u64, window_s: Duration) -> Result<Self, String> {
        Quota::with_clock(limit, window_s, SystemClock)
    }
}

impl<C: Clock> Quota<C> {
    pub fn with_clock(limit: u64, window_s: Duration, clock: C) -> Result<Self, String> {
        if limit == 0 {
            return Err("limit must be positive".into());
        }
        if window_s.is_zero() {
            return Err("window_s must be positive".into());
        }
        Ok(Quota {
            limit,
            window_s,
            clock,
            windows: HashMap::new(),
        })
    }

    fn refresh(&mut self, tenant: &str) {
        let now = self.clock.now();
        if let Some(w) = self.windows.get_mut(tenant) {
            if now.duration_since(w.started_at) >= self.window_s {
                w.started_at = now;
                w.used = 0;
            }
        }
    }

    /// Ensure a window exists for the tenant (created lazily, like the
    /// Python `_window_for`), then return its (started_at, used) snapshot.
    fn ensure_window(&mut self, tenant: &str) -> (Instant, u64) {
        self.refresh(tenant);
        let now = self.clock.now();
        let win = self.windows.entry(tenant.to_string()).or_insert_with(|| Window { started_at: now, used: 0 });
        (win.started_at, win.used)
    }

    fn retry_after_since(&self, started_at: Instant) -> Duration {
        let elapsed = self.clock.now().duration_since(started_at);
        self.window_s.saturating_sub(elapsed)
    }

    /// Try to consume `tokens` for `tenant`.
    ///
    /// Returns `Decision::Allowed` on success; a structured `Decision::Denied`
    /// over budget. A request larger than the whole limit is rejected
    /// immediately (it can never fit in any window).
    pub fn allow(&mut self, tenant: &str, tokens: u64) -> Decision {
        let (started_at, used) = self.ensure_window(tenant);

        if tokens > self.limit {
            return Decision::Denied(Denial {
                tenant: tenant.to_string(),
                requested: tokens,
                remaining: self.limit.saturating_sub(used),
                retry_after: self.retry_after_since(started_at),
                reason: "request_exceeds_total_limit".into(),
            });
        }

        if used + tokens > self.limit {
            return Decision::Denied(Denial {
                tenant: tenant.to_string(),
                requested: tokens,
                remaining: self.limit - used,
                retry_after: self.retry_after_since(started_at),
                reason: "quota_exceeded".into(),
            });
        }

        self.windows.get_mut(tenant).unwrap().used = used + tokens;
        Decision::Allowed
    }

    /// True once the tenant has consumed >= 80% of the limit.
    pub fn warn(&mut self, tenant: &str) -> bool {
        self.refresh(tenant);
        match self.windows.get(tenant) {
            None => false,
            Some(win) => (win.used as f64) >= 0.8 * self.limit as f64,
        }
    }

    /// Tokens left for the tenant in the current window.
    pub fn remaining(&mut self, tenant: &str) -> u64 {
        let (_, used) = self.ensure_window(tenant);
        self.limit.saturating_sub(used)
    }

    /// Tokens consumed by the tenant in the current window.
    pub fn usage(&mut self, tenant: &str) -> u64 {
        let (_, used) = self.ensure_window(tenant);
        used
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::rc::Rc;

    impl Clock for Rc<Cell<Instant>> {
        fn now(&self) -> Instant {
            self.get()
        }
    }

    // The Python tests advance the clock the Quota holds. In Rust we model
    // that with a shared cell the Quota reads through its Clock impl.
    fn make_shared_quota(limit: u64, window_s: Duration) -> (Quota<Rc<Cell<Instant>>>, Rc<Cell<Instant>>) {
        let clock: Rc<Cell<Instant>> = Rc::new(Cell::new(Instant::now()));
        let quota = Quota::with_clock(limit, window_s, clock.clone()).unwrap();
        (quota, clock)
    }

    #[test]
    fn allow_consumes_within_limit() {
        let (mut q, _) = make_shared_quota(100, Duration::from_secs(100));
        assert!(q.allow("acme", 60).is_allowed());
        assert_eq!(q.usage("acme"), 60);
        assert_eq!(q.remaining("acme"), 40);
    }

    #[test]
    fn tenants_are_isolated() {
        let (mut q, _) = make_shared_quota(100, Duration::from_secs(100));
        assert!(q.allow("acme", 100).is_allowed());
        assert!(q.allow("other", 100).is_allowed());
        assert_eq!(q.usage("acme"), 100);
        assert_eq!(q.usage("other"), 100);
    }

    #[test]
    fn over_limit_returns_structured_denial() {
        let (mut q, _) = make_shared_quota(100, Duration::from_secs(100));
        q.allow("acme", 80);
        match q.allow("acme", 30) {
            Decision::Denied(d) => {
                assert_eq!(d.tenant, "acme");
                assert_eq!(d.requested, 30);
                assert_eq!(d.remaining, 20);
                assert_eq!(d.reason, "quota_exceeded");
                assert!(d.retry_after > Duration::ZERO);
            }
            _ => panic!("expected denial"),
        }
    }

    #[test]
    fn denial_never_raises_or_hangs() {
        let (mut q, _) = make_shared_quota(10, Duration::from_secs(100));
        for _ in 0..50 {
            let _ = q.allow("acme", 7); // must not panic; returns a decision
        }
    }

    #[test]
    fn warn_at_80_percent() {
        let (mut q, _) = make_shared_quota(100, Duration::from_secs(100));
        q.allow("acme", 79);
        assert!(!q.warn("acme"));
        q.allow("acme", 1);
        assert!(q.warn("acme"));
    }

    #[test]
    fn warn_false_for_unknown_tenant() {
        let (mut q, _) = make_shared_quota(1000, Duration::from_secs(100));
        assert!(!q.warn("nobody"));
    }

    #[test]
    fn reset_on_window_rollover() {
        let (mut q, clock) = make_shared_quota(100, Duration::from_secs(60));
        assert!(q.allow("acme", 100).is_allowed());
        assert!(!q.allow("acme", 1).is_allowed()); // denial
        clock.set(clock.get() + Duration::from_secs(60));
        assert_eq!(q.usage("acme"), 0);
        assert!(q.allow("acme", 100).is_allowed());
    }

    #[test]
    fn retry_after_counts_down_to_reset() {
        let (mut q, clock) = make_shared_quota(100, Duration::from_secs(60));
        q.allow("acme", 100);
        if let Decision::Denied(d) = q.allow("acme", 1) {
            assert!(d.retry_after > Duration::ZERO && d.retry_after <= Duration::from_secs(60));
        } else {
            panic!("expected denial");
        }
        clock.set(clock.get() + Duration::from_secs(30));
        if let Decision::Denied(d) = q.allow("acme", 1) {
            assert!(d.retry_after > Duration::ZERO && d.retry_after <= Duration::from_secs(30));
        } else {
            panic!("expected denial");
        }
    }

    #[test]
    fn request_larger_than_total_limit_rejected() {
        let (mut q, _) = make_shared_quota(100, Duration::from_secs(100));
        match q.allow("acme", 101) {
            Decision::Denied(d) => assert_eq!(d.reason, "request_exceeds_total_limit"),
            _ => panic!("expected denial"),
        }
    }

    #[test]
    fn invalid_config_errors() {
        assert!(Quota::new(0, Duration::from_secs(10)).is_err());
        assert!(Quota::new(10, Duration::ZERO).is_err());
    }

    #[test]
    fn zero_tokens_is_free_but_tracked() {
        let (mut q, _) = make_shared_quota(100, Duration::from_secs(100));
        assert!(q.allow("acme", 0).is_allowed());
        assert_eq!(q.usage("acme"), 0);
    }

    // Rust-specific: negative tokens are unrepresentable (u64), so the
    // Python `test_negative_tokens_raises` has no direct port — the type
    // system enforces it at compile time.
}
