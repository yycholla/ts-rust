//! Candidate and variant parsing against explicit design-system registrations.
//! Port of Tailwind CSS v4.2.1 candidate.ts; no utility generators are implied.
use crate::value::{decode_arbitrary_value, is_valid_arbitrary, segment};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Modifier {
    Arbitrary { value: String },
    Named { value: String },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum UtilityValue {
    Arbitrary {
        #[serde(rename = "dataType")]
        data_type: Option<String>,
        value: String,
    },
    Named {
        value: String,
        fraction: Option<String>,
    },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Variant {
    Arbitrary {
        selector: String,
        relative: bool,
    },
    Static {
        root: String,
    },
    Functional {
        root: String,
        value: Option<Modifier>,
        modifier: Option<Modifier>,
    },
    Compound {
        root: String,
        modifier: Option<Modifier>,
        variant: Box<Variant>,
    },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Candidate {
    #[serde(flatten)]
    pub kind: CandidateKind,
    /// Rightmost source variant first, matching Tailwind's application order.
    pub variants: Vec<Variant>,
    pub important: bool,
    pub raw: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum CandidateKind {
    Arbitrary {
        property: String,
        value: String,
        modifier: Option<Modifier>,
    },
    Static {
        root: String,
    },
    Functional {
        root: String,
        value: Option<UtilityValue>,
        modifier: Option<Modifier>,
    },
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum VariantKind {
    Static,
    Functional,
    Compound,
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Compounds(pub u8);
impl Compounds {
    pub const NEVER: Self = Self(0);
    pub const AT_RULES: Self = Self(1);
    pub const STYLE_RULES: Self = Self(2);
    pub const ALL: Self = Self(3);
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VariantRegistration {
    pub kind: VariantKind,
    pub compounds: Compounds,
    pub compounds_with: Compounds,
}
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CandidateRegistry {
    pub prefix: Option<String>,
    pub static_utilities: BTreeSet<String>,
    pub functional_utilities: BTreeSet<String>,
    pub variants: BTreeMap<String, VariantRegistration>,
}
impl CandidateRegistry {
    pub fn register_variant(
        &mut self,
        name: impl Into<String>,
        kind: VariantKind,
        compounds: Compounds,
        compounds_with: Compounds,
    ) {
        self.variants.insert(
            name.into(),
            VariantRegistration {
                kind,
                compounds,
                compounds_with,
            },
        );
    }
    pub fn compounds_with(&self, parent: &str, child: &Variant) -> bool {
        let Some(parent) = self.variants.get(parent) else {
            return false;
        };
        if parent.kind != VariantKind::Compound {
            return false;
        }
        let child = match child {
            Variant::Arbitrary { selector, .. } => compounds_for_selectors(&[selector]),
            Variant::Static { root }
            | Variant::Functional { root, .. }
            | Variant::Compound { root, .. } => self
                .variants
                .get(root)
                .map_or(Compounds::NEVER, |r| r.compounds),
        };
        parent.compounds_with.0 & child.0 != 0
    }
}
pub fn compounds_for_selectors(selectors: &[&str]) -> Compounds {
    let mut result = 0;
    for selector in selectors {
        if selector.starts_with('@') {
            if !["@media", "@supports", "@container"]
                .iter()
                .any(|s| selector.starts_with(s))
            {
                return Compounds::NEVER;
            }
            result |= 1;
        } else {
            if selector.contains("::") {
                return Compounds::NEVER;
            }
            result |= 2;
        }
    }
    Compounds(result)
}
fn named(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'.' | b'%' | b'-'))
}
fn decode(value: &str) -> Option<String> {
    let value = decode_arbitrary_value(value).ok()?;
    is_valid_arbitrary(&value).then_some(value)
}
pub fn parse_modifier(input: &str) -> Option<Modifier> {
    if let Some(inner) = input.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
        let value = decode(inner)?;
        if value.trim().is_empty() {
            return None;
        }
        return Some(Modifier::Arbitrary { value });
    }
    if let Some(inner) = input.strip_prefix('(').and_then(|s| s.strip_suffix(')')) {
        if !inner.starts_with("--") || !is_valid_arbitrary(inner) {
            return None;
        }
        return Some(Modifier::Arbitrary {
            value: decode_arbitrary_value(&format!("var({inner})")).ok()?,
        });
    }
    named(input).then(|| Modifier::Named {
        value: input.into(),
    })
}
/// All registered root interpretations in longest-root-first order.
pub fn find_roots<'a>(
    input: &'a str,
    exists: impl Fn(&str) -> bool,
) -> Vec<(&'a str, Option<&'a str>)> {
    let mut roots = Vec::new();
    if exists(input) {
        roots.push((input, None));
    }
    for (i, _) in input.match_indices('-').rev() {
        if i == 0 {
            break;
        }
        let root = &input[..i];
        if exists(root) {
            let value = &input[i + 1..];
            if value.is_empty() || root == "@" {
                break;
            }
            roots.push((root, Some(value)));
        }
    }
    if input.starts_with('@') && exists("@") {
        roots.push(("@", Some(&input[1..])));
    }
    roots
}
pub fn parse_candidate(input: &str, registry: &CandidateRegistry) -> Vec<Candidate> {
    if input.len() > 64 * 1024 {
        return Vec::new();
    }
    let mut raw_variants = segment(input, ':');
    if let Some(prefix) = registry.prefix.as_ref().filter(|p| !p.is_empty()) {
        if raw_variants.len() == 1 || raw_variants[0] != prefix {
            return Vec::new();
        }
        raw_variants.remove(0);
    }
    let mut base = raw_variants.pop().unwrap_or("");
    let mut variants = Vec::new();
    if raw_variants.len() > 256 {
        return Vec::new();
    }
    for v in raw_variants.into_iter().rev() {
        let Some(v) = parse_variant(v, registry) else {
            return Vec::new();
        };
        variants.push(v);
    }
    let important = if let Some(s) = base.strip_suffix('!') {
        base = s;
        true
    } else if let Some(s) = base.strip_prefix('!') {
        base = s;
        true
    } else {
        false
    };
    let make = |kind| Candidate {
        kind,
        variants: variants.clone(),
        important,
        raw: input.into(),
    };
    let mut result = Vec::new();
    if registry.static_utilities.contains(base) && !base.contains('[') {
        result.push(make(CandidateKind::Static { root: base.into() }));
    }
    let segments = segment(base, '/');
    if segments.get(2).is_some_and(|s| !s.is_empty()) {
        return result;
    }
    let base = segments[0];
    let modifier_segment = segments.get(1).copied();
    let modifier = if let Some(m) = modifier_segment {
        let Some(m) = parse_modifier(m) else {
            return result;
        };
        Some(m)
    } else {
        None
    };
    if base.starts_with('[') {
        let Some(inner) = base.strip_prefix('[').and_then(|s| s.strip_suffix(']')) else {
            return result;
        };
        if !inner
            .as_bytes()
            .first()
            .is_some_and(|c| c.is_ascii_lowercase() || *c == b'-')
        {
            return result;
        }
        let Some((property, value)) = inner.split_once(':') else {
            return result;
        };
        if property.is_empty() || value.is_empty() {
            return result;
        }
        if let Some(value) = decode(value) {
            result.push(make(CandidateKind::Arbitrary {
                property: property.into(),
                value,
                modifier,
            }));
        }
        return result;
    }
    let roots: Vec<(String, Option<String>)>;
    if base.ends_with(']') {
        let Some((root, value)) = base.split_once("-[") else {
            return result;
        };
        if !registry.functional_utilities.contains(root) {
            return result;
        }
        roots = vec![(root.into(), Some(format!("[{value}")))];
    } else if base.ends_with(')') {
        let Some((root, rest)) = base.split_once("-(") else {
            return result;
        };
        if !registry.functional_utilities.contains(root) {
            return result;
        }
        let value = &rest[..rest.len() - 1];
        let parts = segment(value, ':');
        let (hint, value) = if parts.len() == 2 {
            (Some(parts[0]), parts[1])
        } else {
            (None, value)
        };
        if !value.starts_with("--") || !is_valid_arbitrary(value) {
            return result;
        }
        let value = if let Some(h) = hint {
            format!("[{h}:var({value})]")
        } else {
            format!("[var({value})]")
        };
        roots = vec![(root.into(), Some(value))];
    } else {
        roots = find_roots(base, |r| registry.functional_utilities.contains(r))
            .into_iter()
            .map(|(r, v)| (r.into(), v.map(str::to_owned)))
            .collect();
    }
    for (root, value) in roots {
        let value = if let Some(value) = value {
            if let Some(i) = value.find('[') {
                if !value.ends_with(']') {
                    return result;
                }
                let Some(mut decoded) = decode(&value[i + 1..value.len() - 1]) else {
                    continue;
                };
                let mut hint = None;
                for (i, c) in decoded.char_indices() {
                    if c == ':' {
                        hint = Some(decoded[..i].to_string());
                        decoded = decoded[i + 1..].to_string();
                        break;
                    }
                    if c != '-' && !c.is_ascii_lowercase() {
                        break;
                    }
                }
                if decoded.trim().is_empty() || hint.as_deref() == Some("") {
                    continue;
                }
                Some(UtilityValue::Arbitrary {
                    data_type: hint,
                    value: decoded,
                })
            } else {
                if !named(&value) {
                    continue;
                }
                let fraction = if matches!(&modifier, Some(Modifier::Arbitrary { .. })) {
                    None
                } else {
                    modifier_segment.map(|m| format!("{value}/{m}"))
                };
                Some(UtilityValue::Named { value, fraction })
            }
        } else {
            None
        };
        result.push(make(CandidateKind::Functional {
            root,
            value,
            modifier: modifier.clone(),
        }));
    }
    result
}
pub fn parse_variant(input: &str, registry: &CandidateRegistry) -> Option<Variant> {
    parse_variant_depth(input, registry, 0)
}
fn parse_variant_depth(input: &str, registry: &CandidateRegistry, depth: usize) -> Option<Variant> {
    if depth > 128 || input.len() > 64 * 1024 {
        return None;
    }
    if let Some(inner) = input.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
        if inner.starts_with('@') && input.contains('&') {
            return None;
        }
        let mut selector = decode(inner)?;
        if selector.trim().is_empty() {
            return None;
        }
        let relative = selector.starts_with(['>', '+', '~']);
        if !relative && !selector.starts_with('@') && !selector.contains('&') {
            selector = format!("&:is({selector})");
        }
        return Some(Variant::Arbitrary { selector, relative });
    }
    let parts = segment(input, '/');
    if parts.get(2).is_some_and(|s| !s.is_empty()) {
        return None;
    }
    let raw_modifier = parts.get(1).copied();
    for (root, value) in find_roots(parts[0], |root| registry.variants.contains_key(root)) {
        let entry = &registry.variants[root];
        match entry.kind {
            VariantKind::Static => {
                if value.is_some() || raw_modifier.is_some() {
                    return None;
                }
                return Some(Variant::Static { root: root.into() });
            }
            VariantKind::Functional => {
                let modifier = if let Some(m) = raw_modifier {
                    Some(parse_modifier(m)?)
                } else {
                    None
                };
                let value = if let Some(v) = value {
                    if v.ends_with(']') {
                        let Some(inner) = v.strip_prefix('[') else {
                            continue;
                        };
                        let value = decode(&inner[..inner.len() - 1])?;
                        if value.trim().is_empty() {
                            return None;
                        }
                        Some(Modifier::Arbitrary { value })
                    } else if v.ends_with(')') {
                        let Some(inner) = v.strip_prefix('(') else {
                            continue;
                        };
                        let value = decode(&inner[..inner.len() - 1])?;
                        if value.trim().is_empty() || !value.starts_with("--") {
                            return None;
                        }
                        Some(Modifier::Arbitrary {
                            value: format!("var({value})"),
                        })
                    } else {
                        if !named(v) {
                            continue;
                        }
                        Some(Modifier::Named { value: v.into() })
                    }
                } else {
                    None
                };
                return Some(Variant::Functional {
                    root: root.into(),
                    value,
                    modifier,
                });
            }
            VariantKind::Compound => {
                let mut value = value?.to_string();
                let mut raw_modifier = raw_modifier;
                if let Some(m) = raw_modifier.filter(|m| !m.is_empty()) {
                    if matches!(root, "not" | "has" | "in") {
                        value.push('/');
                        value.push_str(m);
                        raw_modifier = None;
                    }
                }
                let child = parse_variant_depth(&value, registry, depth + 1)?;
                if !registry.compounds_with(root, &child) {
                    return None;
                }
                let modifier = if let Some(m) = raw_modifier {
                    Some(parse_modifier(m)?)
                } else {
                    None
                };
                return Some(Variant::Compound {
                    root: root.into(),
                    modifier,
                    variant: Box::new(child),
                });
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    fn registry() -> CandidateRegistry {
        let mut r = CandidateRegistry::default();
        r.static_utilities.insert("flex".into());
        for name in ["bg", "bg-red", "w", "-mt"] {
            r.functional_utilities.insert(name.into());
        }
        r.register_variant(
            "hover",
            VariantKind::Static,
            Compounds::STYLE_RULES,
            Compounds::NEVER,
        );
        r.register_variant(
            "before",
            VariantKind::Static,
            Compounds::NEVER,
            Compounds::NEVER,
        );
        r.register_variant(
            "data",
            VariantKind::Functional,
            Compounds::STYLE_RULES,
            Compounds::NEVER,
        );
        r.register_variant(
            "@",
            VariantKind::Functional,
            Compounds::AT_RULES,
            Compounds::NEVER,
        );
        r.register_variant(
            "group",
            VariantKind::Compound,
            Compounds::STYLE_RULES,
            Compounds::STYLE_RULES,
        );
        r.register_variant(
            "not",
            VariantKind::Compound,
            Compounds::STYLE_RULES,
            Compounds::ALL,
        );
        r
    }
    #[test]
    fn registered_roots_fractions_and_prefix() {
        let mut r = registry();
        let c = parse_candidate("bg-red-500/50", &r);
        assert_eq!(c.len(), 2);
        assert!(
            matches!(&c[0].kind,CandidateKind::Functional{root,value:Some(UtilityValue::Named{fraction:Some(f),..}),..} if root=="bg-red" && f=="500/50")
        );
        assert!(parse_candidate("unregistered-500", &r).is_empty());
        r.prefix = Some("tw".into());
        assert!(parse_candidate("hover:flex", &r).is_empty());
        let c = parse_candidate("tw:hover:flex!", &r);
        assert_eq!(c.len(), 1);
        assert!(c[0].important);
    }
    #[test]
    fn arbitrary_shorthand_hints_and_injection() {
        let r = registry();
        let c = parse_candidate("bg-(color:--my_color)/[50%]", &r);
        assert!(
            matches!(&c[0].kind,CandidateKind::Functional{value:Some(UtilityValue::Arbitrary{data_type:Some(t),value}),..} if t=="color" && value=="var(--my_color)")
        );
        for bad in [
            "bg-[]",
            "bg-(red)",
            "bg-red/50/25",
            "[color:red;display:none]",
            "[Color:red]",
            "bg-[red}]",
        ] {
            assert!(parse_candidate(bad, &r).is_empty(), "{bad}");
        }
        assert_eq!(parse_candidate("[--foo:hello_world]", &r).len(), 1);
    }
    #[test]
    fn compound_compatibility_and_forwarded_modifier() {
        let r = registry();
        let c = parse_candidate("not-group-hover/name:flex", &r);
        assert!(
            matches!(&c[0].variants[0],Variant::Compound{root,modifier:None,variant} if root=="not" && matches!(variant.as_ref(),Variant::Compound{modifier:Some(Modifier::Named{value}),..} if value=="name"))
        );
        assert!(parse_candidate("group-before:flex", &r).is_empty());
        assert!(parse_candidate("group-[@media(width>1px)]:flex", &r).is_empty());
        assert_eq!(parse_candidate("@sm:flex", &r).len(), 1);
        // The pinned parser accepts this named value; the @ generator must
        // subsequently reject the unresolved negative breakpoint.
        assert_eq!(parse_candidate("@-sm:flex", &r).len(), 1);
    }
    #[test]
    fn arbitrary_variant_and_reverse_order() {
        let r = registry();
        let c = parse_candidate("hover:[p]:flex", &r);
        assert!(
            matches!(&c[0].variants[0],Variant::Arbitrary{selector,relative:false} if selector=="&:is(p)")
        );
        assert!(parse_candidate("[@media(width>1px){&:hover}]:flex", &r).is_empty());
    }
}
