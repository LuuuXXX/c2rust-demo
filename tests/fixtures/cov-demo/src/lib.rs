//! FFI bindings to the C library compiled with LLVM coverage instrumentation.
//!
//! Running `cargo llvm-cov` here produces a coverage report that includes
//! both this Rust code and the underlying C source files (math.c, counter.c),
//! demonstrating that `c2rust-demo`'s coverage feature works end-to-end.

use std::os::raw::c_int;

extern "C" {
    pub fn add(a: c_int, b: c_int) -> c_int;
    pub fn compute(n: c_int) -> c_int;
    pub fn increment();
    pub fn get_counter() -> c_int;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_add() {
        assert_eq!(unsafe { add(2, 3) }, 5);
        assert_eq!(unsafe { add(0, 0) }, 0);
        assert_eq!(unsafe { add(-1, 1) }, 0);
    }

    #[test]
    fn test_compute() {
        // compute(n) = helper(n) + add(n, 1) = n*2 + (n+1) = 3n + 1
        assert_eq!(unsafe { compute(0) }, 1);
        assert_eq!(unsafe { compute(3) }, 10);
    }

    #[test]
    fn test_counter() {
        // Use a relative check so test ordering does not matter.
        unsafe {
            let before = get_counter();
            increment();
            assert_eq!(get_counter(), before + 1);
        }
    }
}
