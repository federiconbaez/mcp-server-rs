//! Tiny arithmetic expression evaluator used by the sandboxed `calc` tool.
//!
//! Grammar (recursive descent):
//! ```text
//! expr   := term (('+' | '-') term)*
//! term   := factor (('*' | '/' | '%') factor)*
//! factor := unary ('^' factor)?
//! unary  := '-' unary | primary
//! primary:= number | '(' expr ')'
//! ```
//! It is deliberately pure: no I/O and no allocation-heavy work, so it can run
//! inside a strict seccomp filter.

/// Maximum accepted input length, in bytes.
pub const MAX_INPUT: usize = 4096;
/// Maximum nesting depth (parentheses, unary minus, exponent chains).
pub const MAX_DEPTH: usize = 64;

pub fn eval(src: &str) -> Result<f64, String> {
    if src.len() > MAX_INPUT {
        return Err(format!("expression longer than {MAX_INPUT} bytes"));
    }
    let mut p = Parser {
        s: src.as_bytes(),
        i: 0,
        depth: 0,
    };
    let v = p.expr()?;
    p.ws();
    if p.i != p.s.len() {
        return Err(format!("unexpected character at offset {}", p.i));
    }
    if !v.is_finite() {
        return Err("result is not a finite number".into());
    }
    Ok(v)
}

struct Parser<'a> {
    s: &'a [u8],
    i: usize,
    depth: usize,
}

impl Parser<'_> {
    fn ws(&mut self) {
        while self.i < self.s.len() && self.s[self.i].is_ascii_whitespace() {
            self.i += 1;
        }
    }

    fn peek(&mut self) -> Option<u8> {
        self.ws();
        self.s.get(self.i).copied()
    }

    fn enter(&mut self) -> Result<(), String> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            Err(format!("nesting deeper than {MAX_DEPTH}"))
        } else {
            Ok(())
        }
    }

    fn expr(&mut self) -> Result<f64, String> {
        let mut v = self.term()?;
        while let Some(c @ (b'+' | b'-')) = self.peek() {
            self.i += 1;
            let r = self.term()?;
            v = if c == b'+' { v + r } else { v - r };
        }
        Ok(v)
    }

    fn term(&mut self) -> Result<f64, String> {
        let mut v = self.factor()?;
        while let Some(c @ (b'*' | b'/' | b'%')) = self.peek() {
            self.i += 1;
            let r = self.factor()?;
            if (c == b'/' || c == b'%') && r == 0.0 {
                return Err("division by zero".into());
            }
            v = match c {
                b'*' => v * r,
                b'/' => v / r,
                _ => v % r,
            };
        }
        Ok(v)
    }

    fn factor(&mut self) -> Result<f64, String> {
        let base = self.unary()?;
        if self.peek() == Some(b'^') {
            self.i += 1;
            self.enter()?;
            let e = self.factor()?;
            self.depth -= 1;
            return Ok(base.powf(e));
        }
        Ok(base)
    }

    fn unary(&mut self) -> Result<f64, String> {
        if self.peek() == Some(b'-') {
            self.i += 1;
            self.enter()?;
            let v = self.unary()?;
            self.depth -= 1;
            return Ok(-v);
        }
        self.primary()
    }

    fn primary(&mut self) -> Result<f64, String> {
        match self.peek() {
            Some(b'(') => {
                self.i += 1;
                self.enter()?;
                let v = self.expr()?;
                self.depth -= 1;
                if self.peek() != Some(b')') {
                    return Err(format!("expected ')' at offset {}", self.i));
                }
                self.i += 1;
                Ok(v)
            }
            Some(c) if c.is_ascii_digit() || c == b'.' => {
                let start = self.i;
                while self.i < self.s.len()
                    && (self.s[self.i].is_ascii_digit() || self.s[self.i] == b'.')
                {
                    self.i += 1;
                }
                let txt = std::str::from_utf8(&self.s[start..self.i]).map_err(|e| e.to_string())?;
                txt.parse::<f64>()
                    .map_err(|_| format!("invalid number '{txt}' at offset {start}"))
            }
            Some(_) => Err(format!("unexpected character at offset {}", self.i)),
            None => Err("unexpected end of expression".into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::eval;

    #[test]
    fn precedence_and_associativity() {
        assert_eq!(eval("1 + 2 * 3").unwrap(), 7.0);
        assert_eq!(eval("(1 + 2) * 3").unwrap(), 9.0);
        assert_eq!(eval("2 ^ 3 ^ 2").unwrap(), 512.0);
        assert_eq!(eval("-2 ^ 2").unwrap(), 4.0);
        assert_eq!(eval("10 % 4 - 8 / 2").unwrap(), -2.0);
    }

    #[test]
    fn errors() {
        assert!(eval("1 / 0").is_err());
        assert!(eval("1 +").is_err());
        assert!(eval("2 3").is_err());
        assert!(eval("abc").is_err());
        assert!(eval(&"(".repeat(100)).is_err());
        assert!(eval("10 ^ 400").is_err());
    }
}
