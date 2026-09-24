//! The floating-point operations `core` does not have.
//!
//! `f64`'s `abs`, `floor`, `ceil`, `round`, `trunc` and `fract` live in `std`,
//! which a kernel does not have, and the compiler will not synthesise them —
//! they are libm functions in everything but name.  Rather than link a
//! mathematics library for six operations, they are written here, where the
//! only requirement is that they agree with the language about the cases that
//! matter: the sign of a negative fraction, and rounding halves upward rather
//! than away from zero.
//!
//! The guard on magnitude is not decoration.  `x as i64` saturates for a value
//! outside the range of `i64`, so `trunc(1e300)` would come back as 9.2e18 —
//! a finite answer to a question whose answer is the input.  Above the largest
//! exactly-representable integer there is nothing to truncate anyway.

/// The point past which every `f64` is already an integer.
const INT_LIMIT: f64 = 9.007199254740992e15;

pub fn abs(x: f64) -> f64 {
    if x < 0.0 {
        -x
    } else {
        x
    }
}

/// Toward negative infinity.  `-0.5` floor is `-1`, not `0`.
pub fn floor(x: f64) -> f64 {
    if !x.is_finite() || abs(x) >= INT_LIMIT {
        return x;
    }
    let t = (x as i64) as f64;
    if x < 0.0 && t != x {
        t - 1.0
    } else {
        t
    }
}

/// Toward positive infinity.
pub fn ceil(x: f64) -> f64 {
    if !x.is_finite() || abs(x) >= INT_LIMIT {
        return x;
    }
    let t = (x as i64) as f64;
    if x > 0.0 && t != x {
        t + 1.0
    } else {
        t
    }
}

/// Toward zero.
pub fn trunc(x: f64) -> f64 {
    if !x.is_finite() || abs(x) >= INT_LIMIT {
        return x;
    }
    (x as i64) as f64
}

/// To the nearest integer, with a half going up.
///
/// `Math.round(-0.5)` is `-0`, not `-1`: the language rounds halves toward
/// positive infinity, which is why this is floor(x + 0.5) and not the
/// rounding-away-from-zero that a machine instruction would give.
pub fn round(x: f64) -> f64 {
    if !x.is_finite() || abs(x) >= INT_LIMIT {
        return x;
    }
    floor(x + 0.5)
}

/// The part after the decimal point, with the sign of the input.
pub fn fract(x: f64) -> f64 {
    if !x.is_finite() {
        return if x.is_nan() { x } else { 0.0 };
    }
    x - trunc(x)
}
