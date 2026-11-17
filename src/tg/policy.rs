//! Retry policy: jittered exponential backoff that honours `retry_after`.

use std::future::Future;

use rand::Rng;
use rand::SeedableRng;
use rand::rngs::StdRng;

use super::transport::TgError;

#[derive(Debug, Clone, Copy)]
pub struct RetryPolicy {
    pub attempts: u32,
    pub base_delay_ms: u64,
    pub max_delay_ms: u64,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            attempts: 4,
            base_delay_ms: 1_000,
            max_delay_ms: 30_000,
        }
    }
}

impl RetryPolicy {
    pub fn no_retries() -> Self {
        Self {
            attempts: 1,
            base_delay_ms: 0,
            max_delay_ms: 0,
        }
    }

    /// Delay before attempt `attempt` (1-based), with jitter.
    pub fn delay_ms(&self, attempt: u32, error: &TgError, rng: &mut impl Rng) -> u64 {
        if let Some(secs) = error.retry_after_secs() {
            // Telegram knows best; add a small cushion
            return secs.saturating_mul(1_000) + rng.random_range(0..=500);
        }
        let exponential = self
            .base_delay_ms
            .saturating_mul(2u64.saturating_pow(attempt.saturating_sub(1)))
            .min(self.max_delay_ms);
        if exponential == 0 {
            return 0;
        }
        let jitter = rng.random_range(0..=(exponential / 4).max(1));
        (exponential + jitter).min(self.max_delay_ms)
    }

    /// Run `op`, retrying transient failures and rate limits.
    pub async fn run<T, F, Fut>(&self, mut op: F) -> Result<T, TgError>
    where
        F: FnMut() -> Fut,
        Fut: Future<Output = Result<T, TgError>>,
    {
        self.run_with_rng(&mut StdRng::from_os_rng(), &mut op).await
    }

    pub async fn run_with_rng<T, F, Fut>(
        &self,
        rng: &mut impl Rng,
        op: &mut F,
    ) -> Result<T, TgError>
    where
        F: FnMut() -> Fut,
        Fut: Future<Output = Result<T, TgError>>,
    {
        let attempts = self.attempts.max(1);
        let mut last_error: Option<TgError> = None;
        for attempt in 1..=attempts {
            match op().await {
                Ok(value) => return Ok(value),
                Err(err) if !err.is_retryable() => return Err(err),
                Err(err) => {
                    let delay = self.delay_ms(attempt, &err, rng);
                    if attempt == attempts {
                        last_error = Some(err);
                        break;
                    }
                    tracing::warn!(
                        attempt,
                        delay_ms = delay,
                        "telegram send failed, retrying: {err}"
                    );
                    if delay > 0 {
                        tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
                    }
                    last_error = Some(err);
                }
            }
        }
        Err(last_error.unwrap_or(TgError::Transient("gave up".into())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::SeedableRng;
    use std::cell::Cell;

    fn rng(seed: u64) -> StdRng {
        StdRng::seed_from_u64(seed)
    }

    #[test]
    fn honours_retry_after() {
        let policy = RetryPolicy::default();
        let delay = policy.delay_ms(1, &TgError::RateLimited { retry_after: 7 }, &mut rng(1));
        assert!(delay >= 7000, "got {delay}");
        assert!(delay < 8000, "got {delay}");
    }

    #[test]
    fn exponential_without_retry_after() {
        let policy = RetryPolicy {
            attempts: 5,
            base_delay_ms: 100,
            max_delay_ms: 1000,
        };
        let d1 = policy.delay_ms(1, &TgError::Transient("x".into()), &mut rng(2));
        let d3 = policy.delay_ms(3, &TgError::Transient("x".into()), &mut rng(2));
        assert!((100..=125).contains(&d1), "got {d1}");
        assert!((400..=500).contains(&d3), "got {d3}");
        // capped
        let d9 = policy.delay_ms(9, &TgError::Transient("x".into()), &mut rng(2));
        assert_eq!(d9, 1000);
    }

    #[tokio::test]
    async fn retries_transient_then_succeeds() {
        let policy = RetryPolicy {
            attempts: 3,
            base_delay_ms: 1,
            max_delay_ms: 5,
        };
        let calls = Cell::new(0);
        let result: Result<&str, TgError> = policy
            .run(|| {
                let n = calls.get() + 1;
                calls.set(n);
                async move {
                    if n < 3 {
                        Err(TgError::Transient("blip".into()))
                    } else {
                        Ok("sent")
                    }
                }
            })
            .await;
        assert_eq!(result.unwrap(), "sent");
        assert_eq!(calls.get(), 3);
    }

    #[tokio::test]
    async fn permanent_errors_are_not_retried() {
        let policy = RetryPolicy {
            attempts: 5,
            base_delay_ms: 1,
            max_delay_ms: 2,
        };
        let calls = Cell::new(0);
        let result: Result<(), TgError> = policy
            .run(|| {
                calls.set(calls.get() + 1);
                async move { Err(TgError::Permanent("chat not found".into())) }
            })
            .await;
        assert!(result.is_err());
        assert_eq!(calls.get(), 1);
    }

    #[tokio::test]
    async fn gives_up_after_attempts() {
        let policy = RetryPolicy {
            attempts: 2,
            base_delay_ms: 1,
            max_delay_ms: 2,
        };
        let calls = Cell::new(0);
        let result: Result<(), TgError> = policy
            .run(|| {
                calls.set(calls.get() + 1);
                async move { Err(TgError::Transient("down".into())) }
            })
            .await;
        assert!(matches!(result, Err(TgError::Transient(_))));
        assert_eq!(calls.get(), 2);
    }
}
