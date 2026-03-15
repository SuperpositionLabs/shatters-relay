use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

const PRECISION: u64 = 1_000_000;

pub struct TokenBucket {
    tokens: AtomicU64,
    last_refill_nanos: AtomicU64,
    epoch: Instant,
    rate: u64,
    capacity: u64,
}

impl TokenBucket {
    pub fn new(tokens_per_second: u32, burst: u32) -> Self {
        let capacity = (burst as u64) * PRECISION;
        Self {
            tokens: AtomicU64::new(capacity),
            last_refill_nanos: AtomicU64::new(0),
            epoch: Instant::now(),
            rate: (tokens_per_second as u64) * PRECISION,
            capacity,
        }
    }

    pub fn try_acquire(&self, cost: u32) -> bool {
        self.refill();

        let required = (cost as u64) * PRECISION;

        loop {
            let current = self.tokens.load(Ordering::Acquire);
            if current < required {
                return false;
            }
            let new = current - required;
            match self.tokens.compare_exchange_weak(
                current,
                new,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return true,
                Err(_) => continue,
            }
        }
    }

    pub fn try_acquire_one(&self) -> bool {
        self.try_acquire(1)
    }

    fn refill(&self) {
        let now_nanos = self.epoch.elapsed().as_nanos() as u64;
        let prev_nanos = self.last_refill_nanos.swap(now_nanos, Ordering::AcqRel);

        let elapsed_nanos = now_nanos.saturating_sub(prev_nanos);
        if elapsed_nanos == 0 {
            return;
        }

        let add = self
            .rate
            .checked_mul(elapsed_nanos)
            .map(|v| v / 1_000_000_000)
            .unwrap_or(self.capacity);

        if add == 0 {
            return;
        }

        loop {
            let current = self.tokens.load(Ordering::Acquire);
            let new = (current + add).min(self.capacity);
            if new == current {
                break;
            }
            match self.tokens.compare_exchange_weak(
                current,
                new,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => break,
                Err(_) => continue,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn burst_allows_initial_requests() {
        let tb = TokenBucket::new(10, 5);
        for _ in 0..5 {
            assert!(tb.try_acquire_one(), "should allow up to burst");
        }
        assert!(!tb.try_acquire_one(), "should reject after burst exhausted");
    }

    #[test]
    fn refill_adds_tokens_over_time() {
        let tb = TokenBucket::new(100, 10);
        for _ in 0..10 {
            assert!(tb.try_acquire_one());
        }
        assert!(!tb.try_acquire_one());

        thread::sleep(Duration::from_millis(60));
        assert!(tb.try_acquire_one(), "should have refilled after sleep");
    }

    #[test]
    fn cost_greater_than_one() {
        let tb = TokenBucket::new(10, 10);
        assert!(tb.try_acquire(5));
        assert!(tb.try_acquire(5));
        assert!(!tb.try_acquire(1), "should be empty");
    }

    #[test]
    fn capacity_is_capped() {
        let tb = TokenBucket::new(1000, 5);
        thread::sleep(Duration::from_millis(100));
        assert!(tb.try_acquire(5));
        assert!(!tb.try_acquire(1));
    }

    #[test]
    fn concurrent_access() {
        let tb = Arc::new(TokenBucket::new(1000, 100));
        let total_acquired = Arc::new(AtomicU64::new(0));
        let mut handles = vec![];

        for _ in 0..10 {
            let tb = Arc::clone(&tb);
            let counter = Arc::clone(&total_acquired);
            handles.push(thread::spawn(move || {
                for _ in 0..20 {
                    if tb.try_acquire_one() {
                        counter.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }));
        }

        for h in handles {
            h.join().unwrap();
        }

        let acquired = total_acquired.load(Ordering::Relaxed);
        assert!(acquired >= 100, "at least burst should succeed: got {acquired}");
        assert!(acquired <= 200, "can't exceed total attempts: got {acquired}");
    }
}
