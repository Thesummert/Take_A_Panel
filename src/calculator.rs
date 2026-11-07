/// Evaluate arithmetic only; never execute input as code or a shell command.
pub fn evaluate(input: &str) -> Option<String> {
    if input.len() > 4096 {
        return None;
    }
    let input: String = input
        .chars()
        .map(|c| match c {
            '×' => '*',
            '÷' => '/',
            '−' => '-',
            '（' => '(',
            '）' => ')',
            _ => c,
        })
        .collect();
    let input = input
        .trim()
        .strip_suffix('=')
        .unwrap_or(input.trim())
        .trim();
    let mut parser = Parser {
        input: input.as_bytes(),
        pos: 0,
    };
    let value = parser.expression(0, 0)?;
    if parser.peek().is_some() || !value.is_finite() {
        return None;
    }
    // Keep 15 significant digits, hiding ordinary binary floating point noise
    // (e.g. 0.1 + 0.2) while preserving very small and very large results.
    let rounded: f64 = format!("{value:.14e}").parse().ok()?;
    let value = if rounded.is_finite() { rounded } else { value };
    Some(if value == 0.0 {
        "0".into()
    } else {
        value.to_string()
    })
}

struct Parser<'a> {
    input: &'a [u8],
    pos: usize,
}

impl Parser<'_> {
    fn peek(&mut self) -> Option<u8> {
        while self
            .input
            .get(self.pos)
            .is_some_and(u8::is_ascii_whitespace)
        {
            self.pos += 1;
        }
        self.input.get(self.pos).copied()
    }

    fn expression(&mut self, minimum: u8, depth: usize) -> Option<f64> {
        if depth >= 64 {
            return None;
        }
        let mut value = match self.peek()? {
            b'+' | b'-' => {
                let negative = self.input[self.pos] == b'-';
                self.pos += 1;
                let value = self.expression(5, depth + 1)?;
                if negative { -value } else { value }
            }
            b'(' => {
                self.pos += 1;
                let value = self.expression(0, depth + 1)?;
                if self.peek()? != b')' {
                    return None;
                }
                self.pos += 1;
                value
            }
            _ => self.number()?,
        };
        while let Some(operator) = self.peek() {
            let (left, right) = match operator {
                b'+' | b'-' => (1, 2),
                b'*' | b'/' => (3, 4),
                b'^' => (6, 5), // Powers associate to the right, above unary minus.
                _ => break,
            };
            if left < minimum {
                break;
            }
            self.pos += 1;
            let rhs = self.expression(right, depth + 1)?;
            value = match operator {
                b'+' => value + rhs,
                b'-' => value - rhs,
                b'*' => value * rhs,
                b'/' if rhs != 0.0 => value / rhs,
                b'^' => value.powf(rhs),
                _ => return None,
            };
            if !value.is_finite() {
                return None;
            }
        }
        Some(value)
    }

    fn number(&mut self) -> Option<f64> {
        let start = self.pos;
        while self
            .input
            .get(self.pos)
            .is_some_and(|c| c.is_ascii_digit() || *c == b'.')
        {
            self.pos += 1;
        }
        if self
            .input
            .get(self.pos)
            .is_some_and(|c| matches!(c, b'e' | b'E'))
        {
            self.pos += 1;
            if self
                .input
                .get(self.pos)
                .is_some_and(|c| matches!(c, b'+' | b'-'))
            {
                self.pos += 1;
            }
            while self.input.get(self.pos).is_some_and(u8::is_ascii_digit) {
                self.pos += 1;
            }
        }
        let value: f64 = std::str::from_utf8(&self.input[start..self.pos])
            .ok()?
            .parse()
            .ok()?;
        value.is_finite().then_some(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arithmetic_respects_precedence_parentheses_and_unary_signs() {
        for (input, expected) in [
            ("1+2*3", "7"),
            ("(1+2)*3", "9"),
            ("8/4/2", "1"),
            ("2^3^2", "512"),
            ("-2^2", "-4"),
            ("(-2)^2", "4"),
            ("2^-2", "0.25"),
            ("3*-2", "-6"),
            ("1--2", "3"),
            (" （2 + 3）×4÷2 = ", "10"),
            ("−5 + 2", "-3"),
        ] {
            assert_eq!(evaluate(input).as_deref(), Some(expected), "{input}");
        }
    }

    #[test]
    fn decimals_scientific_notation_and_output_formatting() {
        for (input, expected) in [
            ("0.1+0.2", "0.3"),
            (".5+1.", "1.5"),
            ("1e3+2E-1", "1000.2"),
            ("1/4", "0.25"),
            ("-0", "0"),
            ("42", "42"),
            ("1e-20*2", "0.00000000000000000002"),
        ] {
            assert_eq!(evaluate(input).as_deref(), Some(expected), "{input}");
        }
    }

    #[test]
    fn rejects_incomplete_non_arithmetic_and_non_finite_input() {
        for input in [
            "",
            " ",
            "=",
            "1+",
            "(1+2",
            "1+2)",
            "2(3)",
            "1..2",
            "1 2",
            "1e",
            "1/0",
            "0/0",
            "(-1)^.5",
            "1e309",
            "1e308*10",
            "ss",
            "clp",
            "Firefox",
            "nan",
            "inf",
            "1+2abc",
            "1; echo test",
            "1==",
        ] {
            assert_eq!(evaluate(input), None, "{input}");
        }
        assert_eq!(
            evaluate(&format!("{}1{}", "(".repeat(100), ")".repeat(100))),
            None
        );
        assert_eq!(evaluate(&"1+".repeat(3000)), None);
    }
}
