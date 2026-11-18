//! Arithmetic semantics, in one place.
//!
//! The constant folder and the VM both call [`arith`], so a folded expression
//! and an executed one give bit-for-bit identical results.

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ArithOp {
    Add,
    Sub,
    Mul,
    Div,
    IDiv,
    Mod,
    Pow,
}

impl ArithOp {
    /// The name Luau uses in "attempt to perform arithmetic (add) on ..." errors.
    pub fn name(self) -> &'static str {
        match self {
            ArithOp::Add => "add",
            ArithOp::Sub => "sub",
            ArithOp::Mul => "mul",
            ArithOp::Div => "div",
            ArithOp::IDiv => "idiv",
            ArithOp::Mod => "mod",
            ArithOp::Pow => "pow",
        }
    }
}

/// Luau number semantics: IEEE-754 doubles throughout.
///
/// - `a // b` is `floor(a / b)`.
/// - `a % b` is `a - floor(a / b) * b`, so the result takes the divisor's sign.
/// - Division by zero gives ±inf or NaN; it is not an error.
#[inline]
pub fn arith(op: ArithOp, a: f64, b: f64) -> f64 {
    match op {
        ArithOp::Add => a + b,
        ArithOp::Sub => a - b,
        ArithOp::Mul => a * b,
        ArithOp::Div => a / b,
        ArithOp::IDiv => floor(a / b),
        ArithOp::Mod => a - floor(a / b) * b,
        ArithOp::Pow => a.powf(b),
    }
}

/// `x.floor()`, without the libm call that baseline x86-64 (no SSE4.1)
/// compiles `f64::floor` to. Exactly equal to `f64::floor` for every input,
/// signed zeros, infinities and NaN included.
#[inline]
pub fn floor(x: f64) -> f64 {
    // At 2^52 and beyond every double is already an integer (and the cast
    // below would saturate), so only smaller magnitudes need work.
    if x.abs() < 4_503_599_627_370_496.0 {
        let t = x as i64 as f64;
        // Truncation rounds toward zero; negative non-integers need one less.
        // The only zero result of a negative input is floor(-0.0) = -0.0,
        // which `copysign` restores (the cast lost the sign).
        (if t > x { t - 1.0 } else { t }).copysign(x)
    } else {
        x
    }
}

/// Formats a number the way Luau's `tostring` does: integral values print
/// without a fraction (`3`), others use the shortest round-trip form (`0.1`),
/// and very large or small magnitudes use an exponent (`1e+20`).
pub fn format_number(n: f64) -> String {
    if n.is_nan() {
        return if n.is_sign_negative() {
            "-nan".into()
        } else {
            "nan".into()
        };
    }
    if n.is_infinite() {
        return if n > 0.0 { "inf".into() } else { "-inf".into() };
    }
    if n == 0.0 {
        return if n.is_sign_negative() {
            "-0".into()
        } else {
            "0".into()
        };
    }
    let abs = n.abs();
    if (1e-5..1e16).contains(&abs) {
        return format!("{n}");
    }
    // `{:e}` gives "1e20" / "1.5e-7"; Luau writes "1e+20" / "1.5e-07".
    let s = format!("{n:e}");
    let (mantissa, exp) = s.split_once('e').expect("`{:e}` always has an exponent");
    let (sign, digits) = match exp.strip_prefix('-') {
        Some(d) => ('-', d),
        None => ('+', exp),
    };
    format!("{mantissa}e{sign}{digits:0>2}")
}

#[cfg(test)]
mod tests {

    #[test]
    fn floor_matches_f64_floor_bit_for_bit() {
        let mut xs = vec![
            0.0,
            -0.0,
            0.5,
            -0.5,
            1.0,
            -1.0,
            1.5,
            -1.5,
            2.0f64.powi(52),
            -(2.0f64.powi(52)),
            2.0f64.powi(52) - 0.5,
            -(2.0f64.powi(52)) + 0.5,
            1e300,
            -1e300,
            f64::MIN_POSITIVE,
            -f64::MIN_POSITIVE,
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::NAN,
            f64::MAX,
            f64::MIN,
            9.007199254740993e15,
            -9.007199254740993e15,
            0.9999999999999999,
            -0.9999999999999999,
        ];
        // A deterministic spread over many magnitudes.
        let mut seed = 0x9e37_79b9_7f4a_7c15u64;
        for _ in 0..200_000 {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            xs.push(f64::from_bits(seed));
            xs.push((seed % 2_000_001) as f64 / 1000.0 - 1000.0);
        }
        for x in xs {
            let (want, got) = (x.floor(), floor(x));
            assert!(
                want.to_bits() == got.to_bits() || (want.is_nan() && got.is_nan()),
                "floor({x:e}) = {got:e}, want {want:e}"
            );
        }
    }
    use super::*;
    use ArithOp::*;

    #[test]
    fn floor_division_and_modulo_follow_luau() {
        assert_eq!(arith(IDiv, 7.0, 2.0), 3.0);
        assert_eq!(arith(IDiv, -7.0, 2.0), -4.0);
        assert_eq!(arith(Mod, 7.0, 3.0), 1.0);
        assert_eq!(arith(Mod, -7.0, 3.0), 2.0);
        assert_eq!(arith(Mod, 7.0, -3.0), -2.0);
        assert_eq!(arith(Mod, 5.5, 2.0), 1.5);
    }

    #[test]
    fn division_by_zero_is_not_an_error() {
        assert_eq!(arith(Div, 1.0, 0.0), f64::INFINITY);
        assert_eq!(arith(Div, -1.0, 0.0), f64::NEG_INFINITY);
        assert!(arith(Div, 0.0, 0.0).is_nan());
        assert!(arith(Mod, 1.0, 0.0).is_nan());
    }

    #[test]
    fn pow() {
        assert_eq!(arith(Pow, 2.0, 10.0), 1024.0);
        assert_eq!(arith(Pow, 2.0, -1.0), 0.5);
    }

    #[test]
    fn number_formatting() {
        let cases = [
            (3.0, "3"),
            (-3.0, "-3"),
            (0.1, "0.1"),
            (3.5, "3.5"),
            (0.0, "0"),
            (-0.0, "-0"),
            (1e15, "1000000000000000"),
            (1e16, "1e+16"),
            (1e20, "1e+20"),
            (1.5e-7, "1.5e-07"),
            (0.00001, "0.00001"),
            (123456.789, "123456.789"),
            (f64::INFINITY, "inf"),
            (f64::NEG_INFINITY, "-inf"),
            (1e300, "1e+300"),
            (2f64.powi(53), "9007199254740992"),
        ];
        for (n, want) in cases {
            assert_eq!(format_number(n), want, "formatting {n:?}");
        }
        assert_eq!(format_number(f64::NAN), "nan");
    }
}
