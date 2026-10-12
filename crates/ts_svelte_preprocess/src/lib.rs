//! Pure Rust Markdown/Svelte preprocessing. No configuration code, Node process,
//! remark/rehype callback or external highlighter is evaluated by this crate.
use comrak::nodes::NodeValue;
use comrak::{Arena, Options as MarkdownOptions, format_html, parse_document};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::ops::Range;

mod frontmatter;
mod mapping;
mod protect;
pub use mapping::CopySpan;
use mapping::Output;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Highlight {
    /// mdsvex's default HTML shape for unlabelled/plain-text blocks. Languages
    /// requiring Prism fail explicitly until a native matching grammar exists.
    #[default]
    Mdsvex,
    /// Equivalent to mdsvex's `highlight: false`.
    Disabled,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub struct Options {
    pub smartypants: bool,
    pub highlight: Highlight,
    pub extensions: Vec<String>,
    pub mdsvex_version: String,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            smartypants: true,
            highlight: Highlight::Mdsvex,
            extensions: vec![".md".into(), ".svx".into()],
            mdsvex_version: "0.12.7".into(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Preprocessed {
    pub code: String,
    pub map: Value,
    pub copies: Vec<CopySpan>,
    pub metadata: Option<Value>,
    pub dependencies: Vec<String>,
    /// Describes mapping precision rather than implying rewritten typography,
    /// entity escapes or generated HTML have exact original ranges.
    pub mapping_scope: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Error {
    pub code: &'static str,
    pub message: String,
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for Error {}
fn failure(code: &'static str, message: impl Into<String>) -> Error {
    Error {
        code,
        message: message.into(),
    }
}

/// Resolve the JSON-shaped subset of mdsvex options without executing callbacks.
/// Unknown options, layouts and active remark/rehype plugins are explicit errors.
pub fn options_from_json(value: &Value) -> Result<Options, Error> {
    if value.is_null() {
        return Ok(Options::default());
    }
    let object = value
        .as_object()
        .ok_or_else(|| failure("unsupported_options", "Expected an options object"))?;
    let mut result = Options::default();
    for (name, value) in object {
        match name.as_str() {
            "smartypants" => {
                result.smartypants = value.as_bool().ok_or_else(|| {
                    failure(
                        "unsupported_options",
                        "smartypants must be boolean; custom typography options are unsupported",
                    )
                })?
            }
            "highlight" if value == &Value::Bool(false) => result.highlight = Highlight::Disabled,
            "highlight" if value == &Value::Bool(true) || value.is_null() => {
                result.highlight = Highlight::Mdsvex
            }
            "extensions" => {
                result.extensions = serde_json::from_value(value.clone()).map_err(|_| {
                    failure("unsupported_options", "extensions must be a string array")
                })?
            }
            "extension" => {
                result.extensions = vec![
                    value
                        .as_str()
                        .ok_or_else(|| {
                            failure("unsupported_options", "extension must be a string")
                        })?
                        .into(),
                ]
            }
            "mdsvexVersion" => {
                result.mdsvex_version = value
                    .as_str()
                    .ok_or_else(|| {
                        failure("unsupported_options", "mdsvexVersion must be a string")
                    })?
                    .into()
            }
            "remarkPlugins" | "rehypePlugins" if value.as_array().is_some_and(Vec::is_empty) => {}
            _ => {
                return Err(failure(
                    "unsupported_options",
                    format!(
                        "Unsupported mdsvex option {name}; callbacks and layouts require native implementations"
                    ),
                ));
            }
        }
    }
    validate_options(&result)?;
    Ok(result)
}

fn validate_options(options: &Options) -> Result<(), Error> {
    if options.mdsvex_version != "0.12.7" {
        return Err(failure(
            "unsupported_version",
            "This preprocessor targets mdsvex 0.12.7",
        ));
    }
    if options.extensions.is_empty()
        || options.extensions.iter().any(|extension| {
            !extension.starts_with('.')
                || extension.len() < 2
                || extension.contains(['/', '\\', '?', '#'])
        })
    {
        return Err(failure(
            "unsupported_options",
            "Extensions must be nonempty dot-prefixed filename suffixes",
        ));
    }
    Ok(())
}

#[derive(Clone)]
enum Fragment {
    Copy(Range<usize>),
    Generated(String),
    Sequence(Vec<Fragment>),
}

/// Returns `None` for filenames outside the configured extension set.
///
/// The v3 map includes the original source. Only byte-identical copies receive
/// mappings; generated wrappers and transformed text are deliberately unmapped.
pub fn preprocess(
    source: &str,
    filename: &str,
    options: &Options,
) -> Result<Option<Preprocessed>, Error> {
    validate_options(options)?;
    if !options
        .extensions
        .iter()
        .any(|extension| filename.ends_with(extension))
    {
        return Ok(None);
    }
    if source.len() > 1_048_576 {
        return Err(failure("size_limit", "Markdown source exceeds 1 MiB"));
    }
    let (metadata, body) =
        frontmatter::split(source).map_err(|message| failure("invalid_frontmatter", message))?;
    let metadata_script = metadata
        .as_ref()
        .map(frontmatter::script)
        .transpose()
        .map_err(|message| failure("invalid_frontmatter", message))?;
    let mut prefix = "TSMDNATIVEQ".to_string();
    while source.contains(&prefix) {
        prefix.push('Q');
    }
    let protected = protect::protect(source, body, &prefix)
        .map_err(|message| failure("unsupported_svelte_markup", message))?;
    let module_scripts = protected
        .islands
        .iter()
        .filter(|island| island.module_open.is_some())
        .count();
    if module_scripts > 1 {
        return Err(failure("invalid_svelte_markup", "Multiple module scripts"));
    }
    let mut fragments: Vec<(String, Fragment)> = protected
        .islands
        .iter()
        .map(|island| {
            let fragment =
                if let (Some(open), Some(script)) = (island.module_open, &metadata_script) {
                    Fragment::Sequence(vec![
                        Fragment::Copy(island.original.start..open),
                        Fragment::Generated(script.clone()),
                        Fragment::Copy(open..island.original.end),
                    ])
                } else {
                    Fragment::Copy(island.original.clone())
                };
            (island.token.clone(), fragment)
        })
        .collect();
    let mut markdown = MarkdownOptions::default();
    markdown.parse.smart = options.smartypants;
    markdown.render.r#unsafe = true;
    markdown.extension.table = true;
    markdown.extension.strikethrough = true;
    markdown.extension.tasklist = true;
    markdown.extension.autolink = true;
    let arena = Arena::new();
    let root = parse_document(&arena, &protected.text, &markdown);
    let lines = mapping::lines(&protected.text);
    for node in root.descendants() {
        // Image descendants are rendered as attribute text by comrak. Injecting
        // HTML marker nodes there would change their alt text representation.
        if node
            .ancestors()
            .skip(1)
            .any(|parent| matches!(parent.data.borrow().value, NodeValue::Image(_)))
        {
            continue;
        }
        let mut data = node.data.borrow_mut();
        let position = protect::source_range(data.sourcepos, &protected.text, &lines);
        let replacement = match &mut data.value {
            NodeValue::Text(text) => {
                let exact = position
                    .as_ref()
                    .filter(|range| protected.text.get((*range).clone()) == Some(text.as_ref()))
                    .and_then(|range| protected.original(range.clone()));
                let fragment = if let Some(range) = exact {
                    escaped_fragment(source, range, false)
                } else {
                    Fragment::Generated(escape(text, false))
                };
                Some(fragment)
            }
            NodeValue::Code(code) => {
                let literal = escape(&code.literal, true);
                Some(Fragment::Generated(format!("<code>{literal}</code>")))
            }
            NodeValue::CodeBlock(code) => {
                let language = code
                    .info
                    .split_whitespace()
                    .next()
                    .unwrap_or("")
                    .to_ascii_lowercase();
                if options.highlight == Highlight::Mdsvex
                    && !["", "text", "txt", "plain", "plaintext"].contains(&language.as_str())
                {
                    return Err(failure(
                        "unsupported_highlighting",
                        format!(
                            "Prism highlighting for {language:?} is not implemented; request highlight:false to explicitly disable highlighting"
                        ),
                    ));
                }
                let literal = code.literal.strip_suffix('\n').unwrap_or(&code.literal);
                let literal = escape(literal, true);
                let rendered = if options.highlight == Highlight::Mdsvex {
                    let language = if language.is_empty() {
                        "undefined"
                    } else {
                        &language
                    };
                    // Static escaped HTML has identical rendered content to the
                    // default {@html `...`} optimisation and needs no JS string.
                    format!(
                        "<pre class=\"language-{language}\"><code class=\"language-{language}\">{literal}</code></pre>\n"
                    )
                } else if language.is_empty() {
                    format!("<pre><code>{literal}\n</code></pre>\n")
                } else {
                    format!(
                        "<pre><code class=\"language-{}\">{literal}\n</code></pre>\n",
                        escape(&language, true)
                    )
                };
                Some(Fragment::Generated(rendered))
            }
            NodeValue::HtmlInline(html) => {
                let fragment = position
                    .and_then(|range| protected.original(range))
                    .filter(|range| source.get(range.clone()) == Some(html.as_str()))
                    .map_or_else(|| Fragment::Generated(html.clone()), Fragment::Copy);
                Some(fragment)
            }
            NodeValue::HtmlBlock(html) => {
                // Markdown-looking content inside raw HTML cannot be passed
                // through as though mdsvex's custom nested parser processed it.
                if html.literal.contains("**")
                    || html.literal.contains("\n# ")
                    || html.literal.contains("\n- ")
                {
                    return Err(failure(
                        "unsupported_svelte_markup",
                        "Markdown nested inside raw HTML blocks needs the mdsvex nested-markup parser",
                    ));
                }
                let fragment = position
                    .and_then(|range| protected.original(range))
                    .and_then(|range| {
                        let copied = source.get(range.clone())?;
                        if copied == html.literal {
                            Some(Fragment::Copy(range))
                        } else if html.literal.strip_suffix('\n') == Some(copied) {
                            // Comrak's HTML block source position excludes its
                            // terminal newline, while its literal includes it.
                            // Preserve exact HTML positions and map the newline
                            // only when that byte really exists in the input.
                            if source.as_bytes().get(range.end) == Some(&b'\n') {
                                Some(Fragment::Copy(range.start..range.end + 1))
                            } else {
                                Some(Fragment::Sequence(vec![
                                    Fragment::Copy(range),
                                    Fragment::Generated("\n".into()),
                                ]))
                            }
                        } else {
                            None
                        }
                    })
                    .unwrap_or_else(|| Fragment::Generated(html.literal.clone()));
                Some(fragment)
            }
            NodeValue::Link(link) | NodeValue::Image(link) => {
                link.url = link.url.replace("%7B", "{").replace("%7D", "}");
                None
            }
            _ => None,
        };
        if let Some(fragment) = replacement {
            let token = format!("{prefix}R{}END", fragments.len());
            fragments.push((token.clone(), fragment));
            data.value = NodeValue::Raw(token);
        }
    }
    let mut rendered = String::new();
    format_html(root, &markdown, &mut rendered)
        .map_err(|error| failure("markdown_render", error.to_string()))?;
    rendered = external_links(&rendered);
    let mut output = Output::new(source);
    if module_scripts == 0
        && let Some(script) = metadata_script
    {
        output.generated("<script context=\"module\">");
        output.generated(&script);
        output.generated("</script>\n");
    }
    output.generated("\n");
    let index = fragments
        .iter()
        .map(|(token, fragment)| (token.as_str(), fragment))
        .collect();
    append_generated(&rendered, &index, &prefix, &mut output, 0)?;
    if output.code.contains(&prefix) {
        return Err(failure(
            "preprocess_incomplete",
            "A protected Svelte source token was not restored",
        ));
    }
    if output.code.len() > 16 * 1_048_576 {
        return Err(failure("size_limit", "Generated component exceeds 16 MiB"));
    }
    let map = output.map(filename);
    Ok(Some(Preprocessed {
        code: output.code,
        map,
        copies: output.copies,
        metadata,
        dependencies: Vec::new(),
        mapping_scope: "Exact copied source only; generated markup, frontmatter exports, normalized code and typography are unmapped",
    }))
}

fn append_fragment(
    fragment: &Fragment,
    fragments: &BTreeMap<&str, &Fragment>,
    prefix: &str,
    output: &mut Output<'_>,
    depth: usize,
) -> Result<(), Error> {
    match fragment {
        Fragment::Copy(range) => output.copy(range.clone()),
        Fragment::Generated(text) => append_generated(text, fragments, prefix, output, depth + 1)?,
        Fragment::Sequence(parts) => {
            for part in parts {
                append_fragment(part, fragments, prefix, output, depth + 1)?;
            }
        }
    }
    Ok(())
}

fn append_generated(
    text: &str,
    fragments: &BTreeMap<&str, &Fragment>,
    prefix: &str,
    output: &mut Output<'_>,
    depth: usize,
) -> Result<(), Error> {
    if depth > 16 {
        return Err(failure(
            "preprocess_incomplete",
            "Recursive preprocessing marker",
        ));
    }
    let mut cursor = 0;
    while let Some(relative) = text[cursor..].find(prefix) {
        let prefix_start = cursor + relative;
        let comment = prefix_start >= cursor + 4 && &text[prefix_start - 4..prefix_start] == "<!--";
        let start = if comment {
            prefix_start - 4
        } else {
            prefix_start
        };
        let end = text[prefix_start..]
            .find("END")
            .ok_or_else(|| failure("preprocess_incomplete", "Unclosed preprocessing marker"))?
            + prefix_start
            + 3;
        let end = if comment && text[end..].starts_with("-->") {
            end + 3
        } else {
            end
        };
        let fragment = fragments
            .get(&text[start..end])
            .ok_or_else(|| failure("preprocess_incomplete", "Unknown preprocessing marker"))?;
        output.generated(&text[cursor..start]);
        append_fragment(fragment, fragments, prefix, output, depth + 1)?;
        cursor = end;
    }
    output.generated(&text[cursor..]);
    Ok(())
}

fn escaped_fragment(source: &str, range: Range<usize>, braces: bool) -> Fragment {
    let mut parts = Vec::new();
    let mut start = range.start;
    for (offset, character) in source[range.clone()].char_indices() {
        if let Some(entity) = entity(character, braces) {
            let offset = offset + range.start;
            if start < offset {
                parts.push(Fragment::Copy(start..offset));
            }
            parts.push(Fragment::Generated(entity.into()));
            start = offset + character.len_utf8();
        }
    }
    if start < range.end {
        parts.push(Fragment::Copy(start..range.end));
    }
    Fragment::Sequence(parts)
}

fn entity(character: char, braces: bool) -> Option<&'static str> {
    match character {
        '&' => Some("&amp;"),
        '<' => Some("&lt;"),
        '>' => Some("&gt;"),
        '"' => Some("&quot;"),
        '{' if braces => Some("&#123;"),
        '}' if braces => Some("&#125;"),
        _ => None,
    }
}
fn escape(text: &str, braces: bool) -> String {
    let mut output = String::new();
    for character in text.chars() {
        if let Some(entity) = entity(character, braces) {
            output.push_str(entity);
        } else {
            output.push(character);
        }
    }
    output
}

fn external_links(html: &str) -> String {
    let mut result = String::new();
    let mut cursor = 0;
    while let Some(relative) = html[cursor..].find("<a href=\"") {
        let start = cursor + relative;
        let value = start + "<a href=\"".len();
        result.push_str(&html[cursor..start]);
        if ["https://", "http://", "//"]
            .iter()
            .any(|prefix| html[value..].starts_with(prefix))
        {
            result.push_str("<a rel=\"nofollow\" href=\"");
        } else {
            result.push_str("<a href=\"");
        }
        cursor = value;
    }
    result.push_str(&html[cursor..]);
    result
}

#[cfg(test)]
mod tests;
