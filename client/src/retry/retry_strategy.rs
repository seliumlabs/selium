use std::time::Duration;

const DEFAULT_ATTEMPTS_AMOUNT: u32 = 5;
const DEFAULT_DURATION_STEP: Duration = Duration::from_secs(1);

#[derive(Debug, Clone)]
pub struct RetryStrategy(Vec<Duration>);

impl RetryStrategy {
    pub fn linear(attempts: u32, step: Duration) -> Self {
        let strategy = (1..=attempts).map(|i| i * step).collect();
        Self(strategy)
    }

    pub fn constant(attempts: u32, step: Duration) -> Self {
        let strategy = (0..attempts).map(|_| step).collect();
        Self(strategy)
    }

    pub fn exponential(attempts: u32, exp: u32, step: Duration) -> Self {
        let strategy = (0..attempts)
            .map(|i| step.mul_f64(exp.pow(i) as f64))
            .collect();

        Self(strategy)
    }

    pub fn inner(&self) -> Vec<Duration> {
        self.0.clone()
    }
}

impl Default for RetryStrategy {
    fn default() -> Self {
        Self::linear(DEFAULT_ATTEMPTS_AMOUNT, DEFAULT_DURATION_STEP)
    }
}

impl From<&[Duration]> for RetryStrategy {
    fn from(value: &[Duration]) -> Self {
        Self(value.to_vec())
    }
}

impl std::cmp::PartialEq<&[Duration]> for RetryStrategy {
    fn eq(&self, other: &&[Duration]) -> bool {
        &self.0 == other
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::prelude::*;
    use std::time::Duration;

    #[test]
    fn generates_linear_strategy() {
        let mut rng = rand::thread_rng();

        let attempts = rng.gen_range(3..5);
        let step = rng.gen_range(1..5);
        let step = Duration::from_secs(step);

        let strategy = RetryStrategy::linear(attempts, step);
        let expected = (1..=attempts).map(|i| i * step).collect::<Vec<_>>();

        assert_eq!(strategy, &expected)
    }

    #[test]
    fn generates_constant_strategy() {
        let mut rng = rand::thread_rng();

        let attempts = rng.gen_range(3..5);
        let step = rng.gen_range(1..5);
        let step = Duration::from_secs(step);

        let strategy = RetryStrategy::constant(attempts, step);
        let expected = (0..attempts).map(|_| step).collect::<Vec<_>>();

        assert_eq!(strategy, &expected)
    }

    #[test]
    fn generates_exponential_strategy() {
        let mut rng = rand::thread_rng();

        let attempts = rng.gen_range(3..5);
        let exp = rng.gen_range(2..5);
        let step = rng.gen_range(1..5);
        let step = Duration::from_secs(step);

        let strategy = RetryStrategy::exponential(attempts, exp, step);
        let expected = (0..attempts)
            .map(|i| step.mul_f64(exp.pow(i) as f64))
            .collect::<Vec<_>>();

        assert_eq!(strategy, &expected)
    }

    #[test]
    fn allows_explicit_strategy() {
        let mut rng = rand::thread_rng();

        let attempts = rng.gen_range(3..10);
        let strategy = (1..=attempts)
            .map(|i| Duration::from_secs(i))
            .collect::<Vec<_>>();

        assert_eq!(RetryStrategy::from(strategy.as_slice()), &strategy);
    }
}
