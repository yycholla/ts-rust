//! Source-only syntax facts for editors. These contain no runtime transforms or
//! generated TypeScript and remain usable while a component is incomplete.
use rsvelte_core::ast::template::{Fragment, TemplateNode};
use serde::Serialize;
use ts_svelte::{ByteRange, compatibility::Profile};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyntaxNode {
    pub kind: &'static str,
    pub name: String,
    pub span: ByteRange,
    pub name_span: Option<ByteRange>,
    pub closing_name: Option<ByteRange>,
    pub children: Vec<SyntaxNode>,
}

fn element(source: &str, name: &str, span: ByteRange, children: Vec<SyntaxNode>) -> SyntaxNode {
    let name_span = source
        .get(span.start..span.start.saturating_add(name.len() + 1))
        .filter(|s| s.starts_with('<') && &s[1..] == name)
        .map(|_| ByteRange {
            start: span.start + 1,
            end: span.start + 1 + name.len(),
        });
    let closing_name = source.get(span.start..span.end).and_then(|text| {
        let offset = text.rfind("</")?;
        let tail = text.get(offset + 2..)?;
        let after = tail.strip_prefix(name)?;
        if !after.strip_suffix('>')?.trim().is_empty() {
            return None;
        }
        Some(ByteRange {
            start: span.start + offset + 2,
            end: span.start + offset + 2 + name.len(),
        })
    });
    SyntaxNode {
        kind: "element",
        name: name.into(),
        span,
        name_span,
        closing_name,
        children,
    }
}

fn fragment(
    source: &str,
    fragment: &Fragment<'_>,
    depth: usize,
    remaining: &mut usize,
) -> Result<Vec<SyntaxNode>, String> {
    if depth > 128 {
        return Err("Editor syntax nesting exceeds 128".into());
    }
    let mut result = Vec::new();
    for node in &fragment.nodes {
        if *remaining == 0 {
            return Err("Editor syntax exceeds 20000 nodes".into());
        }
        *remaining -= 1;
        let (start, end) = node.span();
        let span = ByteRange {
            start: start as usize,
            end: end as usize,
        };
        if source.get(span.start..span.end).is_none() {
            return Err("Invalid editor syntax span".into());
        }
        let mut children = Vec::new();
        let mut add = |child| -> Result<(), String> {
            children.extend(self::fragment(source, child, depth + 1, remaining)?);
            Ok(())
        };
        let (name, kind) = match node {
            TemplateNode::RegularElement(n) => {
                add(&n.fragment)?;
                (n.name.as_str(), "element")
            }
            TemplateNode::Component(n) => {
                add(&n.fragment)?;
                (n.name.as_str(), "element")
            }
            TemplateNode::TitleElement(n) => {
                add(&n.fragment)?;
                (n.name.as_str(), "element")
            }
            TemplateNode::SlotElement(n) => {
                add(&n.fragment)?;
                (n.name.as_str(), "element")
            }
            TemplateNode::SvelteBody(n)
            | TemplateNode::SvelteDocument(n)
            | TemplateNode::SvelteFragment(n)
            | TemplateNode::SvelteBoundary(n)
            | TemplateNode::SvelteHead(n)
            | TemplateNode::SvelteOptions(n)
            | TemplateNode::SvelteSelf(n)
            | TemplateNode::SvelteWindow(n) => {
                add(&n.fragment)?;
                (n.name.as_str(), "element")
            }
            TemplateNode::SvelteComponent(n) => {
                add(&n.fragment)?;
                (n.name.as_str(), "element")
            }
            TemplateNode::SvelteElement(n) => {
                add(&n.fragment)?;
                (n.name.as_str(), "element")
            }
            TemplateNode::IfBlock(n) => {
                add(&n.consequent)?;
                if let Some(n) = &n.alternate {
                    add(n)?;
                }
                ("if", "block")
            }
            TemplateNode::EachBlock(n) => {
                add(&n.body)?;
                if let Some(n) = &n.fallback {
                    add(n)?;
                }
                ("each", "block")
            }
            TemplateNode::AwaitBlock(n) => {
                for f in [&n.pending, &n.then, &n.catch].into_iter().flatten() {
                    add(f)?;
                }
                ("await", "block")
            }
            TemplateNode::KeyBlock(n) => {
                add(&n.fragment)?;
                ("key", "block")
            }
            TemplateNode::SnippetBlock(n) => {
                add(&n.body)?;
                ("snippet", "block")
            }
            TemplateNode::Comment(_) => ("comment", "comment"),
            _ => continue,
        };
        result.push(if kind == "element" {
            element(source, name, span, children)
        } else {
            SyntaxNode {
                kind,
                name: name.into(),
                span,
                name_span: None,
                closing_name: None,
                children,
            }
        });
    }
    Ok(result)
}

/// Parse lightweight structural facts using the selected compiler syntax profile.
/// # Errors
/// Reports malformed unrecoverable source, resource limits and unsupported BOMs.
pub fn facts(source: &str, profile: &Profile) -> Result<Vec<SyntaxNode>, String> {
    if source.len() > super::MAX_SOURCE_BYTES {
        return Err("Source exceeds 1 MiB".into());
    }
    if source.starts_with('\u{feff}') {
        return Err("BOM source requires normalized editor offsets".into());
    }
    let arena = oxc_allocator::Allocator::default();
    let ast = rsvelte_core::compiler::phases::phase1_parse::parse_with_compatibility(
        source,
        &arena,
        rsvelte_core::ParseOptions {
            modern: true,
            loose: true,
            defer_script_parse: true,
            lenient_script: true,
            skip_expression_loc: true,
            skip_non_css_lang_style: true,
            ..Default::default()
        },
        profile.svelte_minor(),
    )
    .map_err(|e| format!("{e:?}"))?;
    let mut nodes = fragment(source, &ast.fragment, 0, &mut 20_000)?;
    for script in [ast.instance.as_deref(), ast.module.as_deref()]
        .into_iter()
        .flatten()
    {
        let mut node = element(
            source,
            "script",
            ByteRange {
                start: script.start as usize,
                end: script.end as usize,
            },
            Vec::new(),
        );
        node.kind = "script";
        nodes.push(node);
    }
    if let Some(style) = ast.css {
        let mut node = element(
            source,
            "style",
            ByteRange {
                start: style.start as usize,
                end: style.end as usize,
            },
            Vec::new(),
        );
        node.kind = "style";
        nodes.push(node);
    }
    nodes.sort_by_key(|n| n.span.start);
    Ok(nodes)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pairs_follow_parser_not_tag_looking_strings() {
        let source =
            "<script>let text = '</div>';</script>\n<div title={'</div>'}><span>😀</span></div>";
        let profile = ts_svelte::compatibility::resolve(&Default::default()).unwrap();
        let nodes = facts(source, &profile).unwrap();
        let div = nodes.iter().find(|n| n.name == "div").unwrap();
        assert_eq!(
            &source[div.closing_name.unwrap().start..div.closing_name.unwrap().end],
            "div"
        );
        assert_eq!(
            div.closing_name.unwrap().start,
            source.rfind("</div>").unwrap() + 2
        );
        assert_eq!(div.children[0].name, "span");
        assert!(div.children[0].closing_name.is_some());
    }
    #[test]
    fn implicit_and_self_closing_tags_never_acquire_a_closing_pair() {
        let profile = ts_svelte::compatibility::resolve(&Default::default()).unwrap();
        for source in ["<div/>", "<p>a<p>b"] {
            assert!(
                facts(source, &profile)
                    .unwrap()
                    .iter()
                    .all(|n| n.closing_name.is_none())
            );
        }
    }
}
