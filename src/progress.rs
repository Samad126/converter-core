use std::sync::Arc;

/// Progress of a long job, 0.0..=1.0 when known.
#[derive(Debug, Clone, PartialEq)]
pub enum Progress {
    Started,
    Fraction(f32),
    Finished,
}

pub type ProgressFn = Arc<dyn Fn(Progress) + Send + Sync>;

pub fn noop() -> ProgressFn { Arc::new(|_| {}) }
