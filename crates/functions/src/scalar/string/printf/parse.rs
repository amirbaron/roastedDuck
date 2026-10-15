//! The format string interpreters of DuckDB's `{fmt}` fork: `vsprintf`
//! (`basic_printf_context::format` in `printf.h`) and `vformat` (`parse_format_string`
//! with `format_handler` in `format.h`).

use rustdb_common::Result;

use super::writer::{
    Align, Sign, Specs, format_bool, format_char, format_double, format_error, format_int, format_string,
};

/// One formatting argument, as `{fmt}` stores it (`basic_format_arg`).
#[derive(Clone, Copy, Debug)]
pub(crate) enum Arg<'a> {
    Bool(bool),
    /// `int` (after a printf length conversion).
    Int(i32),
    /// `unsigned`.
    UInt(u32),
    /// `long long`.
    LongLong(i64),
    /// `unsigned long long`.
    ULongLong(u64),
    Int128(i128),
    UInt128(u128),
    /// `char` (after `%c`).
    Char(u8),
    Double(f64),
    Str(&'a [u8]),
}

impl Arg<'_> {
    fn is_integral(self) -> bool {
        !matches!(self, Self::Double(_) | Self::Str(_))
    }

    fn is_arithmetic(self) -> bool {
        !matches!(self, Self::Str(_))
    }

    /// The value of an integral argument as a sign and magnitude.
    fn integer(self) -> Option<(bool, u128)> {
        Some(match self {
            Self::Bool(value) => (false, u128::from(value)),
            Self::Int(value) => (value < 0, u128::from(value.unsigned_abs())),
            Self::UInt(value) => (false, u128::from(value)),
            Self::LongLong(value) => (value < 0, u128::from(value.unsigned_abs())),
            Self::ULongLong(value) => (false, u128::from(value)),
            Self::Int128(value) => (value < 0, value.unsigned_abs()),
            Self::UInt128(value) => (false, value),
            Self::Char(value) => (false, u128::from(value)),
            Self::Double(_) | Self::Str(_) => return None,
        })
    }
}

/// Formats one argument with optional specifiers (`arg_formatter_base`).
fn format_arg(out: &mut Vec<u8>, arg: Arg<'_>, specs: Option<&Specs>) -> Result<()> {
    match arg {
        Arg::Bool(value) => format_bool(out, value, specs),
        Arg::Char(value) => match specs {
            Some(specs) if specs.ty != 0 && specs.ty != b'c' => {
                format_int(out, false, u128::from(value), Some(specs))
            }
            Some(specs) => {
                if specs.align == Align::Numeric || specs.sign != Sign::None || specs.alt {
                    return Err(format_error("invalid format specifier for char"));
                }
                format_char(out, value, specs);
                Ok(())
            }
            None => {
                out.push(value);
                Ok(())
            }
        },
        Arg::Double(value) => format_double(out, value, specs),
        Arg::Str(value) => format_string(out, value, specs),
        integral => {
            let (negative, magnitude) = integral.integer().unwrap_or_default();
            format_int(out, negative, magnitude, specs)
        }
    }
}

/// The arguments with `{fmt}`'s automatic/manual indexing state.
struct Args<'a, 'b> {
    args: &'b [Arg<'a>],
    /// The next automatic index, or `-1` once manual indexing is used.
    next: i64,
}

impl<'a> Args<'a, '_> {
    /// `basic_format_parse_context::next_arg_id`.
    fn next_id(&mut self) -> Result<usize> {
        if self.next < 0 {
            return Err(format_error(
                "cannot switch from manual to automatic argument indexing",
            ));
        }
        let id = self.next as usize;
        self.next += 1;
        Ok(id)
    }

    /// `basic_format_parse_context::check_arg_id`.
    fn check_id(&mut self) -> Result<()> {
        if self.next > 0 {
            return Err(format_error(
                "cannot switch from automatic to manual argument indexing",
            ));
        }
        self.next = -1;
        Ok(())
    }

    /// `internal::get_arg`.
    fn get(&self, id: usize) -> Result<Arg<'a>> {
        self.args
            .get(id)
            .copied()
            .ok_or_else(|| format_error(format!("Argument index \"{id}\" out of range")))
    }
}

/// `parse_nonnegative_int`: `input[*pos]` is a digit.
fn parse_nonnegative_int(input: &[u8], pos: &mut usize) -> Result<usize> {
    const MAX: u64 = i32::MAX as u64;
    if input[*pos] == b'0' {
        *pos += 1;
        return Ok(0);
    }
    let mut value: u64 = 0;
    while let Some(&c) = input.get(*pos).filter(|c| c.is_ascii_digit()) {
        if value > MAX / 10 {
            value = MAX + 1;
            break;
        }
        value = value * 10 + u64::from(c - b'0');
        *pos += 1;
    }
    if value > MAX {
        return Err(format_error("number is too big"));
    }
    Ok(value as usize)
}

// ------------------------------------------------------------------------------------
// printf
// ------------------------------------------------------------------------------------

/// `printf_width_handler`: a `*` width argument (negative means left-aligned).
fn printf_width(arg: Arg<'_>, specs: &mut Specs) -> Result<usize> {
    let Some((negative, magnitude)) = arg.integer().filter(|_| !matches!(arg, Arg::Bool(_))) else {
        return match arg {
            Arg::Bool(value) => Ok(usize::from(value)),
            _ => Err(format_error("width is not integer")),
        };
    };
    if negative {
        specs.align = Align::Left;
    }
    if magnitude > i32::MAX as u128 {
        return Err(format_error("number is too big"));
    }
    Ok(magnitude as usize)
}

/// `printf_precision_handler`: a `*` precision argument (negative means none).
fn printf_precision(arg: Arg<'_>) -> Result<i32> {
    let Some((negative, magnitude)) = arg.integer() else {
        return Err(format_error("precision is not integer"));
    };
    if negative {
        let fits = magnitude <= 1u128 << 31;
        return if fits {
            Ok(0)
        } else {
            Err(format_error("number is too big"))
        };
    }
    if magnitude > i32::MAX as u128 {
        return Err(format_error("number is too big"));
    }
    Ok(magnitude as i32)
}

/// Integer widths of the printf length modifiers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Length {
    /// No modifier: the argument keeps its width.
    Native,
    /// `hh` (`signed char`).
    Char,
    /// `h` (`short`).
    Short,
    /// `l`, `ll`, `j`, `z`, `t` (64-bit).
    Long,
}

/// `arg_converter`: converts an integral argument for the conversion `ty`.
#[allow(clippy::cast_possible_truncation, reason = "C integer conversions")]
fn convert_arg(arg: Arg<'_>, length: Length, ty: u8) -> Arg<'_> {
    let is_unsigned = matches!(arg, Arg::UInt(_) | Arg::ULongLong(_) | Arg::UInt128(_));
    let ty = if matches!(ty, b'd' | b'i') && is_unsigned {
        b'u'
    } else {
        ty
    };
    if let Arg::Bool(_) = arg
        && ty == b's'
    {
        return arg;
    }
    let Some((negative, magnitude)) = arg.integer() else {
        return arg;
    };
    // The argument's two's complement bits.
    let bits = if negative {
        magnitude.wrapping_neg()
    } else {
        magnitude
    };
    let signed = matches!(ty, b'd' | b'i');
    let native_bytes = match arg {
        Arg::Bool(_) | Arg::Char(_) => 1,
        Arg::Int(_) | Arg::UInt(_) => 4,
        Arg::LongLong(_) | Arg::ULongLong(_) => 8,
        _ => 16,
    };
    let target_bytes = match length {
        Length::Native => native_bytes,
        Length::Char => 1,
        Length::Short => 2,
        Length::Long => 8,
    };
    if target_bytes <= 4 {
        let truncated = match target_bytes {
            1 => bits as u8 as u32,
            2 => bits as u16 as u32,
            _ => bits as u32,
        };
        return if signed {
            let value = match target_bytes {
                1 => i32::from(truncated as u8 as i8),
                2 => i32::from(truncated as u16 as i16),
                _ => truncated as i32,
            };
            Arg::Int(value)
        } else {
            Arg::UInt(truncated)
        };
    }
    if signed {
        if target_bytes > 8 {
            match arg {
                Arg::UInt128(value) => Arg::Int128(value as i128),
                _ => Arg::Int128(bits as i128),
            }
        } else {
            Arg::LongLong(bits as u64 as i64)
        }
    } else {
        match native_bytes {
            16 => Arg::UInt128(bits),
            8 => Arg::ULongLong(bits as u64),
            4 => Arg::UInt(bits as u32),
            _ => Arg::Bool(bits != 0),
        }
    }
}

/// `basic_printf_context::parse_flags`.
fn parse_flags(input: &[u8], pos: &mut usize, specs: &mut Specs) {
    while let Some(&c) = input.get(*pos) {
        match c {
            b'-' => specs.align = Align::Left,
            b'+' => specs.sign = Sign::Plus,
            b'0' => specs.fill = b'0',
            b' ' => specs.sign = Sign::Space,
            b'#' => specs.alt = true,
            b',' | b'\'' | b'_' => specs.thousands = c,
            _ => return,
        }
        *pos += 1;
    }
}

/// `duckdb_fmt::vsprintf`.
pub(crate) fn sprintf(format: &[u8], args: &[Arg<'_>]) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(format.len());
    let mut state = Args { args, next: 0 };
    let mut start = 0;
    let mut pos = 0;
    while pos < format.len() {
        let c = format[pos];
        pos += 1;
        if c != b'%' {
            continue;
        }
        if format.get(pos) == Some(&b'%') {
            out.extend_from_slice(&format[start..pos]);
            pos += 1;
            start = pos;
            continue;
        }
        out.extend_from_slice(&format[start..pos - 1]);
        let mut specs = Specs {
            align: Align::Right,
            ..Specs::default()
        };

        // Argument index, flags and width (`parse_header`).
        let mut arg_index: Option<usize> = None;
        let mut width_done = false;
        if let Some(&first) = format.get(pos).filter(|c| c.is_ascii_digit()) {
            let value = parse_nonnegative_int(format, &mut pos)?;
            if format.get(pos) == Some(&b'$') {
                pos += 1;
                arg_index = Some(value);
            } else {
                if first == b'0' {
                    specs.fill = b'0';
                }
                if value != 0 {
                    specs.width = value;
                    width_done = true;
                }
            }
        }
        if !width_done {
            parse_flags(format, &mut pos, &mut specs);
            match format.get(pos) {
                Some(c) if c.is_ascii_digit() => specs.width = parse_nonnegative_int(format, &mut pos)?,
                Some(b'*') => {
                    pos += 1;
                    let id = state.next_id()?;
                    let arg = state.get(id)?;
                    specs.width = printf_width(arg, &mut specs)?;
                }
                _ => {}
            }
        }
        if arg_index == Some(0) {
            return Err(format_error("argument index out of range"));
        }

        // Precision.
        let mut empty_precision = false;
        if format.get(pos) == Some(&b'.') {
            pos += 1;
            match format.get(pos) {
                Some(c) if c.is_ascii_digit() => {
                    specs.precision =
                        i32::try_from(parse_nonnegative_int(format, &mut pos)?).unwrap_or(i32::MAX);
                }
                Some(b'*') => {
                    pos += 1;
                    let id = state.next_id()?;
                    specs.precision = printf_precision(state.get(id)?)?;
                }
                _ => {
                    specs.precision = 0;
                    empty_precision = true;
                }
            }
        }

        let mut arg = match arg_index {
            None => {
                let id = state.next_id()?;
                state.get(id)?
            }
            Some(index) => {
                state.check_id()?;
                state.get(index - 1)?
            }
        };
        if specs.alt && arg.integer().is_some_and(|(_, magnitude)| magnitude == 0) {
            specs.alt = false;
        }
        if specs.fill == b'0' {
            if arg.is_arithmetic() {
                specs.align = Align::Numeric;
            } else {
                specs.fill = b' ';
            }
        }

        // Length modifier and conversion.
        let c = format.get(pos).copied().unwrap_or(0);
        if pos < format.len() {
            pos += 1;
        }
        let mut t = format.get(pos).copied().unwrap_or(0);
        let length = match c {
            b'h' if t == b'h' => {
                pos += 1;
                t = format.get(pos).copied().unwrap_or(0);
                Some(Length::Char)
            }
            b'h' => Some(Length::Short),
            b'l' if t == b'l' => {
                pos += 1;
                t = format.get(pos).copied().unwrap_or(0);
                Some(Length::Long)
            }
            b'l' | b'j' | b'z' | b't' => Some(Length::Long),
            b'L' => None,
            _ => {
                pos -= usize::from(c != 0);
                t = c;
                Some(Length::Native)
            }
        };
        if let Some(length) = length {
            arg = convert_arg(arg, length, t);
        }

        // Type.
        let Some(&ty) = format.get(pos) else {
            return Err(format_error("invalid format string"));
        };
        pos += 1;
        specs.ty = ty;
        if arg.is_integral() {
            match specs.ty {
                b'i' | b'u' => specs.ty = b'd',
                b'c' => {
                    if let Some((negative, magnitude)) = arg.integer() {
                        let bits = if negative {
                            magnitude.wrapping_neg()
                        } else {
                            magnitude
                        };
                        #[allow(clippy::cast_possible_truncation, reason = "a char argument keeps one byte")]
                        let byte = bits as u32 as u8;
                        arg = Arg::Char(byte);
                    }
                }
                _ => {}
            }
        }
        if specs.ty == b'd' && empty_precision {
            specs.thousands = b'.';
        }
        start = pos;
        printf_arg(&mut out, arg, &mut specs)?;
    }
    out.extend_from_slice(&format[start..pos]);
    Ok(out)
}

/// `printf_arg_formatter`.
fn printf_arg(out: &mut Vec<u8>, arg: Arg<'_>, specs: &mut Specs) -> Result<()> {
    match arg {
        Arg::Bool(value) => {
            if specs.ty != b's' {
                return format_arg(out, Arg::Int(i32::from(value)), Some(specs));
            }
            specs.ty = 0;
            format_bool(out, value, Some(specs))
        }
        Arg::Char(value) => {
            if specs.ty != 0 && specs.ty != b'c' {
                return format_arg(out, Arg::Int(i32::from(value)), Some(specs));
            }
            specs.sign = Sign::None;
            specs.alt = false;
            specs.align = Align::Right;
            format_arg(out, arg, Some(specs))
        }
        _ => format_arg(out, arg, Some(specs)),
    }
}

// ------------------------------------------------------------------------------------
// format
// ------------------------------------------------------------------------------------

fn is_name_start(c: u8) -> bool {
    c.is_ascii_alphabetic() || c == b'_'
}

/// An argument reference inside braces.
enum ArgId<'f> {
    Auto,
    Index(usize),
    Name(&'f [u8]),
}

/// `parse_arg_id`: `input[*pos]` is not the end.
fn parse_arg_id<'f>(input: &'f [u8], pos: &mut usize) -> Result<ArgId<'f>> {
    let c = input[*pos];
    if c == b'}' || c == b':' {
        return Ok(ArgId::Auto);
    }
    if c.is_ascii_digit() {
        let index = parse_nonnegative_int(input, pos)?;
        return match input.get(*pos) {
            Some(b'}' | b':') => Ok(ArgId::Index(index)),
            _ => Err(format_error("invalid format string")),
        };
    }
    if !is_name_start(c) {
        return Err(format_error("invalid format string"));
    }
    let begin = *pos;
    *pos += 1;
    while input
        .get(*pos)
        .is_some_and(|&c| is_name_start(c) || c.is_ascii_digit())
    {
        *pos += 1;
    }
    Ok(ArgId::Name(&input[begin..*pos]))
}

/// Resolves an argument reference (`format_handler::on_arg_id`).
fn resolve<'a>(state: &mut Args<'a, '_>, id: &ArgId<'_>) -> Result<Arg<'a>> {
    match id {
        ArgId::Auto => {
            let id = state.next_id()?;
            state.get(id)
        }
        &ArgId::Index(index) => {
            state.check_id()?;
            state.get(index)
        }
        ArgId::Name(name) => {
            let name = String::from_utf8_lossy(name);
            Err(format_error(format!(
                "Argument with name \"{name}\" not found, did you mean to use it as a format specifier (e.g. {{:{name}}}"
            )))
        }
    }
}

/// `width_checker` / `precision_checker` with `get_dynamic_spec`.
fn dynamic_spec(arg: Arg<'_>, what: &str) -> Result<usize> {
    let value = match arg {
        Arg::Bool(_) | Arg::Char(_) | Arg::Double(_) | Arg::Str(_) => {
            return Err(format_error(format!("{what} is not integer")));
        }
        integral => integral.integer().unwrap_or_default(),
    };
    if value.0 {
        return Err(format_error(format!("negative {what}")));
    }
    if value.1 > i32::MAX as u128 {
        return Err(format_error("number is too big"));
    }
    Ok(value.1 as usize)
}

/// `numeric_specs_checker` checks against the argument being formatted.
fn require_numeric(arg: Arg<'_>) -> Result<()> {
    if arg.is_arithmetic() {
        Ok(())
    } else {
        Err(format_error("format specifier requires numeric argument"))
    }
}

fn check_sign(arg: Arg<'_>) -> Result<()> {
    require_numeric(arg)?;
    if arg.is_integral() && !matches!(arg, Arg::Int(_) | Arg::LongLong(_) | Arg::Char(_)) {
        return Err(format_error("format specifier requires signed argument"));
    }
    Ok(())
}

/// A dynamic width/precision reference `{...}` at `input[*pos]` (after the `{`).
fn parse_dynamic(input: &[u8], pos: &mut usize, state: &mut Args<'_, '_>, what: &str) -> Result<usize> {
    let id = if *pos < input.len() {
        parse_arg_id(input, pos)?
    } else {
        ArgId::Auto
    };
    if input.get(*pos) != Some(&b'}') {
        return Err(format_error("invalid format string"));
    }
    *pos += 1;
    dynamic_spec(resolve(state, &id)?, what)
}

/// `parse_format_specs` with `specs_checker<specs_handler>`; returns at the closing brace.
fn parse_format_specs(
    input: &[u8],
    pos: &mut usize,
    arg: Arg<'_>,
    state: &mut Args<'_, '_>,
) -> Result<Specs> {
    let mut specs = Specs::default();
    let end = input.len();
    if *pos == end || input[*pos] == b'}' {
        return Ok(specs);
    }
    // Fill and alignment.
    let align_at = |index: usize| match input.get(index) {
        Some(b'<') => Some(Align::Left),
        Some(b'>') => Some(Align::Right),
        Some(b'=') => Some(Align::Numeric),
        Some(b'^') => Some(Align::Center),
        _ => None,
    };
    let fill_align = if *pos + 1 == end {
        None
    } else {
        align_at(*pos + 1).map(|align| (Some(input[*pos]), align))
    }
    .or_else(|| align_at(*pos).map(|align| (None, align)));
    if let Some((fill, align)) = fill_align {
        if let Some(fill) = fill {
            if fill == b'{' {
                return Err(format_error("invalid fill character '{'"));
            }
            *pos += 2;
            specs.fill = fill;
        } else {
            *pos += 1;
        }
        if align == Align::Numeric {
            require_numeric(arg)?;
        }
        specs.align = align;
    }
    if *pos == end {
        return Ok(specs);
    }
    // Sign and thousands separators.
    match input[*pos] {
        b'+' => {
            check_sign(arg)?;
            specs.sign = Sign::Plus;
            *pos += 1;
        }
        b'-' => {
            check_sign(arg)?;
            specs.sign = Sign::Minus;
            *pos += 1;
        }
        b' ' => {
            check_sign(arg)?;
            specs.sign = Sign::Space;
            *pos += 1;
        }
        c @ (b',' | b'_' | b'\'') => {
            specs.thousands = c;
            *pos += 1;
        }
        b't' => {
            *pos += 1;
            if *pos == end {
                return Ok(specs);
            }
            specs.thousands = input[*pos];
            *pos += 1;
        }
        _ => {}
    }
    if *pos == end {
        return Ok(specs);
    }
    if input[*pos] == b'#' {
        require_numeric(arg)?;
        specs.alt = true;
        *pos += 1;
        if *pos == end {
            return Ok(specs);
        }
    }
    if input[*pos] == b'0' {
        require_numeric(arg)?;
        specs.align = Align::Numeric;
        specs.fill = b'0';
        *pos += 1;
        if *pos == end {
            return Ok(specs);
        }
    }
    // Width.
    if input[*pos].is_ascii_digit() {
        specs.width = parse_nonnegative_int(input, pos)?;
    } else if input[*pos] == b'{' {
        *pos += 1;
        specs.width = parse_dynamic(input, pos, state, "width")?;
    }
    if *pos == end {
        return Ok(specs);
    }
    // Precision.
    if input[*pos] == b'.' {
        *pos += 1;
        match input.get(*pos) {
            Some(c) if c.is_ascii_digit() => {
                specs.precision = i32::try_from(parse_nonnegative_int(input, pos)?).unwrap_or(i32::MAX);
            }
            Some(b'{') => {
                *pos += 1;
                specs.precision =
                    i32::try_from(parse_dynamic(input, pos, state, "precision")?).unwrap_or(i32::MAX);
            }
            _ => return Err(format_error("missing precision specifier")),
        }
        if arg.is_integral() {
            return Err(format_error("precision not allowed for this argument type"));
        }
    }
    // Type.
    if *pos < end && input[*pos] != b'}' {
        specs.ty = input[*pos];
        *pos += 1;
    }
    Ok(specs)
}

/// Copies literal text, unescaping `}}` (`pfs_writer`).
fn write_text(out: &mut Vec<u8>, mut text: &[u8]) -> Result<()> {
    while let Some(index) = memchr::memchr(b'}', text) {
        if text.get(index + 1) != Some(&b'}') {
            return Err(format_error("unmatched '}' in format string"));
        }
        out.extend_from_slice(&text[..=index]);
        text = &text[index + 2..];
    }
    out.extend_from_slice(text);
    Ok(())
}

/// `duckdb_fmt::vformat`.
pub(crate) fn format(input: &[u8], args: &[Arg<'_>]) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(input.len());
    let mut state = Args { args, next: 0 };
    let end = input.len();
    let mut begin = 0;
    while begin != end {
        let Some(offset) = memchr::memchr(b'{', &input[begin..]) else {
            write_text(&mut out, &input[begin..])?;
            break;
        };
        let mut p = begin + offset;
        write_text(&mut out, &input[begin..p])?;
        p += 1;
        if p == end {
            return Err(format_error("invalid format string"));
        }
        match input[p] {
            b'}' => {
                let arg = resolve(&mut state, &ArgId::Auto)?;
                format_arg(&mut out, arg, None)?;
            }
            b'{' => out.push(b'{'),
            _ => {
                let id = parse_arg_id(input, &mut p)?;
                let arg = resolve(&mut state, &id)?;
                match input.get(p) {
                    Some(b'}') => format_arg(&mut out, arg, None)?,
                    Some(b':') => {
                        p += 1;
                        let specs = parse_format_specs(input, &mut p, arg, &mut state)?;
                        if input.get(p) != Some(&b'}') {
                            return Err(format_error("missing '}' in format string"));
                        }
                        format_arg(&mut out, arg, Some(&specs))?;
                    }
                    _ => return Err(format_error("missing '}' in format string")),
                }
            }
        }
        begin = p + 1;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn printf(format: &str, args: &[Arg<'_>]) -> String {
        String::from_utf8(sprintf(format.as_bytes(), args).unwrap()).unwrap()
    }

    fn fmt(input: &str, args: &[Arg<'_>]) -> String {
        String::from_utf8(format(input.as_bytes(), args).unwrap()).unwrap()
    }

    #[test]
    fn printf_matches_fmt() {
        assert_eq!(
            printf(
                "%d + %d = %d",
                &[Arg::LongLong(3), Arg::LongLong(5), Arg::LongLong(8)]
            ),
            "3 + 5 = 8"
        );
        assert_eq!(
            printf(
                "%04d|%-5s|%5.1f",
                &[Arg::LongLong(33), Arg::Str(b"ab"), Arg::Double(3.25)]
            ),
            "0033|ab   |  3.2"
        );
        assert_eq!(printf("%,d", &[Arg::LongLong(123_456_789)]), "123,456,789");
        assert_eq!(printf("%.d", &[Arg::LongLong(123_456_789)]), "123.456.789");
        assert_eq!(
            printf("%x %#o", &[Arg::LongLong(-1), Arg::LongLong(8)]),
            "ffffffffffffffff 010"
        );
    }

    #[test]
    fn format_matches_fmt() {
        assert_eq!(
            fmt("{1} {1} {0} {0}", &[Arg::LongLong(1), Arg::LongLong(2)]),
            "2 2 1 1"
        );
        assert_eq!(
            fmt("{:>6.2f}|{:^7}|{{}}", &[Arg::Double(2.5), Arg::Str(b"ab")]),
            "  2.50|  ab   |{}"
        );
        assert_eq!(fmt("{:,}", &[Arg::LongLong(123_456_789)]), "123,456,789");
        assert_eq!(fmt("{:t|}", &[Arg::LongLong(1234)]), "1|234");
    }
}
