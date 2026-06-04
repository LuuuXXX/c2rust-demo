/// Process-global mutex for tests that manipulate environment variables.
///
/// Since env vars are process-wide, any test that sets/removes `C2RUST_COV`
/// or similar variables must hold this lock for the duration of the test to
/// prevent races with other tests running concurrently.
///
/// Usage:
/// ```ignore
/// let _g = crate::test_utils::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
/// ```
pub static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
