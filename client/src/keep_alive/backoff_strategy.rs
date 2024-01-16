use std::time::Duration;

/// Default max retry attempts.
pub const DEFAULT_MAX_ATTEMPTS: u32 = 5;
/// Default attempt duration step.
pub const DEFAULT_STEP: Duration = Duration::from_secs(1);

#[derive(Debug, PartialEq)]
pub struct NextAttempt {
    pub duration: Duration,
    pub attempt_num: u32,
    pub max_attempts: u32,
}

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

/// Configuration type used to override default backoff strategy.
///
/// The `BackoffStrategy` type includes high-level associated functions to construct a configuration
/// using a preset strategy.
///
/// ```
/// use selium::keep_alive::BackoffStrategy;
///
/// let linear = BackoffStrategy::linear();
/// let constant = BackoffStrategy::constant();
/// let exponential = BackoffStrategy::exponential(2);
/// ```
///
/// The preset strategy can then be tweaked further by overriding the default settings provided for
/// the max attempts, max retry duration, and duration step.
///
/// ```
/// use selium::keep_alive::BackoffStrategy;
/// use std::time::Duration;
///
/// let tweaked_linear = BackoffStrategy::linear()
///     .with_max_attempts(5)
///     .with_max_duration(Duration::from_secs(3))
///     .with_step(Duration::from_secs(1));
/// ```
#[derive(Default, Debug, Clone)]
pub struct BackoffStrategy {
    strategy_type: Strategy,
    state: BackoffStrategyState,
}

impl BackoffStrategy {
    fn new(strategy_type: Strategy) -> Self {
        Self {
            strategy_type,
            ..Self::default()
        }
    }

    /// Constructs a new `RetryStrategy` instance with a `linear` preset.
    ///
    /// A linear retry strategy will increase each successive attempt linearly by the provided
    /// (or default if not provided) `step`.
    ///
    /// # Examples
    ///
    /// The following strategy will produce a sequence of [Duration] values equivalent
    /// to `2s, 4s, 6s, 8s, 10s`.
    ///
    /// ```
    /// # use selium::keep_alive::BackoffStrategy;
    /// # use std::time::Duration;
    /// #
    /// BackoffStrategy::linear()
    ///     .with_max_attempts(5)
    ///     .with_step(Duration::from_secs(2));
    /// ```
    pub fn linear() -> Self {
        Self::new(Strategy::Linear)
    }

    /// Constructs a new `RetryStrategy` instance with a `constant` preset.
    ///
    /// A constant retry strategy will not increase the duration of each successive retry.
    ///
    /// # Examples
    ///
    /// The following strategy will produce a sequence of [Duration] values equivalent
    /// to `2s, 2s, 2s, 2s, 2s`.
    ///
    /// ```
    /// # use selium::keep_alive::BackoffStrategy;
    /// # use std::time::Duration;
    /// #
    /// BackoffStrategy::constant()
    ///     .with_max_attempts(5)
    ///     .with_step(Duration::from_secs(2));
    /// ```
    pub fn constant() -> Self {
        Self::new(Strategy::Constant)
    }

    /// Constructs a new `RetryStrategy` instance with an `exponential` preset.
    ///
    /// An exponential retry strategy will increase each successive attempt exponentially using
    /// the provided (or default if not provided) `step` and exponential `factor`.
    ///
    /// # Examples
    ///
    /// The following strategy will produce a sequence of [Duration] values equivalent
    /// to `2s, 4s, 8s, 16s, 32s`.
    ///
    /// ```
    /// # use selium::keep_alive::BackoffStrategy;
    /// # use std::time::Duration;
    /// #
    /// BackoffStrategy::exponential(2)
    ///     .with_max_attempts(5)
    ///     .with_step(Duration::from_secs(2));
    /// ```
    pub fn exponential(factor: u64) -> Self {
        Self::new(Strategy::Exponential(factor))
    }

    /// Overrides the [default max attempts](DEFAULT_MAX_ATTEMPTS) value to specify the amount of
    /// [Duration] values that will be produced by the iterator.
    ///
    /// # Examples
    ///
    /// The following will produce a sequence equivalent to `2s, 4s, 6s`
    ///
    /// ```
    /// # use selium::keep_alive::BackoffStrategy;
    /// # use std::time::Duration;
    /// #
    /// BackoffStrategy::linear()
    ///     .with_max_attempts(3)
    ///     .with_step(Duration::from_secs(2));
    /// ```
    ///
    /// whereas the following will produce a sequence equivalent to `2s, 4s, 6s, 8s, 10s`
    ///
    /// ```
    /// # use selium::keep_alive::BackoffStrategy;
    /// # use std::time::Duration;
    /// #
    /// BackoffStrategy::linear()
    ///     .with_max_attempts(5) // Increased max attempts!
    ///     .with_step(Duration::from_secs(2));
    /// ```
    pub fn with_max_attempts(mut self, attempts: u32) -> Self {
        self.state.max_attempts = attempts;
        self
    }

    /// Specifies a max duration to clamp produced [Duration]s to a maximum value. Can be useful
    /// when creating an `exponential` strategy with a high amount of retries to keep the [Duration]
    /// values within reasonable boundaries.
    ///
    /// # Examples
    ///
    /// The following, uncapped `exponential` strategy will produce a sequence equivalent to
    /// `2s, 4s, 8s, 16s, 32s, 64s`, which can be quite unwieldy.
    ///
    /// ```
    /// # use selium::keep_alive::BackoffStrategy;
    /// # use std::time::Duration;
    /// #
    /// BackoffStrategy::exponential(2)
    ///     .with_max_attempts(6)
    ///     .with_step(Duration::from_secs(2));
    /// ```
    ///
    /// whereas the following, capped `exponential` strategy would produce a sequence equivalent to
    /// `2s, 4s, 8s, 8s, 8s, 8s`
    ///
    /// ```
    /// # use selium::keep_alive::BackoffStrategy;
    /// # use std::time::Duration;
    /// #
    /// BackoffStrategy::exponential(2)
    ///     .with_max_attempts(6)
    ///     .with_max_duration(Duration::from_secs(8)) // Capped to 8 seconds maximum!
    ///     .with_step(Duration::from_secs(2));
    /// ```
    pub fn with_max_duration(mut self, max: Duration) -> Self {
        self.state.max_duration = Some(max);
        self
    }

    /// Overrides the [default step](DEFAULT_STEP) to use for each strategy. Depending on the
    /// strategy, this will have different results.
    ///
    /// - For `linear` strategies, the `step` is used to increase the duration for each successive attempt.
    /// - For `constant` strategies, the `step` is used as a constant duration for every attempt.
    /// - For `exponential` strategies, each attempt is increased exponentially using the product of the
    /// `step` and an exponential `factor`
    ///
    /// # Examples
    ///
    /// The following linear strategy will produce a sequence equivalent to
    /// `2s, 4s, 6s, 8s, 10s`
    ///
    /// ```
    /// # use selium::keep_alive::BackoffStrategy;
    /// # use std::time::Duration;
    /// #
    /// BackoffStrategy::linear()
    ///     .with_max_attempts(5) // Increased max attempts!
    ///     .with_step(Duration::from_secs(2));
    /// ```
    ///
    /// whereas the following strategy will produce a sequence equivalent to `4s, 8s, 12s, 16s, 20s`
    ///
    /// ```
    /// # use selium::keep_alive::BackoffStrategy;
    /// # use std::time::Duration;
    /// #
    /// BackoffStrategy::linear()
    ///     .with_max_attempts(5)
    ///     .with_step(Duration::from_secs(4)); // Increased step!
    /// ```
    pub fn with_step(mut self, step: Duration) -> Self {
        self.state.step = step;
        self
    }
}

/// Consumes the `BackoffStrategy` instance to create an iterator that produces [Duration] values
/// representing a sequence of retry attempts.
impl IntoIterator for BackoffStrategy {
    type Item = NextAttempt;
    type IntoIter = BackoffStrategyIter;

    fn into_iter(self) -> Self::IntoIter {
        BackoffStrategyIter {
            strategy_type: self.strategy_type,
            current_attempt: 1,
            state: self.state,
        }
    }
}

#[doc(hidden)]
pub struct BackoffStrategyIter {
    strategy_type: Strategy,
    state: BackoffStrategyState,
    current_attempt: u32,
}

impl Iterator for BackoffStrategyIter {
    type Item = NextAttempt;

    fn next(&mut self) -> Option<Self::Item> {
        let step = self.state.step;
        let max_duration = self.state.max_duration;
        let max_attempts = self.state.max_attempts;
        let current_attempt = self.current_attempt;

        if current_attempt > max_attempts {
            return None;
        }

        let mut next_duration = match self.strategy_type {
            Strategy::Linear => step * current_attempt,
            Strategy::Constant => step,
            Strategy::Exponential(factor) => step.mul_f64(factor.pow(current_attempt - 1) as f64),
        };

        self.current_attempt += 1;

        if let Some(max) = max_duration {
            next_duration = next_duration.min(max);
        }

        let next = NextAttempt {
            duration: next_duration,
            attempt_num: current_attempt,
            max_attempts,
        };

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
            assert_eq!(strategy.next().unwrap().duration, value);
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
            assert_eq!(strategy.next().unwrap().duration, value);
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
            assert_eq!(strategy.next().unwrap().duration, value);
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
            assert_eq!(strategy.next().unwrap().duration, value);
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
            assert_eq!(strategy.next().unwrap().duration, value);
        }

        // We should have fully consumed the iterator in the previous step
        assert_eq!(strategy.next(), None);
    }
}
