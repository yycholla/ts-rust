//! Pure value helpers ported from Tailwind 4.2.1 utilities and infer-data-type.
use crate::{
    candidate::Modifier,
    theme::{Theme, ThemeOptions},
    value::segment,
};
use std::cmp::Ordering;
pub fn number(input: &str) -> bool {
    let b = input.as_bytes();
    let mut i = usize::from(matches!(b.first(), Some(b'+' | b'-')));
    let mut digits = 0;
    while b.get(i).is_some_and(u8::is_ascii_digit) {
        i += 1;
        digits += 1;
    }
    if b.get(i) == Some(&b'.') {
        i += 1;
        let start = i;
        while b.get(i).is_some_and(u8::is_ascii_digit) {
            i += 1;
            digits += 1;
        }
        if start == i {
            return false;
        }
    }
    if digits == 0 {
        return false;
    }
    if matches!(b.get(i), Some(b'e' | b'E')) {
        i += 1;
        if matches!(b.get(i), Some(b'+' | b'-')) {
            i += 1;
        }
        let start = i;
        while b.get(i).is_some_and(u8::is_ascii_digit) {
            i += 1;
        }
        if start == i {
            return false;
        }
    }
    i == b.len()
}
fn canonical_number(input: &str) -> Option<f64> {
    let n = input.parse::<f64>().ok()?;
    if !n.is_finite() {
        return None;
    }
    let repr = if n == 0.0 {
        // JavaScript String(Number("-0")) is "0", unlike Rust Display.
        "0".into()
    } else if n.abs() >= 1e21 || n.abs() < 1e-6 {
        let e = format!("{n:e}");
        if let Some((a, b)) = e.split_once('e') {
            format!("{a}e{}{b}", if b.starts_with('-') { "" } else { "+" })
        } else {
            e
        }
    } else {
        n.to_string()
    };
    (repr == input).then_some(n)
}
pub fn integer(input: &str) -> bool {
    canonical_number(input).is_some_and(|n| n >= 0.0 && n.fract() == 0.0)
}
pub fn quarter(input: &str) -> bool {
    canonical_number(input).is_some_and(|n| n >= 0.0 && n % 0.25 == 0.0)
}
pub fn with_alpha(value: &str, alpha: &str) -> String {
    let alpha = alpha
        .parse::<f64>()
        .ok()
        .filter(|n| n.is_finite())
        .map_or_else(|| alpha.to_string(), |n| format!("{}%", n * 100.0));
    if alpha == "100%" {
        value.into()
    } else {
        format!("color-mix(in oklab, {value} {alpha}, transparent)")
    }
}
pub fn replace_alpha(value: &str, alpha: &str) -> String {
    let alpha = alpha
        .parse::<f64>()
        .ok()
        .filter(|n| n.is_finite())
        .map_or_else(|| alpha.to_string(), |n| format!("{}%", n * 100.0));
    format!("oklab(from {value} l a b / {alpha})")
}
pub fn as_color(value: &str, modifier: Option<&Modifier>, theme: &Theme) -> Option<String> {
    match modifier {
        None => Some(value.into()),
        Some(Modifier::Arbitrary { value: alpha }) => Some(with_alpha(value, alpha)),
        Some(Modifier::Named { value: alpha }) => {
            let alpha = theme
                .resolve(Some(alpha), &["--opacity"], ThemeOptions::NONE)
                .or_else(|| quarter(alpha).then(|| format!("{alpha}%")))?;
            Some(with_alpha(value, &alpha))
        }
    }
}
pub fn theme_color(
    name: &str,
    keys: &[&str],
    modifier: Option<&Modifier>,
    theme: &Theme,
) -> Option<String> {
    let value = match name {
        "inherit" => "inherit".into(),
        "transparent" => "transparent".into(),
        "current" => "currentcolor".into(),
        _ => theme.resolve(Some(name), keys, ThemeOptions::NONE)?,
    };
    as_color(&value, modifier, theme)
}
fn math(value: &str) -> bool {
    [
        "calc", "min", "max", "clamp", "mod", "rem", "sin", "cos", "tan", "asin", "acos", "atan",
        "atan2", "pow", "sqrt", "hypot", "log", "exp", "round",
    ]
    .iter()
    .any(|name| value.contains(&format!("{name}(")))
}
pub fn length(value: &str) -> bool {
    math(value)
        || [
            "cm", "mm", "Q", "in", "pc", "pt", "px", "em", "ex", "ch", "rem", "lh", "rlh", "vw",
            "vh", "vmin", "vmax", "vb", "vi", "svw", "svh", "lvw", "lvh", "dvw", "dvh", "cqw",
            "cqh", "cqi", "cqb", "cqmin", "cqmax",
        ]
        .iter()
        .any(|unit| value.strip_suffix(unit).is_some_and(number))
}
fn percentage(value: &str) -> bool {
    math(value) || value.strip_suffix('%').is_some_and(number)
}
pub fn color(value: &str) -> bool {
    let value = value.to_ascii_lowercase();
    value.starts_with('#')
        || [
            "rgb(",
            "rgba(",
            "hsl(",
            "hsla(",
            "hwb(",
            "color(",
            "lab(",
            "lch(",
            "oklab(",
            "oklch(",
            "light-dark(",
            "color-mix(",
        ]
        .iter()
        .any(|prefix| value.starts_with(prefix))
        || include_str!("utility_color_names.txt")
            .lines()
            .any(|name| name == value)
}
pub fn infer<'a>(value: &str, types: &[&'a str]) -> Option<&'a str> {
    if value.starts_with("var(") {
        return None;
    }
    types.iter().copied().find(|ty| match *ty {
        "color" => color(value),
        "length" => length(value),
        "percentage" => percentage(value),
        "number" => number(value) || math(value),
        "integer" => integer(value),
        "ratio" => {
            math(value)
                || value
                    .split_once('/')
                    .is_some_and(|(a, b)| number(a.trim()) && number(b.trim()))
        }
        "line-width" => segment(value, ' ')
            .iter()
            .all(|p| length(p) || number(p) || matches!(*p, "thin" | "medium" | "thick")),
        "url" => value.starts_with("url(") && value.ends_with(')'),
        "image" => {
            let parts = segment(value, ',');
            let mut count = 0;
            parts.iter().all(|p| {
                if p.starts_with("var(") {
                    true
                } else if (p.starts_with("url(") && p.ends_with(')'))
                    || [
                        "linear-gradient(",
                        "radial-gradient(",
                        "conic-gradient(",
                        "repeating-linear-gradient(",
                        "repeating-radial-gradient(",
                        "repeating-conic-gradient(",
                        "element(",
                        "image(",
                        "cross-fade(",
                        "image-set(",
                    ]
                    .iter()
                    .any(|f| p.starts_with(f))
                {
                    count += 1;
                    true
                } else {
                    false
                }
            }) && count > 0
        }
        "position" => {
            let mut count = 0;
            segment(value, ' ').iter().all(|p| {
                if p.starts_with("var(") {
                    true
                } else if matches!(*p, "center" | "top" | "right" | "bottom" | "left")
                    || length(p)
                    || percentage(p)
                {
                    count += 1;
                    true
                } else {
                    false
                }
            }) && count > 0
        }
        "bg-size" => {
            let mut count = 0;
            let valid = segment(value, ',').iter().all(|p| {
                if matches!(*p, "cover" | "contain") {
                    count += 1;
                    return true;
                }
                let parts = segment(p, ' ');
                if !matches!(parts.len(), 1 | 2) {
                    return false;
                }
                if parts
                    .iter()
                    .all(|p| *p == "auto" || length(p) || percentage(p))
                {
                    count += 1;
                }
                true
            });
            valid && count > 0
        }
        "generic-name" => matches!(
            value,
            "serif"
                | "sans-serif"
                | "monospace"
                | "cursive"
                | "fantasy"
                | "system-ui"
                | "ui-serif"
                | "ui-sans-serif"
                | "ui-monospace"
                | "ui-rounded"
                | "math"
                | "emoji"
                | "fangsong"
        ),
        "family-name" => {
            let mut count = 0;
            segment(value, ',').iter().all(|p| {
                if p.as_bytes().first().is_some_and(u8::is_ascii_digit) {
                    return false;
                }
                if !p.starts_with("var(") {
                    count += 1;
                }
                true
            }) && count > 0
        }
        "absolute-size" => matches!(
            value,
            "xx-small"
                | "x-small"
                | "small"
                | "medium"
                | "large"
                | "x-large"
                | "xx-large"
                | "xxx-large"
        ),
        "relative-size" => matches!(value, "larger" | "smaller"),
        "angle" => ["deg", "rad", "grad", "turn"]
            .iter()
            .any(|unit| value.strip_suffix(unit).is_some_and(number)),
        "vector" => {
            let parts: Vec<_> = value.split(' ').filter(|s| !s.is_empty()).collect();
            parts.len() == 3 && parts.into_iter().all(number)
        }
        _ => false,
    })
}
pub fn replace_shadow_colors(input: &str, mut replace: impl FnMut(&str) -> String) -> String {
    segment(input, ',')
        .into_iter()
        .map(|shadow| {
            let shadow = shadow.trim();
            let mut color = None;
            let mut offsets = 0;
            for part in segment(shadow, ' ')
                .into_iter()
                .filter(|p| !p.trim().is_empty())
            {
                if matches!(part, "inset" | "inherit" | "initial" | "revert" | "unset") {
                    continue;
                }
                let p = part.strip_prefix('-').unwrap_or(part);
                if p.as_bytes().first().is_some_and(u8::is_ascii_digit)
                    || (p.starts_with('.') && p.as_bytes().get(1).is_some_and(u8::is_ascii_digit))
                {
                    offsets += 1;
                } else if color.is_none() {
                    color = Some(part);
                }
            }
            if offsets < 2 {
                return shadow.into();
            }
            let replacement = replace(color.unwrap_or("currentcolor"));
            if let Some(color) = color {
                shadow.replacen(color, &replacement, 1)
            } else {
                format!("{shadow} {replacement}")
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}
pub fn compare_breakpoints(a: &str, b: &str) -> Ordering {
    fn bucket(s: &str) -> String {
        s.find('(').map_or_else(
            || {
                s.chars()
                    .filter(|c| !c.is_ascii_digit() && *c != '.')
                    .collect()
            },
            |i| s[..i].into(),
        )
    }
    fn integer_prefix(s: &str) -> Option<i64> {
        let end = s
            .char_indices()
            .take_while(|(i, c)| c.is_ascii_digit() || (*i == 0 && matches!(c, '-' | '+')))
            .map(|(i, c)| i + c.len_utf8())
            .last()?;
        s[..end].parse().ok()
    }
    let cmp = bucket(a).cmp(&bucket(b));
    if cmp != Ordering::Equal {
        return cmp;
    }
    match (integer_prefix(a), integer_prefix(b)) {
        (Some(a), Some(b)) => a.cmp(&b),
        _ => a.cmp(b),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn canonical_numbers() {
        for bad in ["01", "1.0", ".5", "-0", "NaN", "Infinity", "1e3"] {
            assert!(!quarter(bad), "{bad}");
        }
        for good in ["0", "0.25", "1.5", "200"] {
            assert!(quarter(good));
        }
        assert!(!integer("0.5"));
    }
    #[test]
    fn color_type_and_shadow() {
        assert_eq!(infer("var(--v)", &["color", "length"]), None);
        assert_eq!(
            infer("calc(1rem + 1px)", &["color", "length"]),
            Some("length")
        );
        assert_eq!(with_alpha("red", "1"), "red");
        assert_eq!(
            replace_shadow_colors("0 2px 4px #0003, inset 1px 2px", |c| format!(
                "var(--shadow, {c})"
            )),
            "0 2px 4px var(--shadow, #0003), inset 1px 2px var(--shadow, currentcolor)"
        );
    }
}
