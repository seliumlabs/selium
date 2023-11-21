use std::time::Duration;

const DEFAULT_MAX_ATTEMPTS: u32 = 5;
const DEFAULT_STEP: Duration = Duration::from_secs(1);

#[derive(Debug, Clone)]
enum Strategy {
    Linear,
    Constant,
    Exponential(u64),
}

impl Default for Strategy {
    fn default() -> Self {
        Self::Linear
    }
}

#[derive(Debug, Clone)]
struct BackoffStrategyState {
    max_duration: Option<Duration>,
    max_attempts: u32,
    step: Duration,
}

impl Default for BackoffStrategyState {
    fn default() -> Self {
        Self {
            max_duration: None,
            max_attempts: DEFAULT_MAX_ATTEMPTS,
            step: DEFAULT_STEP,
        }
    }
}

#[derive(Debug, Clone)]
pub struct BackoffStrategy {
    strategy_type: Strategy,
    state: BackoffStrategyState,
}

impl Default for BackoffStrategy {
    fn default() -> Self {
        Self {
            strategy_type: Strategy::default(),
            state: BackoffStrategyState::default(),
        }
    }
}

impl BackoffStrategy {
    fn new(strategy_type: Strategy) -> Self {
        Self {
            strategy_type,
            ..Self::default()
        }
    }

    pub fn linear() -> Self {
        Self::new(Strategy::Linear)
    }

    pub fn constant() -> Self {
        Self::new(Strategy::Constant)
    }

    pub fn exponential(factor: u64) -> Self {
        Self::new(Strategy::Exponential(factor))
    }

    pub fn with_max_attempts(mut self, attempts: u32) -> Self {
        self.state.max_attempts = attempts;
        self
    }

    pub fn with_max_duration(mut self, max: Duration) -> Self {
        self.state.max_duration = Some(max);
        self
    }

    pub fn with_step(mut self, step: Duration) -> Self {
        self.state.step = step;
        self
    }
}

impl IntoIterator for BackoffStrategy {
    type Item = Duration;
    type IntoIter = BackoffStrategyIter;

    fn into_iter(self) -> Self::IntoIter {
        BackoffStrategyIter {
            strategy_type: self.strategy_type,
            current_attempt: 1,
            state: self.state,
        }
    }
}

pub struct BackoffStrategyIter {
    strategy_type: Strategy,
    state: BackoffStrategyState,
    current_attempt: u32,
}

impl Iterator for BackoffStrategyIter {
    type Item = Duration;

    fn next(&mut self) -> Option<Self::Item> {
        let step = self.state.step;
        let max_duration = self.state.max_duration;
        let max_attempts = self.state.max_attempts;
        let current_attempt = self.current_attempt;

        if current_attempt > max_attempts {
            return None;
        }

        let mut next = match self.strategy_type {
            Strategy::Linear => step * current_attempt,
            Strategy::Constant => step,
            Strategy::Exponential(factor) => step.mul_f64(factor.pow(current_attempt - 1) as f64),
        };

        self.current_attempt += 1;

        if let Some(max) = max_duration {
            next = next.min(max);
        }

        Some(next)
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

        let mut strategy = BackoffStrategy::linear()
            .with_max_attempts(attempts)
            .with_step(step)
            .into_iter();

        let expected = (1..=attempts).map(|i| i * step).collect::<Vec<_>>();

        for value in expected {
            assert_eq!(strategy.next().unwrap(), value);
        }
    }

    #[test]
    fn generates_constant_strategy() {
        let mut rng = rand::thread_rng();

        let attempts = rng.gen_range(3..5);
        let step = rng.gen_range(1..5);
        let step = Duration::from_secs(step);

        let mut strategy = BackoffStrategy::constant()
            .with_max_attempts(attempts)
            .with_step(step)
            .into_iter();

        let expected = (0..attempts).map(|_| step).collect::<Vec<_>>();

        for value in expected {
            assert_eq!(strategy.next().unwrap(), value);
        }
    }

    #[test]
    fn generates_exponential_strategy() {
        let mut rng = rand::thread_rng();

        let attempts = rng.gen_range(3..5);
        let factor = rng.gen_range(2..5);
        let step = rng.gen_range(1..5);
        let step = Duration::from_secs(step);

        let mut strategy = BackoffStrategy::exponential(factor)
            .with_max_attempts(attempts)
            .with_step(step)
            .into_iter();

        let expected = (0..attempts)
            .map(|i| step.mul_f64(factor.pow(i) as f64))
            .collect::<Vec<_>>();

        for value in expected {
            assert_eq!(strategy.next().unwrap(), value);
        }
    }

    #[test]
    fn clamps_generated_durations_to_provided_max() {
        let mut rng = rand::thread_rng();

        let attempts: u32 = rng.gen_range(3..5);
        let step = rng.gen_range(1..5);
        let step = Duration::from_secs(step);

        // We want to clip the last step in the linear backoff strategy
        let max_duration = (attempts - 1) as u64 * step.as_secs();
        let max_duration = Duration::from_secs(max_duration);

        let mut strategy = BackoffStrategy::linear()
            .with_max_attempts(attempts)
            .with_max_duration(max_duration)
            .with_step(step)
            .into_iter();

        // Push the unmodifed values, and then the 'clipped' value.
        let mut expected = Vec::with_capacity(attempts as usize);
        (1..attempts).for_each(|i| expected.push(i * step));
        expected.push(max_duration);

        for value in expected {
            assert_eq!(strategy.next().unwrap(), value);
        }
    }

    #[test]
    fn iterator_is_exhausted_after_max_attempts() {
        let mut rng = rand::thread_rng();

        let attempts = rng.gen_range(3..5);
        let step = rng.gen_range(1..5);
        let step = Duration::from_secs(step);

        let mut strategy = BackoffStrategy::constant()
            .with_max_attempts(attempts)
            .with_step(step)
            .into_iter();

        let expected = (0..attempts).map(|_| step).collect::<Vec<_>>();

        for value in expected {
            assert_eq!(strategy.next().unwrap(), value);
        }

        // We should have fully consumed the iterator in the previous step
        assert_eq!(strategy.next(), None);
    }
}
