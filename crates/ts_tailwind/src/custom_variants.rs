//! Tailwind CSS v4.2.1 custom variants from `variants.ts::fromAst`,
//! `substituteAtSlot`, `substituteAtVariant` and `index.ts` registration (MIT,
//! copyright Tailwind Labs). Parent registry owns ordering and built-in variants.
use super::*;
use crate::candidate::parse_variant;
use std::collections::BTreeSet;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "camelCase")]
pub enum CustomVariantBody {
    /// Resolved selector list without the directive's outer parentheses.
    Selectors(Vec<String>),
    /// Directive body, preserving @slot and nested @variant until application.
    Template(Vec<AstNode>),
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CustomVariantDefinition {
    pub name: String,
    pub body: CustomVariantBody,
}
fn valid_name(name: &str) -> bool {
    let name = name.strip_prefix('@').unwrap_or(name);
    let bytes = name.as_bytes();
    bytes
        .first()
        .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && bytes
            .iter()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-'))
        && bytes.last().is_some_and(u8::is_ascii_alphanumeric)
}
fn bound_tree(nodes: &[AstNode]) -> Result<()> {
    let mut pending = nodes.iter().map(|n| (n, 0)).collect::<Vec<_>>();
    let mut count = 0;
    let mut bytes = 0;
    while let Some((node, depth)) = pending.pop() {
        count += 1;
        bytes += match &node.kind {
            NodeKind::Rule { selector, .. } => selector.len(),
            NodeKind::AtRule { name, params, .. } => name.len() + params.len(),
            NodeKind::Declaration {
                property, value, ..
            } => property.len() + value.as_ref().map_or(0, String::len),
            NodeKind::Comment { value } => value.len(),
            NodeKind::Context { context, .. } => context
                .iter()
                .map(|(k, v)| {
                    k.len()
                        + match v {
                            crate::ast::ContextValue::String(s) => s.len(),
                            crate::ast::ContextValue::Bool(_) => 1,
                        }
                })
                .sum(),
            NodeKind::AtRoot { .. } => 0,
        };
        if depth > 128 || count > 100_000 || bytes > 16 * 1024 * 1024 {
            return Err(invalid("Custom variant AST exceeds bounds"));
        }
        if let Some(children) = node.children() {
            pending.extend(children.iter().map(|n| (n, depth + 1)));
        }
    }
    Ok(())
}
fn template_metadata(
    nodes: &[AstNode],
    selectors: &mut Vec<String>,
    dependencies: &mut Vec<String>,
) {
    for node in nodes {
        match &node.kind {
            NodeKind::Rule { selector, .. } => selectors.push(selector.clone()),
            NodeKind::AtRule { name, params, .. } if name == "@variant" => {
                dependencies.push(params.clone())
            }
            NodeKind::AtRule { name, params, .. } if name != "@slot" => {
                selectors.push(format!("{name} {params}"))
            }
            _ => {}
        }
        if let Some(children) = node.children() {
            template_metadata(children, selectors, dependencies);
        }
    }
}
impl Variants {
    pub(super) fn contains_custom_template(&self, variant: &Variant) -> bool {
        match variant {
            Variant::Static { root } => self
                .definitions
                .get(root)
                .is_some_and(|d| matches!(d.generator, Generator::Template(_))),
            Variant::Compound { variant, .. } => self.contains_custom_template(variant),
            _ => false,
        }
    }
    pub(super) fn apply_custom_compound(
        &self,
        name: &str,
        modifier: Option<&Modifier>,
        child: &Variant,
        body: Vec<AstNode>,
        depth: usize,
    ) -> Result<Vec<AstNode>> {
        let compounds = root(child)
            .and_then(|name| self.metadata(name))
            .map(|m| m.compounds)
            .unwrap_or(Compounds::NEVER);
        let with = self
            .metadata(name)
            .map(|m| m.compounds_with)
            .unwrap_or(Compounds::NEVER);
        if compounds.0 & with.0 == 0 {
            return Err(invalid("Variant cannot compound with this child"));
        }
        let slot = AstNode::new(NodeKind::AtRule {
            name: "@slot".into(),
            params: String::new(),
            nodes: vec![],
        });
        let mut isolated = self.apply_inner(child, vec![slot], depth)?;
        fn transform(
            registry: &Variants,
            nodes: &mut [AstNode],
            name: &str,
            modifier: Option<&Modifier>,
            in_style: bool,
            found: &mut bool,
        ) -> Result<()> {
            for node in nodes {
                let mut style = in_style;
                if let NodeKind::Rule { selector, .. } = &mut node.kind {
                    if in_style {
                        return Err(invalid(
                            "Compound variant cannot contain nested style rules",
                        ));
                    }
                    let transformed = registry.compound_branches(
                        name,
                        modifier,
                        vec![Branch::single(Wrapper::Style(selector.clone()))],
                    )?;
                    let Wrapper::Style(result) = &transformed[0].wrappers[0] else {
                        unreachable!()
                    };
                    *selector = result.clone();
                    *found = true;
                    style = true;
                }
                if let Some(children) = node.children_mut() {
                    transform(registry, children, name, modifier, style, found)?;
                }
            }
            Ok(())
        }
        // Upstream applies the compound independently to every isolated root.
        for node in &mut isolated {
            let mut found = false;
            transform(
                self,
                std::slice::from_mut(node),
                name,
                modifier,
                false,
                &mut found,
            )?;
            if !found {
                return Err(invalid("Compound variant requires a style selector"));
            }
        }
        self.apply_custom_template(&isolated, body, depth + 1)
    }
    /// Register a complete directive batch. Forward references are resolved only
    /// after every final definition is installed; a failure leaves self unchanged.
    /// Overwrites retain their original position, including overwritten builtins.
    pub fn register_custom_variants(
        &mut self,
        definitions: &[CustomVariantDefinition],
    ) -> Result<()> {
        if definitions.len() > 4096 {
            return Err(invalid("Custom variant registration limit exceeded"));
        }
        let mut next = self.clone();
        for definition in definitions {
            if !valid_name(&definition.name) {
                return Err(invalid(format!(
                    "Invalid custom variant name {}",
                    definition.name
                )));
            }
            let (generator, selectors) = match &definition.body {
                CustomVariantBody::Selectors(selectors) => {
                    if selectors.is_empty() || selectors.len() > 4096 {
                        return Err(invalid(
                            "Custom variant requires a bounded nonempty selector list",
                        ));
                    }
                    let mut styles = vec![];
                    let mut ats = vec![];
                    for selector in selectors {
                        let selector = selector.trim();
                        if selector.is_empty() || selector.len() > 64 * 1024 {
                            return Err(invalid(
                                "Custom variant selector is empty or exceeds bounds",
                            ));
                        }
                        if selector.starts_with('@') {
                            ats.push(selector.to_string());
                        } else {
                            styles.push(selector.to_string());
                        }
                    }
                    let metadata = styles.iter().chain(ats.iter()).cloned().collect::<Vec<_>>();
                    if !styles.is_empty() {
                        ats.insert(0, styles.join(", "));
                    }
                    (Generator::Selectors(ats), metadata)
                }
                CustomVariantBody::Template(nodes) => {
                    if nodes.is_empty() {
                        return Err(invalid("Custom variant has no selector or body"));
                    }
                    bound_tree(nodes)?;
                    let mut selectors = vec![];
                    template_metadata(nodes, &mut selectors, &mut vec![]);
                    (Generator::Template(nodes.clone()), selectors)
                }
            };
            next.add(
                &definition.name,
                VariantKind::Static,
                generator,
                compounds_for_selectors(&selectors.iter().map(String::as_str).collect::<Vec<_>>()),
                Compounds::NEVER,
                None,
            );
        }
        next.validate_custom_dependencies()?;
        *self = next;
        Ok(())
    }
    pub fn register_custom_selectors(&mut self, name: &str, selectors: Vec<String>) -> Result<()> {
        self.register_custom_variants(&[CustomVariantDefinition {
            name: name.into(),
            body: CustomVariantBody::Selectors(selectors),
        }])
    }
    pub fn register_custom_template(&mut self, name: &str, template: Vec<AstNode>) -> Result<()> {
        self.register_custom_variants(&[CustomVariantDefinition {
            name: name.into(),
            body: CustomVariantBody::Template(template),
        }])
    }
    fn parse_custom_reference(&self, text: &str) -> Result<Variant> {
        let mut registry = CandidateRegistry::default();
        self.register_candidates(&mut registry);
        parse_variant(text, &registry)
            .ok_or_else(|| invalid(format!("Cannot use @variant with unknown variant: {text}")))
    }
    fn validate_custom_dependencies(&self) -> Result<()> {
        fn roots(variant: &Variant, result: &mut Vec<String>) {
            if let Some(name) = root(variant) {
                result.push(name.into());
            }
            if let Variant::Compound { variant, .. } = variant {
                roots(variant, result);
            }
        }
        let mut edges = BTreeMap::<String, Vec<String>>::new();
        for name in &self.insertion {
            let Generator::Template(nodes) = &self.definitions[name].generator else {
                continue;
            };
            let mut dependencies = vec![];
            template_metadata(nodes, &mut vec![], &mut dependencies);
            let mut names = vec![];
            for dependency in dependencies {
                roots(&self.parse_custom_reference(&dependency)?, &mut names);
            }
            edges.insert(name.clone(), names);
        }
        fn visit(
            name: &str,
            edges: &BTreeMap<String, Vec<String>>,
            done: &mut BTreeSet<String>,
            path: &mut Vec<String>,
        ) -> Result<()> {
            if done.contains(name) {
                return Ok(());
            }
            if path.iter().any(|n| n == name) {
                path.push(name.into());
                return Err(invalid(format!(
                    "Circular dependency detected in custom variants: {}",
                    path.join(" -> ")
                )));
            }
            if path.len() > 128 {
                return Err(invalid("Custom variant dependency depth exceeded"));
            }
            path.push(name.into());
            if let Some(children) = edges.get(name) {
                for child in children {
                    visit(child, edges, done, path)?;
                }
            }
            path.pop();
            done.insert(name.into());
            Ok(())
        }
        let mut done = BTreeSet::new();
        for name in &self.insertion {
            visit(name, &edges, &mut done, &mut vec![])?;
        }
        Ok(())
    }
    /// Lower directive uses into actual `&` rule nodes. Slot substitution belongs
    /// to custom-template application, so bare @slot stays intact here.
    pub fn substitute_at_variants(&self, nodes: &[AstNode]) -> Result<Vec<AstNode>> {
        bound_tree(nodes)?;
        self.substitute_variants_inner(nodes.to_vec(), 0)
    }
    fn substitute_variants_inner(&self, nodes: Vec<AstNode>, depth: usize) -> Result<Vec<AstNode>> {
        if depth > 128 {
            return Err(invalid("Nested @variant expansion limit exceeded"));
        }
        let mut output = vec![];
        for mut node in nodes {
            if let NodeKind::AtRule {
                name,
                params,
                nodes,
            } = &node.kind
            {
                if name == "@variant" {
                    let variant = self.parse_custom_reference(params)?;
                    let result = self.apply_inner(&variant, nodes.clone(), depth + 1)?;
                    let result = self.substitute_variants_inner(result, depth + 1)?;
                    output.push(AstNode::new(NodeKind::Rule {
                        selector: "&".into(),
                        nodes: result,
                    }));
                    continue;
                }
            }
            if let Some(children) = node.children_mut() {
                *children = self.substitute_variants_inner(std::mem::take(children), depth + 1)?;
            }
            output.push(node);
        }
        bound_tree(&output)?;
        Ok(output)
    }
    pub(super) fn apply_custom_template(
        &self,
        template: &[AstNode],
        body: Vec<AstNode>,
        depth: usize,
    ) -> Result<Vec<AstNode>> {
        bound_tree(&body)?;
        let mut template = self.substitute_variants_inner(template.to_vec(), depth)?;
        // Account duplication before cloning potentially large utility bodies.
        fn counts(nodes: &[AstNode]) -> (usize, usize) {
            let mut count = 0;
            let mut slots = 0;
            for node in nodes {
                count += 1;
                if matches!(&node.kind, NodeKind::AtRule { name, .. } if name == "@slot") {
                    slots += 1;
                }
                if let Some(children) = node.children() {
                    let (n, s) = counts(children);
                    count += n;
                    slots += s;
                }
            }
            (count, slots)
        }
        let (template_count, slot_count) = counts(&template);
        let (body_count, _) = counts(&body);
        let body_bytes = serde_json::to_vec(&body)
            .map_err(|e| invalid(e.to_string()))?
            .len();
        if template_count.saturating_add(slot_count.saturating_mul(body_count)) > 100_000
            || slot_count.saturating_mul(body_bytes) > 16 * 1024 * 1024
        {
            return Err(invalid("Custom variant slot expansion exceeds bounds"));
        }
        fn slots(nodes: Vec<AstNode>, body: &[AstNode]) -> Vec<AstNode> {
            let mut result = vec![];
            for mut node in nodes {
                if let NodeKind::AtRule {
                    name,
                    params,
                    nodes,
                } = &node.kind
                {
                    if name == "@slot" {
                        result.extend_from_slice(body);
                        continue;
                    }
                    if name == "@keyframes" || name == "@property" {
                        // Object.assign upstream preserves provenance on the
                        // outer AtRoot, while constructing a fresh inner rule.
                        let inner = AstNode::new(NodeKind::AtRule {
                            name: name.clone(),
                            params: params.clone(),
                            nodes: nodes.clone(),
                        });
                        node.kind = NodeKind::AtRoot { nodes: vec![inner] };
                        result.push(node);
                        continue;
                    }
                }
                if let Some(children) = node.children_mut() {
                    *children = slots(std::mem::take(children), body);
                }
                result.push(node);
            }
            result
        }
        template = slots(template, &body);
        bound_tree(&template)?;
        Ok(template)
    }
    pub(super) fn custom_template_branches(
        &self,
        template: &[AstNode],
        depth: usize,
    ) -> Result<Vec<Branch>> {
        // Compounds act on an isolated child before inserting the utility body.
        // Pure selector/condition templates are exactly representable by the
        // existing branch algebra. Other IR must not be silently discarded.
        let nodes = self.substitute_variants_inner(template.to_vec(), depth)?;
        fn collect(
            nodes: &[AstNode],
            wrappers: &[Wrapper],
            output: &mut Vec<Branch>,
        ) -> Result<()> {
            for node in nodes {
                match &node.kind {
                    NodeKind::AtRule { name, .. } if name == "@slot" => output.push(Branch {
                        wrappers: wrappers.to_vec(),
                        before: vec![],
                    }),
                    NodeKind::Rule { selector, nodes } => {
                        let mut wrappers = wrappers.to_vec();
                        wrappers.push(Wrapper::Style(selector.clone()));
                        collect(nodes, &wrappers, output)?;
                    }
                    NodeKind::AtRule {
                        name,
                        params,
                        nodes,
                    } => {
                        let mut wrappers = wrappers.to_vec();
                        wrappers.push(Wrapper::At(name.clone(), params.clone()));
                        collect(nodes, &wrappers, output)?;
                    }
                    NodeKind::Context { nodes, context } if context.is_empty() => {
                        collect(nodes, wrappers, output)?
                    }
                    _ => {
                        return Err(unsupported(
                            "Compounding custom templates with declarations, context or hoisted rules requires a full AST compound transform",
                        ));
                    }
                }
            }
            Ok(())
        }
        let mut output = vec![];
        collect(&nodes, &[], &mut output)?;
        if output.is_empty() {
            return Err(invalid("Compound custom variant has no applicable @slot"));
        }
        Ok(output)
    }
}

#[cfg(test)]
#[path = "custom_variant_tests.rs"]
mod tests;
