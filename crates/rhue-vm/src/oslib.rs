//! The `utf8` library and the side-effect-free part of `os`: `time`, `clock`,
//! `date` and `difftime`. Nothing in here touches files, processes or the
//! environment, so it's safe in a sandbox. Dates are UTC (there's no time zone
//! database), and `os.clock` counts seconds since the VM was made.

use std::rc::Rc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use smallvec::smallvec;

use crate::stdlib::{bad_arg, opt_int, text_arg};
use crate::{NativeCtx, NativeError, Rets, Table, Value, Vm};

type R = Result<Rets, NativeError>;

pub(crate) fn install(vm: &mut Vm) {
    let started = Instant::now();
    let os = lib(
        vm,
        vec![
            ("time", Rc::new(os_time)),
            (
                "clock",
                Rc::new(move |_: &mut NativeCtx<'_>, _: &[Value]| -> R {
                    Ok(smallvec![Value::Number(started.elapsed().as_secs_f64())])
                }),
            ),
            ("date", Rc::new(os_date)),
            ("difftime", Rc::new(os_difftime)),
        ],
    );
    vm.set_global("os", os);
    let utf8 = lib(
        vm,
        vec![
            ("char", Rc::new(utf8_char)),
            ("codepoint", Rc::new(utf8_codepoint)),
            ("len", Rc::new(utf8_len)),
            ("offset", Rc::new(utf8_offset)),
            ("codes", Rc::new(utf8_codes)),
        ],
    );
    vm.set_global("utf8", utf8);
}

fn lib(vm: &mut Vm, entries: Vec<(&str, crate::NativeFn)>) -> Value {
    let mut table = Table::default();
    for (name, func) in entries {
        let key = vm.string(name);
        let f = vm.native(name, func);
        let _ = table.set(key, f);
    }
    Value::Table(vm.heap_mut().alloc_table(table))
}

// ─ os ────────────────────────────────────────────────────────────────────────

fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs() as f64)
}

/// Days since 1970-01-01 for a proleptic Gregorian date (Howard Hinnant's
/// `days_from_civil`).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The inverse: (year, month, day).
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (
        if m <= 2 {
            yoe + era * 400 + 1
        } else {
            yoe + era * 400
        },
        m,
        d,
    )
}

/// `os.time([t])`, now or the UTC time a date table describes (fields can be
/// out of range: `{year = 2026, month = 13, day = 1}` is January 2027).
fn os_time(ctx: &mut NativeCtx<'_>, args: &[Value]) -> R {
    let t = match args.first().copied() {
        None | Some(Value::Nil) => return Ok(smallvec![Value::Number(now())]),
        Some(Value::Table(t)) => t,
        Some(other) => return Err(bad_arg(1, "time", "table", other)),
    };
    let mut field = |name: &str, default: Option<i64>| -> Result<i64, NativeError> {
        let key = ctx.string(name);
        match ctx.heap().table(t).get(key) {
            Value::Number(n) if n.fract() == 0.0 => Ok(n as i64),
            Value::Nil => default.ok_or_else(|| {
                NativeError::Message(format!("field '{name}' missing in date table"))
            }),
            _ => Err(NativeError::Message(format!(
                "field '{name}' is not an integer"
            ))),
        }
    };
    let year = field("year", None)?;
    let month = field("month", None)?;
    let day = field("day", None)?;
    let hour = field("hour", Some(12))?;
    let min = field("min", Some(0))?;
    let sec = field("sec", Some(0))?;
    // normalize the month so `days_from_civil` sees 1..=12
    let (year, month) = (
        year + (month - 1).div_euclid(12),
        (month - 1).rem_euclid(12) + 1,
    );
    let days = days_from_civil(year, month, 1) + day - 1;
    let secs = days * 86_400 + hour * 3600 + min * 60 + sec;
    Ok(smallvec![Value::Number(secs as f64)])
}

struct DateParts {
    year: i64,
    month: i64,
    day: i64,
    hour: i64,
    min: i64,
    sec: i64,
    /// 1 = Sunday
    wday: i64,
    yday: i64,
}

fn date_parts(t: i64) -> DateParts {
    let days = t.div_euclid(86_400);
    let rem = t.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    DateParts {
        year,
        month,
        day,
        hour: rem / 3600,
        min: rem % 3600 / 60,
        sec: rem % 60,
        // 1970-01-01 was a Thursday (5 with Sunday = 1)
        wday: (days + 4).rem_euclid(7) + 1,
        yday: days - days_from_civil(year, 1, 1) + 1,
    }
}

const DAYS: [&str; 7] = [
    "Sunday",
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
];
const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

/// `os.date([format [, time]])`, always UTC (a leading `!` is fine). `*t` gives
/// a table, otherwise `strftime`-style `%` directives: `a A b B c d H I j m M p
/// S w x X y Y %`.
fn os_date(ctx: &mut NativeCtx<'_>, args: &[Value]) -> R {
    let fmt = match args.first() {
        None | Some(Value::Nil) => "%c".to_string(),
        Some(_) => text_arg(ctx, args, 0, "date")?,
    };
    let fmt = fmt.strip_prefix('!').unwrap_or(&fmt);
    let t = match args.get(1) {
        None | Some(Value::Nil) => now() as i64,
        Some(_) => opt_int(args, 1, "date", 0)?,
    };
    let d = date_parts(t);
    if fmt.starts_with("*t") {
        let mut table = Table::with_capacity(0, 9);
        for (name, v) in [
            ("year", d.year),
            ("month", d.month),
            ("day", d.day),
            ("hour", d.hour),
            ("min", d.min),
            ("sec", d.sec),
            ("wday", d.wday),
            ("yday", d.yday),
        ] {
            let k = ctx.string(name);
            let _ = table.set(k, Value::Number(v as f64));
        }
        let k = ctx.string("isdst");
        let _ = table.set(k, Value::Bool(false));
        return Ok(smallvec![ctx.new_table(table)]);
    }
    let mut out = String::new();
    let mut chars = fmt.chars();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        let day_name = DAYS[(d.wday - 1) as usize];
        let month_name = MONTHS[(d.month - 1) as usize];
        let hour12 = if d.hour % 12 == 0 { 12 } else { d.hour % 12 };
        match chars.next() {
            Some('a') => out.push_str(&day_name[..3]),
            Some('A') => out.push_str(day_name),
            Some('b') => out.push_str(&month_name[..3]),
            Some('B') => out.push_str(month_name),
            Some('c') => out.push_str(&format!(
                "{} {} {:2} {:02}:{:02}:{:02} {}",
                &day_name[..3],
                &month_name[..3],
                d.day,
                d.hour,
                d.min,
                d.sec,
                d.year
            )),
            Some('d') => out.push_str(&format!("{:02}", d.day)),
            Some('H') => out.push_str(&format!("{:02}", d.hour)),
            Some('I') => out.push_str(&format!("{hour12:02}")),
            Some('j') => out.push_str(&format!("{:03}", d.yday)),
            Some('m') => out.push_str(&format!("{:02}", d.month)),
            Some('M') => out.push_str(&format!("{:02}", d.min)),
            Some('p') => out.push_str(if d.hour < 12 { "AM" } else { "PM" }),
            Some('S') => out.push_str(&format!("{:02}", d.sec)),
            Some('w') => out.push_str(&(d.wday - 1).to_string()),
            Some('x') => out.push_str(&format!(
                "{:02}/{:02}/{:02}",
                d.month,
                d.day,
                d.year.rem_euclid(100)
            )),
            Some('X') => out.push_str(&format!("{:02}:{:02}:{:02}", d.hour, d.min, d.sec)),
            Some('y') => out.push_str(&format!("{:02}", d.year.rem_euclid(100))),
            Some('Y') => out.push_str(&d.year.to_string()),
            Some('%') => out.push('%'),
            Some(other) => {
                return Err(NativeError::Message(format!(
                    "invalid conversion specifier '%{other}'"
                )));
            }
            None => return Err("invalid conversion specifier '%'".into()),
        }
    }
    Ok(smallvec![ctx.string(&out)])
}

fn os_difftime(_: &mut NativeCtx<'_>, args: &[Value]) -> R {
    let a = crate::stdlib::num(args, 0, "difftime")?;
    let b = match args.get(1) {
        None | Some(Value::Nil) => 0.0,
        Some(_) => crate::stdlib::num(args, 1, "difftime")?,
    };
    Ok(smallvec![Value::Number(a - b)])
}

// ─ utf8 ──────────────────────────────────────────────────────────────────────

fn utf8_char(ctx: &mut NativeCtx<'_>, args: &[Value]) -> R {
    let mut out = String::new();
    for i in 0..args.len() {
        let c = crate::stdlib::int(args, i, "char")?;
        let ch = u32::try_from(c)
            .ok()
            .and_then(char::from_u32)
            .ok_or_else(|| {
                NativeError::Message(format!(
                    "invalid argument #{} to 'char' (value out of range)",
                    i + 1
                ))
            })?;
        out.push(ch);
    }
    Ok(smallvec![ctx.string(&out)])
}

/// 1-based, negative from the end (Lua's `posrelat`), as a 0-based offset that
/// might be one past the end or negative.
fn rel(i: i64, len: usize) -> i64 {
    if i >= 0 { i - 1 } else { len as i64 + i }
}

fn utf8_codepoint(ctx: &mut NativeCtx<'_>, args: &[Value]) -> R {
    let s = text_arg(ctx, args, 0, "codepoint")?;
    let i = opt_int(args, 1, "codepoint", 1)?;
    let i0 = rel(i, s.len());
    let j0 = rel(opt_int(args, 2, "codepoint", i)?, s.len());
    if i0 < 0 {
        return Err("bad argument #2 to 'codepoint' (out of bounds)".into());
    }
    if j0 >= s.len() as i64 {
        return Err("bad argument #3 to 'codepoint' (out of bounds)".into());
    }
    let mut out = Rets::new();
    let mut k = i0 as usize;
    while (k as i64) <= j0 {
        let Some(c) = s.get(k..).and_then(|r| r.chars().next()) else {
            return Err("invalid UTF-8 code".into());
        };
        out.push(Value::Number(c as u32 as f64));
        k += c.len_utf8();
    }
    Ok(out)
}

/// `utf8.len(s [, i [, j]])`, characters starting in bytes `i..=j`, or `nil,
/// position` if `i` is inside a character.
fn utf8_len(ctx: &mut NativeCtx<'_>, args: &[Value]) -> R {
    let s = text_arg(ctx, args, 0, "len")?;
    let i0 = rel(opt_int(args, 1, "len", 1)?, s.len());
    let j0 = rel(opt_int(args, 2, "len", -1)?, s.len());
    if i0 < 0 || i0 > s.len() as i64 {
        return Err("bad argument #2 to 'len' (initial position out of bounds)".into());
    }
    if j0 >= s.len() as i64 {
        return Err("bad argument #3 to 'len' (final position out of bounds)".into());
    }
    let mut n = 0;
    let mut k = i0 as usize;
    while (k as i64) <= j0 {
        match s.get(k..).and_then(|r| r.chars().next()) {
            Some(c) => {
                n += 1;
                k += c.len_utf8();
            }
            None => return Ok(smallvec![Value::Nil, Value::Number((k + 1) as f64)]),
        }
    }
    Ok(smallvec![Value::Number(n as f64)])
}

/// `utf8.offset(s, n [, i])`, the byte position where the `n`-th character
/// (counting from the one at `i`) starts.
fn utf8_offset(ctx: &mut NativeCtx<'_>, args: &[Value]) -> R {
    let s = text_arg(ctx, args, 0, "offset")?;
    let b = s.as_bytes();
    let len = b.len() as i64;
    let mut n = crate::stdlib::int(args, 1, "offset")?;
    let default = if n >= 0 { 1 } else { len + 1 };
    let mut i = rel(opt_int(args, 2, "offset", default)?, b.len());
    if i < 0 || i > len {
        return Err("bad argument #3 to 'offset' (position out of bounds)".into());
    }
    let cont = |i: i64| i < len && (b[i as usize] & 0xC0) == 0x80;
    if n == 0 {
        while i > 0 && cont(i) {
            i -= 1;
        }
        return Ok(smallvec![Value::Number((i + 1) as f64)]);
    }
    if cont(i) {
        return Err("initial position is a continuation byte".into());
    }
    if n < 0 {
        while n < 0 && i > 0 {
            i -= 1;
            while i > 0 && cont(i) {
                i -= 1;
            }
            n += 1;
        }
    } else {
        n -= 1;
        while n > 0 && i < len {
            i += 1;
            while cont(i) {
                i += 1;
            }
            n -= 1;
        }
    }
    Ok(if n == 0 {
        smallvec![Value::Number((i + 1) as f64)]
    } else {
        smallvec![Value::Nil]
    })
}

/// `for pos, code in utf8.codes(s)`.
fn utf8_codes(ctx: &mut NativeCtx<'_>, args: &[Value]) -> R {
    let s = match args.first().copied() {
        Some(v @ Value::String(_)) => v,
        other => return Err(bad_arg(1, "codes", "string", other.unwrap_or(Value::Nil))),
    };
    let iter = ctx.vm().native(
        "codes_iter",
        Rc::new(|ctx: &mut NativeCtx<'_>, args: &[Value]| -> R {
            let s = text_arg(ctx, args, 0, "codes")?;
            let pos = crate::stdlib::int(args, 1, "codes")?;
            // skip the character at `pos` (1-based), then report the next
            let mut k = pos.max(0) as usize;
            if pos > 0 {
                let Some(c) = s.get(k - 1..).and_then(|r| r.chars().next()) else {
                    return Err("invalid UTF-8 code".into());
                };
                k = k - 1 + c.len_utf8();
            }
            match s.get(k..).and_then(|r| r.chars().next()) {
                Some(c) => Ok(smallvec![
                    Value::Number((k + 1) as f64),
                    Value::Number(c as u32 as f64)
                ]),
                None => Ok(smallvec![Value::Nil]),
            }
        }),
    );
    Ok(smallvec![iter, s, Value::Number(0.0)])
}
