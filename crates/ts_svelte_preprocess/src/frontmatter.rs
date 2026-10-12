use chrono::{DateTime, NaiveDate, SecondsFormat, Utc};
use serde_json::{Map, Number, Value};
use std::collections::{BTreeMap, BTreeSet};
use yaml_rust::parser::{Event, Parser};
use yaml_rust::scanner::{Scanner, TScalarStyle, TokenType};

pub(crate) fn split(source: &str) -> Result<(Option<Value>, usize), String> {
    let source_offset = usize::from(source.starts_with('\u{feff}')) * '\u{feff}'.len_utf8();
    let source_tail = &source[source_offset..];
    let mut lines = source_tail.split_inclusive('\n');
    if lines.next().map(|line| line.trim_end_matches(['\r', '\n'])) != Some("---") {
        return Ok((None, source_offset));
    }
    let first = source_tail.find('\n').map_or(source_tail.len(), |n| n + 1);
    let mut end = first;
    for line in lines {
        if line.trim_end_matches(['\r', '\n']) == "---" {
            let value = parse(&source_tail[first..end])?;
            return Ok((Some(value), source_offset + end + line.len()));
        }
        end += line.len();
    }
    Err("Unclosed YAML frontmatter".into())
}

fn parse(source: &str) -> Result<Value, String> {
    for token in Scanner::new(source.chars()) {
        if let TokenType::Tag(handle, name) = token.1
            && (handle != "!!"
                || !["str", "null", "bool", "int", "float", "timestamp"].contains(&name.as_str()))
        {
            return Err("Custom or collection YAML tags are not supported".into());
        }
    }
    let mut parser = Parser::new(source.chars());
    let mut anchors = BTreeMap::new();
    let mut budget = 1_048_576;
    loop {
        match next(&mut parser)? {
            Event::StreamStart | Event::DocumentStart => {}
            Event::StreamEnd => return Ok(Value::Object(Map::new())),
            event => {
                let value = node(event, &mut parser, &mut anchors, 0, &mut budget)?;
                while !matches!(next(&mut parser)?, Event::StreamEnd) {
                    if !matches!(
                        parser.peek().map_err(|e| e.to_string())?.0,
                        Event::StreamEnd
                    ) {
                        return Err("Multiple YAML documents are not supported".into());
                    }
                }
                return if value.is_object() {
                    Ok(value)
                } else if value.is_null() {
                    Ok(Value::Object(Map::new()))
                } else {
                    Err("Frontmatter must be a mapping".into())
                };
            }
        }
    }
}

fn next<T: Iterator<Item = char>>(parser: &mut Parser<T>) -> Result<Event, String> {
    parser
        .next()
        .map(|(event, _)| event)
        .map_err(|e| format!("Invalid YAML: {e}"))
}

fn node<T: Iterator<Item = char>>(
    event: Event,
    parser: &mut Parser<T>,
    anchors: &mut BTreeMap<usize, Value>,
    depth: usize,
    budget: &mut usize,
) -> Result<Value, String> {
    if depth > 64 {
        return Err("YAML nesting exceeds 64 levels".into());
    }
    let (value, anchor) = match event {
        Event::Scalar(text, style, anchor, tag) => (scalar(&text, style, tag)?, anchor),
        Event::Alias(id) => (
            anchors
                .get(&id)
                .cloned()
                .ok_or("Recursive or unresolved YAML alias")?,
            0,
        ),
        Event::SequenceStart(anchor) => {
            let mut values = Vec::new();
            loop {
                let event = next(parser)?;
                if event == Event::SequenceEnd {
                    break;
                }
                values.push(node(event, parser, anchors, depth + 1, budget)?);
            }
            (Value::Array(values), anchor)
        }
        Event::MappingStart(anchor) => {
            let mut values = Map::new();
            let mut explicit = BTreeSet::new();
            loop {
                let event = next(parser)?;
                if event == Event::MappingEnd {
                    break;
                }
                let merge = matches!(&event, Event::Scalar(text, TScalarStyle::Plain, _, None) if text == "<<");
                let key = node(event, parser, anchors, depth + 1, budget)?;
                let key = match key {
                    Value::String(key) => key,
                    Value::Number(n) => n.to_string(),
                    Value::Bool(b) => b.to_string(),
                    Value::Null => "null".into(),
                    _ => return Err("Complex YAML mapping keys are not supported".into()),
                };
                let value = node(next(parser)?, parser, anchors, depth + 1, budget)?;
                if merge {
                    let objects = if let Value::Array(objects) = value {
                        objects
                    } else {
                        vec![value]
                    };
                    for object in objects {
                        let Value::Object(object) = object else {
                            return Err(
                                "YAML merge value must be a mapping or a sequence of mappings"
                                    .into(),
                            );
                        };
                        for (key, value) in object {
                            values.entry(key).or_insert(value);
                        }
                    }
                } else {
                    if !explicit.insert(key.clone()) {
                        return Err(format!("Duplicate YAML key {key:?}"));
                    }
                    values.insert(key, value);
                }
            }
            (Value::Object(values), anchor)
        }
        _ => return Err("Unexpected YAML parser event".into()),
    };
    let size = value.to_string().len();
    *budget = budget
        .checked_sub(size)
        .ok_or("Expanded YAML exceeds size limit")?;
    if anchor != 0 {
        anchors.insert(anchor, value.clone());
    }
    Ok(value)
}

fn scalar(text: &str, style: TScalarStyle, tag: Option<TokenType>) -> Result<Value, String> {
    let tag = match tag {
        None => None,
        Some(TokenType::Tag(handle, name)) if handle == "!!" || handle == "tag:yaml.org,2002:" => {
            Some(name)
        }
        Some(_) => return Err("Custom YAML tags are not supported".into()),
    };
    if tag.as_deref() == Some("str") || (tag.is_none() && style != TScalarStyle::Plain) {
        return Ok(Value::String(text.into()));
    }
    if tag
        .as_deref()
        .is_some_and(|tag| !["null", "bool", "int", "float", "timestamp"].contains(&tag))
    {
        return Err("Unsupported YAML scalar tag".into());
    }
    let lower = text.to_ascii_lowercase();
    let inferred = if text.is_empty() || text == "~" || lower == "null" {
        Some(("null", Value::Null))
    } else if lower == "true" || lower == "false" {
        Some(("bool", Value::Bool(lower == "true")))
    } else if let Some(date) = timestamp(text) {
        Some(("timestamp", Value::String(date)))
    } else if let Some(number) = number(text) {
        Some(number)
    } else {
        None
    };
    match (tag, inferred) {
        (None, Some((_, value))) => Ok(value),
        (None, None) => Ok(Value::String(text.into())),
        (Some(tag), Some((kind, value))) if tag == kind || (tag == "float" && kind == "int") => {
            Ok(value)
        }
        (Some(tag), _) => Err(format!("Invalid YAML {tag} scalar {text:?}")),
    }
}

fn timestamp(text: &str) -> Option<String> {
    if text.len() == 10 {
        let date = NaiveDate::parse_from_str(text, "%Y-%m-%d").ok()?;
        return Some(
            date.and_hms_opt(0, 0, 0)?
                .and_utc()
                .to_rfc3339_opts(SecondsFormat::Millis, true),
        );
    }
    let normalized = text.replacen('t', "T", 1);
    let normalized = normalized.replacen(' ', "T", 1);
    let normalized = normalized
        .replace(" Z", "Z")
        .replace(" +", "+")
        .replace(" -", "-");
    DateTime::parse_from_rfc3339(&normalized).ok().map(|date| {
        date.with_timezone(&Utc)
            .to_rfc3339_opts(SecondsFormat::Millis, true)
    })
}

fn number(text: &str) -> Option<(&'static str, Value)> {
    let value = text.replace('_', "");
    let (negative, unsigned) = value
        .strip_prefix('-')
        .map_or((false, value.as_str()), |n| (true, n));
    let unsigned = unsigned.strip_prefix('+').unwrap_or(unsigned);
    let radix = if unsigned.starts_with("0x") {
        Some((16, &unsigned[2..]))
    } else if unsigned.starts_with("0b") {
        Some((2, &unsigned[2..]))
    } else if unsigned.len() > 1
        && unsigned.starts_with('0')
        && unsigned.bytes().all(|c| (b'0'..=b'7').contains(&c))
    {
        Some((8, &unsigned[1..]))
    } else {
        None
    };
    let integer = if let Some((radix, digits)) = radix {
        i64::from_str_radix(digits, radix)
            .ok()
            .map(|n| if negative { -n } else { n })
    } else {
        value.parse::<i64>().ok()
    };
    if let Some(integer) = integer {
        return Some(("int", Value::Number(integer.into())));
    }
    if [".nan", ".inf", "-.inf", "+.inf"].contains(&value.to_ascii_lowercase().as_str()) {
        return Some(("float", Value::Null));
    }
    // YAML sexagesimal forms accepted by js-yaml's default safe schema.
    if unsigned.contains(':') && !unsigned.starts_with('0') {
        let mut total = 0f64;
        for (index, part) in unsigned.split(':').enumerate() {
            let number = part.parse::<f64>().ok()?;
            if number < 0. || (index != 0 && number >= 60.) {
                return None;
            }
            total = total * 60. + number;
        }
        return Number::from_f64(if negative { -total } else { total }).map(|n| {
            (
                if value.contains('.') { "float" } else { "int" },
                Value::Number(n),
            )
        });
    }
    if value.contains(['.', 'e', 'E']) {
        return value
            .parse::<f64>()
            .ok()
            .and_then(Number::from_f64)
            .map(|n| ("float", Value::Number(n)));
    }
    None
}

pub(crate) fn script(metadata: &Value) -> Result<String, String> {
    let object = metadata
        .as_object()
        .ok_or("Frontmatter must be a mapping")?;
    let mut names = BTreeSet::new();
    let mut bindings = Vec::new();
    for key in object.keys() {
        let name = key.replace('-', "_");
        if name.is_empty()
            || !name.chars().enumerate().all(|(i, c)| {
                c == '_' || c == '$' || c.is_ascii_alphabetic() || (i > 0 && c.is_ascii_digit())
            })
            || [
                "metadata",
                "await",
                "break",
                "case",
                "catch",
                "class",
                "const",
                "continue",
                "debugger",
                "default",
                "delete",
                "do",
                "else",
                "enum",
                "export",
                "extends",
                "false",
                "finally",
                "for",
                "function",
                "if",
                "import",
                "in",
                "instanceof",
                "let",
                "new",
                "null",
                "return",
                "super",
                "switch",
                "this",
                "throw",
                "true",
                "try",
                "typeof",
                "var",
                "void",
                "while",
                "with",
                "yield",
                "implements",
                "interface",
                "package",
                "private",
                "protected",
                "public",
                "static",
            ]
            .contains(&name.as_str())
            || !names.insert(name.clone())
        {
            return Err(format!(
                "Frontmatter key {key:?} cannot become a unique JavaScript binding"
            ));
        }
        bindings.push(if key == &name {
            name
        } else {
            format!("{}: {name}", serde_json::to_string(key).unwrap())
        });
    }
    let json = metadata
        .to_string()
        .replace('<', "\\u003c")
        .replace('\u{2028}', "\\u2028")
        .replace('\u{2029}', "\\u2029");
    Ok(format!(
        "\nexport const metadata = {json};\nconst {{ {} }} = metadata;\n",
        bindings.join(", ")
    ))
}
