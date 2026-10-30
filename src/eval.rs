//! Integer expressions for the `eval` builtin.
//!
//! Follows GNU m4: 32-bit signed arithmetic that wraps on overflow, C operator
//! precedence plus `**` for exponentiation, and `0x`, `0b` and leading-zero
//! octal literals.

/// Evaluate `expr`, or describe why it is invalid.
pub(crate) fn evaluate(expr: &str) -> Result<i32, &'static str> {
    let tokens = tokenize(expr)?;
    let mut parser = Parser { tokens, pos: 0 };
    let value = parser.binary(0)?;
    if parser.pos != parser.tokens.len() {
        return Err("bad expression");
    }
    Ok(value)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tok {
    Num(i32),
    Op(&'static str),
    Open,
    Close,
}

// Longest operators first so `<=` wins over `<`.
const OPERATORS: [&str; 21] = [
    "**", "<<", ">>", "<=", ">=", "==", "!=", "&&", "||", "+", "-", "*", "/", "%", "<", ">", "&",
    "|", "^", "!", "~",
];

fn tokenize(expr: &str) -> Result<Vec<Tok>, &'static str> {
    let bytes = expr.as_bytes();
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if c.is_ascii_whitespace() {
            i += 1;
        } else if c.is_ascii_digit() {
            let start = i;
            while i < bytes.len() && bytes[i].is_ascii_alphanumeric() {
                i += 1;
            }
            tokens.push(Tok::Num(parse_number(&expr[start..i])?));
        } else if c == b'(' {
            tokens.push(Tok::Open);
            i += 1;
        } else if c == b')' {
            tokens.push(Tok::Close);
            i += 1;
        } else if let Some(op) = OPERATORS.iter().find(|op| expr[i..].starts_with(**op)) {
            tokens.push(Tok::Op(op));
            i += op.len();
        } else {
            return Err("bad expression");
        }
    }
    Ok(tokens)
}

fn parse_number(text: &str) -> Result<i32, &'static str> {
    let (digits, radix) = if let Some(hex) = text.strip_prefix("0x").or(text.strip_prefix("0X")) {
        (hex, 16)
    } else if let Some(bin) = text.strip_prefix("0b").or(text.strip_prefix("0B")) {
        (bin, 2)
    } else if text.len() > 1 && text.starts_with('0') {
        (&text[1..], 8)
    } else {
        (text, 10)
    };
    // Accumulate with wrapping, as GNU m4 does for oversized literals.
    let mut value: i32 = 0;
    for ch in digits.chars() {
        let digit = ch.to_digit(radix).ok_or("bad number")?;
        value = value.wrapping_mul(radix as i32).wrapping_add(digit as i32);
    }
    if digits.is_empty() {
        return Err("bad number");
    }
    Ok(value)
}

/// Binary operator precedence, lowest first; `**` is right-associative.
fn precedence(op: &str) -> Option<u8> {
    Some(match op {
        "||" => 1,
        "&&" => 2,
        "|" => 3,
        "^" => 4,
        "&" => 5,
        "==" | "!=" => 6,
        "<" | "<=" | ">" | ">=" => 7,
        "<<" | ">>" => 8,
        "+" | "-" => 9,
        "*" | "/" | "%" => 10,
        "**" => 11,
        _ => return None,
    })
}

struct Parser {
    tokens: Vec<Tok>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> Option<Tok> {
        self.tokens.get(self.pos).copied()
    }

    fn binary(&mut self, min_prec: u8) -> Result<i32, &'static str> {
        let mut lhs = self.unary()?;
        while let Some(Tok::Op(op)) = self.peek() {
            let Some(prec) = precedence(op) else { break };
            if prec < min_prec {
                break;
            }
            self.pos += 1;
            let next_min = if op == "**" { prec } else { prec + 1 };
            let rhs = self.binary(next_min)?;
            lhs = apply(op, lhs, rhs)?;
        }
        Ok(lhs)
    }

    fn unary(&mut self) -> Result<i32, &'static str> {
        match self.peek() {
            Some(Tok::Op(op @ ("-" | "+" | "!" | "~"))) => {
                self.pos += 1;
                let value = self.unary()?;
                Ok(match op {
                    "-" => value.wrapping_neg(),
                    "+" => value,
                    "!" => i32::from(value == 0),
                    _ => !value,
                })
            }
            Some(Tok::Num(value)) => {
                self.pos += 1;
                Ok(value)
            }
            Some(Tok::Open) => {
                self.pos += 1;
                let value = self.binary(0)?;
                if self.peek() != Some(Tok::Close) {
                    return Err("missing right parenthesis");
                }
                self.pos += 1;
                Ok(value)
            }
            _ => Err("bad expression"),
        }
    }
}

fn apply(op: &str, lhs: i32, rhs: i32) -> Result<i32, &'static str> {
    Ok(match op {
        "||" => i32::from(lhs != 0 || rhs != 0),
        "&&" => i32::from(lhs != 0 && rhs != 0),
        "|" => lhs | rhs,
        "^" => lhs ^ rhs,
        "&" => lhs & rhs,
        "==" => i32::from(lhs == rhs),
        "!=" => i32::from(lhs != rhs),
        "<" => i32::from(lhs < rhs),
        "<=" => i32::from(lhs <= rhs),
        ">" => i32::from(lhs > rhs),
        ">=" => i32::from(lhs >= rhs),
        "<<" => lhs.wrapping_shl(rhs as u32),
        ">>" => lhs.wrapping_shr(rhs as u32),
        "+" => lhs.wrapping_add(rhs),
        "-" => lhs.wrapping_sub(rhs),
        "*" => lhs.wrapping_mul(rhs),
        "/" | "%" if rhs == 0 => return Err("divide by zero"),
        "/" => lhs.wrapping_div(rhs),
        "%" => lhs.wrapping_rem(rhs),
        "**" if rhs < 0 => return Err("negative exponent"),
        "**" => lhs.wrapping_pow(rhs as u32),
        _ => return Err("bad expression"),
    })
}

/// Render `value` in `radix`, zero-padded to at least `width` digits.
pub(crate) fn format(value: i32, radix: u32, width: usize) -> String {
    let negative = value < 0;
    let mut magnitude = i64::from(value).unsigned_abs();
    let mut digits = Vec::new();
    loop {
        digits.push(std::char::from_digit((magnitude % u64::from(radix)) as u32, radix).unwrap());
        magnitude /= u64::from(radix);
        if magnitude == 0 {
            break;
        }
    }
    while digits.len() < width {
        digits.push('0');
    }
    if negative {
        digits.push('-');
    }
    digits.iter().rev().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_precedence_and_associativity() {
        assert_eq!(evaluate("2**3+7/2-(-1)"), Ok(12));
        assert_eq!(evaluate("2**3**2"), Ok(512));
        assert_eq!(evaluate("1+2*3"), Ok(7));
        assert_eq!(evaluate("1<2&&3>=3"), Ok(1));
        assert_eq!(evaluate("7-2-1"), Ok(4));
    }

    #[test]
    fn test_literals() {
        assert_eq!(evaluate("0x10"), Ok(16));
        assert_eq!(evaluate("010"), Ok(8));
        assert_eq!(evaluate("0b101"), Ok(5));
    }

    #[test]
    fn test_wraps_like_32_bit_ints() {
        assert_eq!(evaluate("2147483647+1"), Ok(i32::MIN));
    }

    #[test]
    fn test_errors() {
        assert_eq!(evaluate("1/0"), Err("divide by zero"));
        assert_eq!(evaluate("(1"), Err("missing right parenthesis"));
        assert_eq!(evaluate("1 +"), Err("bad expression"));
        assert_eq!(evaluate("2**-1"), Err("negative exponent"));
    }

    #[test]
    fn test_format_radix_and_width() {
        assert_eq!(format(255, 16, 4), "00ff");
        assert_eq!(format(-5, 10, 3), "-005");
    }
}
