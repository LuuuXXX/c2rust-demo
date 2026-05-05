use anyhow::anyhow;
use std::fmt;

pub type Result<T> = anyhow::Result<T>;

/// Convenience trait for mapping errors with context
pub trait ToError<T> {
    fn ctx(self, info: &str) -> Result<T>;
}

impl<T, E: fmt::Display> ToError<T> for core::result::Result<T, E> {
    fn ctx(self, info: &str) -> Result<T> {
        self.map_err(|e| anyhow!("{}: {}", info, e))
    }
}
