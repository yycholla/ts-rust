//! Pinned Tailwind 4.2.1 utils/brace-expansion.ts (MIT, Tailwind Labs),
//! with explicit resource bounds for untrusted snapshot input.
use crate::value::segment;

pub(super) fn expand(pattern: &str) -> Result<Vec<String>, String> {
    fn inner(pattern: &str, depth: usize, bytes: &mut usize) -> Result<Vec<String>, String> {
        if depth > 128 {
            return Err("Inline source brace nesting limit exceeded".into());
        }
        let Some(index) = pattern.find('{') else {
            return Ok(vec![pattern.into()]);
        };
        let rest = &pattern[index..];
        let mut nesting = 0;
        let mut end = rest.rfind('}');
        for (i, c) in rest.char_indices() {
            if c == '{' {
                nesting += 1;
            }
            if c == '}' {
                nesting -= 1;
                if nesting == 0 {
                    end = Some(i);
                    break;
                }
            }
        }
        let end = end.ok_or_else(|| format!("The pattern `{pattern}` is not balanced."))?;
        let inside = &rest[1..end];
        let components: Vec<_> = inside.split("..").collect();
        let numeric = (components.len() == 2 || components.len() == 3)
            && components.iter().all(|part| {
                let digits = part.strip_prefix('-').unwrap_or(part);
                !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit())
            });
        let parts = if numeric {
            let values = components
                .iter()
                .map(|p| {
                    p.parse::<f64>()
                        .map_err(|_| "Invalid inline source sequence")
                })
                .collect::<Result<Vec<_>, _>>()?;
            if values.iter().any(|v| !v.is_finite()) {
                return Err("Inline source sequence exceeds numeric bounds".into());
            }
            let (start, end) = (values[0], values[1]);
            let mut step = values
                .get(2)
                .copied()
                .unwrap_or(if start <= end { 1.0 } else { -1.0 });
            if step == 0.0 {
                return Err("Step cannot be zero in sequence expansion.".into());
            }
            let increasing = start < end;
            step = if increasing { step.abs() } else { -step.abs() };
            let mut parts = Vec::new();
            let mut current = start;
            while if increasing {
                current <= end
            } else {
                current >= end
            } {
                if parts.len() >= 100_000 {
                    return Err("Inline source sequence exceeds candidate limit".into());
                }
                let text = if current == 0.0 {
                    "0".into()
                } else if current.abs() >= 1e21 {
                    let value = format!("{current:e}");
                    let (mantissa, exponent) = value.split_once('e').unwrap();
                    format!(
                        "{mantissa}e{}{exponent}",
                        if exponent.starts_with('-') { "" } else { "+" }
                    )
                } else {
                    current.to_string()
                };
                parts.push(text);
                let next = current + step;
                if next == current {
                    return Err(
                        "Inline source sequence step cannot advance at this magnitude".into(),
                    );
                }
                current = next;
            }
            parts
        } else {
            segment(inside, ',')
                .into_iter()
                .map(str::to_string)
                .collect()
        };
        let mut expanded_parts = Vec::new();
        for part in parts {
            expanded_parts.extend(inner(&part, depth + 1, bytes)?);
            if expanded_parts.len() > 100_000 {
                return Err("Inline source expansion exceeds candidate limit".into());
            }
        }
        let tails = inner(&rest[end + 1..], depth + 1, bytes)?;
        if tails.len().saturating_mul(expanded_parts.len()) > 100_000 {
            return Err("Inline source expansion exceeds candidate limit".into());
        }
        let mut result = Vec::new();
        for tail in tails {
            for part in &expanded_parts {
                let candidate = format!("{}{part}{tail}", &pattern[..index]);
                *bytes = bytes.saturating_add(candidate.len());
                if *bytes > 16 * 1024 * 1024 {
                    return Err("Inline source expansion exceeds 16 MiB".into());
                }
                result.push(candidate);
            }
        }
        Ok(result)
    }
    inner(pattern, 0, &mut 0)
}
