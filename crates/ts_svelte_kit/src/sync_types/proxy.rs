//! AST-based counterpart of Kit's `tweak_types`. It removes contextual return
//! widening while retaining contextual event parameter types for inference.
use std::collections::BTreeSet;

use crate::sync_versions::KitSyncVersion;
use oxc_allocator::Allocator;
use oxc_ast::ast::{
    BindingPattern, Declaration, Expression, FormalParameter, ObjectPropertyKind, PropertyKey,
    Statement, VariableDeclaration,
};
use oxc_parser::Parser;
use oxc_span::{GetSpan, SourceType};

#[derive(Debug, Clone)]
pub(super) struct Proxy {
    pub modified: bool,
    pub code: String,
    pub exports: BTreeSet<String>,
    pub trailing_slash: Option<String>,
    pub options_unknown: bool,
    pub star_exports: Vec<String>,
}

struct Edit {
    start: usize,
    end: usize,
    text: String,
}

struct Rewriter<'a> {
    source: &'a str,
    comments: Vec<(usize, usize)>,
    edits: Vec<Edit>,
    suffix: String,
}

fn parameter<'a>(expression: &'a Expression<'a>) -> Option<&'a FormalParameter<'a>> {
    match expression {
        Expression::ArrowFunctionExpression(function) => function.params.items.first(),
        Expression::FunctionExpression(function) => function.params.items.first(),
        _ => None,
    }
}

fn binding_name<'a>(pattern: &'a BindingPattern<'a>) -> Option<&'a str> {
    if let BindingPattern::BindingIdentifier(name) = pattern {
        Some(name.name.as_str())
    } else {
        None
    }
}

fn binding_exports(pattern: &BindingPattern<'_>, exports: &mut BTreeSet<String>, server: bool) {
    match pattern {
        BindingPattern::BindingIdentifier(name) => {
            if name.name == "load" || (server && name.name == "actions") {
                exports.insert(name.name.to_string());
            }
        }
        BindingPattern::ObjectPattern(object) => {
            for property in &object.properties {
                binding_exports(&property.value, exports, server);
            }
            if let Some(rest) = &object.rest {
                binding_exports(&rest.argument, exports, server);
            }
        }
        BindingPattern::ArrayPattern(array) => {
            for item in array.elements.iter().flatten() {
                binding_exports(item, exports, server);
            }
            if let Some(rest) = &array.rest {
                binding_exports(&rest.argument, exports, server);
            }
        }
        BindingPattern::AssignmentPattern(assignment) => {
            binding_exports(&assignment.left, exports, server)
        }
    }
}

impl Rewriter<'_> {
    fn replace(&mut self, start: usize, end: usize, text: impl Into<String>) {
        self.edits.push(Edit {
            start,
            end,
            text: text.into(),
        });
    }

    fn type_parameter(&mut self, expression: &Expression<'_>, ty: &str) -> bool {
        let Some(arg) = parameter(expression) else {
            return false;
        };
        if arg.type_annotation.is_some() {
            return false;
        }
        let span = arg.pattern.span();
        let start = span.start as usize;
        let end = span.end as usize + usize::from(arg.optional);
        // An arrow's single unparenthesized binding needs parentheses. The AST
        // parameter span starts at its identifier, not the async keyword.
        let mut before = self.source[..start].trim_end().len();
        while let Some(&(a, _)) = self.comments.iter().rev().find(|&&(_, b)| b == before) {
            before = self.source[..a].trim_end().len();
        }
        let parenthesized = self.source[..before].ends_with('(');
        let arrow = matches!(expression, Expression::ArrowFunctionExpression(_));
        if arrow && !parenthesized {
            self.replace(start, start, "(");
        }
        self.replace(
            end,
            end,
            format!(": {ty}{}", if arrow && !parenthesized { ")" } else { "" }),
        );
        true
    }

    /// Only JSDoc comments attached immediately before this declaration are
    /// eligible. Strings/templates and unrelated comments are never searched.
    fn jsdoc(
        &mut self,
        start: usize,
        expression: Option<&Expression<'_>>,
        function_arg: Option<&FormalParameter<'_>>,
    ) -> Result<bool, String> {
        let mut eligible = Vec::new();
        let mut cursor = start;
        for &(a, b) in self.comments.iter().rev() {
            if b > cursor {
                continue;
            }
            if !self.source[b..cursor].trim().is_empty() {
                break;
            }
            eligible.push((a, b));
            cursor = a;
        }
        let mut modified = false;
        for (a, b) in eligible {
            let comment = &self.source[a..b];
            if !comment.starts_with("/**") {
                continue;
            }
            let mut cursor = 0;
            while let Some(at) = comment[cursor..].find("@type") {
                let tag = cursor + at;
                cursor = tag + 5;
                if comment
                    .as_bytes()
                    .get(cursor)
                    .is_some_and(|c| c.is_ascii_alphanumeric() || *c == b'_')
                {
                    continue;
                }
                let after = comment[cursor..].trim_start();
                if !after.starts_with('{') {
                    return Err("JSDoc @type without a braced type expression".into());
                }
                let open = comment.len() - after.len();
                let mut nesting = 0;
                let mut close = None;
                let mut quote = None;
                let mut escaped = false;
                for (offset, ch) in comment[open..].char_indices() {
                    if let Some(q) = quote {
                        if escaped {
                            escaped = false;
                        } else if ch == '\\' {
                            escaped = true;
                        } else if ch == q {
                            quote = None;
                        }
                        continue;
                    }
                    if ch == '\'' || ch == '"' || ch == '`' {
                        quote = Some(ch);
                        continue;
                    }
                    if ch == '{' {
                        nesting += 1;
                    }
                    if ch == '}' {
                        nesting -= 1;
                        if nesting == 0 {
                            close = Some(open + offset);
                            break;
                        }
                    }
                }
                let close = close.ok_or("Unclosed JSDoc @type expression")?;
                let ty = &comment[open + 1..close];
                let arg = expression.and_then(parameter).or(function_arg);
                if let Some(arg) = arg {
                    let name = binding_name(&arg.pattern).unwrap_or("event");
                    self.replace(
                        a + tag,
                        a + close + 1,
                        format!("@param {{Parameters<{ty}>[0]}} {name}"),
                    );
                } else {
                    self.replace(a + tag, a + close + 1, "");
                }
                cursor = close + 1;
                modified = true;
            }
        }
        Ok(modified)
    }

    fn actions(
        &mut self,
        expression: &Expression<'_>,
        jsdoc: bool,
        ts: bool,
    ) -> Result<(), String> {
        let Expression::ObjectExpression(object) = expression else {
            return Ok(());
        };
        for property in &object.properties {
            let ObjectPropertyKind::ObjectProperty(property) = property else {
                continue;
            };
            if property.method
                || property.shorthand
                || !matches!(property.key, PropertyKey::StaticIdentifier(_))
            {
                continue;
            }
            if jsdoc {
                let changed =
                    self.jsdoc(property.span.start as usize, Some(&property.value), None)?;
                if !changed && let Some(arg) = parameter(&property.value) {
                    let name = binding_name(&arg.pattern).unwrap_or("event");
                    let start = property.value.span().start as usize;
                    self.replace(
                        start,
                        start,
                        format!("/** @param {{import('./$types').RequestEvent}} {name} */ "),
                    );
                }
            }
            if ts {
                self.type_parameter(&property.value, "import('./$types').RequestEvent");
            }
        }
        Ok(())
    }

    fn variables(
        &mut self,
        declaration: &VariableDeclaration<'_>,
        statement_start: usize,
        server: bool,
    ) -> Result<(), String> {
        for (index, variable) in declaration.declarations.iter().enumerate() {
            let Some(name) = binding_name(&variable.id) else {
                continue;
            };
            if name != "load" && !(server && name == "actions") {
                continue;
            }
            let Some(expression) = &variable.init else {
                continue;
            };
            let mut jsdoc = false;
            if index == 0 {
                jsdoc = self.jsdoc(statement_start, Some(expression), None)?;
            }
            if variable.span.start as usize != statement_start {
                jsdoc |= self.jsdoc(variable.span.start as usize, Some(expression), None)?;
            }
            let mut typed = false;
            if let Some(annotation) = &variable.type_annotation {
                let ty = annotation.type_annotation.span();
                let ty = self.source[ty.start as usize..ty.end as usize].to_owned();
                self.replace(
                    variable.id.span().end as usize,
                    annotation.span.end as usize,
                    "",
                );
                typed = true;
                if name == "actions"
                    || !self.type_parameter(expression, &format!("Parameters<{ty}>[0]"))
                {
                    self.suffix.push_str(&format!(";null as any as {ty};"));
                }
            }
            if name == "actions" {
                self.actions(expression, jsdoc, typed)?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
pub(super) fn transform(source: &str, server: bool) -> Result<Proxy, String> {
    transform_for_version(source, server, KitSyncVersion::default())
}

pub(super) fn transform_for_version(
    source: &str,
    server: bool,
    version: KitSyncVersion,
) -> Result<Proxy, String> {
    let allocator = Allocator::default();
    // Match Kit's ScriptKind.TS: it parses JS JSDoc and TS syntax identically.
    let parsed = Parser::new(&allocator, source, SourceType::ts()).parse();
    if !parsed.diagnostics.is_empty() {
        return Err(format!(
            "Cannot parse route module for type inference: {}",
            parsed.diagnostics[0]
        ));
    }
    let mut rewriter = Rewriter {
        source,
        comments: parsed
            .program
            .comments
            .iter()
            .map(|c| (c.span.start as usize, c.span.end as usize))
            .collect(),
        edits: Vec::new(),
        suffix: String::new(),
    };
    let mut exports = BTreeSet::new();
    let mut trailing_slash = None;
    let mut options_unknown = false;
    let mut star_exports = Vec::new();
    let relevant = |name: &str| name == "load" || (server && name == "actions");
    for statement in &parsed.program.body {
        match statement {
            Statement::ExportDeclaration(export) => match &export.declaration {
                Declaration::VariableDeclaration(declaration) => {
                    for variable in &declaration.declarations {
                        if version.destructured_exports() {
                            binding_exports(&variable.id, &mut exports, server);
                        }
                        if let Some(name) = binding_name(&variable.id)
                            && relevant(name)
                        {
                            exports.insert(name.to_owned());
                        }
                        if let Some(name) = binding_name(&variable.id) {
                            if name == "trailingSlash" {
                                if let Some(Expression::StringLiteral(value)) = &variable.init {
                                    if !["always", "never", "ignore"]
                                        .contains(&value.value.as_str())
                                    {
                                        return Err("Invalid trailingSlash value".into());
                                    }
                                    trailing_slash = Some(value.value.to_string());
                                } else {
                                    options_unknown = true;
                                }
                            } else if ["ssr", "csr", "prerender"].contains(&name)
                                && !matches!(
                                    variable.init,
                                    Some(
                                        Expression::BooleanLiteral(_)
                                            | Expression::StringLiteral(_)
                                    )
                                )
                            {
                                options_unknown = true;
                            }
                        }
                    }
                    rewriter.variables(declaration, export.span.start as usize, server)?;
                }
                Declaration::FunctionDeclaration(function) => {
                    if let Some(id) = &function.id
                        && relevant(id.name.as_str())
                    {
                        exports.insert(id.name.to_string());
                        if id.name == "load" {
                            rewriter.jsdoc(
                                export.span.start as usize,
                                None,
                                function.params.items.first(),
                            )?;
                        }
                    }
                }
                _ => {}
            },
            Statement::ExportNamedDeclaration(export) => {
                for specifier in &export.specifiers {
                    let name = specifier.exported.name();
                    if relevant(name.as_str()) {
                        exports.insert(name.to_string());
                    }
                    if ["trailingSlash", "ssr", "csr", "prerender"].contains(&name.as_str()) {
                        options_unknown = true;
                    }
                }
            }
            Statement::ExportFromDeclaration(export) => {
                for specifier in &export.specifiers {
                    let name = specifier.exported.name();
                    if relevant(name.as_str()) {
                        exports.insert(name.to_string());
                    }
                    if ["trailingSlash", "ssr", "csr", "prerender", "load"].contains(&name.as_str())
                    {
                        options_unknown = true;
                    }
                }
            }
            Statement::ExportAllDeclaration(export) => {
                if !export.export_kind.is_type() && export.exported.is_none() {
                    star_exports.push(export.source.value.to_string());
                    options_unknown = true;
                }
            }
            Statement::VariableDeclaration(declaration) => {
                rewriter.variables(declaration, declaration.span.start as usize, server)?;
            }
            Statement::FunctionDeclaration(function) => {
                if function.id.as_ref().is_some_and(|id| id.name == "load") {
                    rewriter.jsdoc(
                        function.span.start as usize,
                        None,
                        function.params.items.first(),
                    )?;
                }
            }
            _ => {}
        }
    }
    let modified = !rewriter.edits.is_empty();
    if !modified {
        return Ok(Proxy {
            modified,
            code: source.to_owned(),
            exports,
            trailing_slash,
            options_unknown,
            star_exports,
        });
    }
    rewriter.edits.sort_by_key(|edit| (edit.start, edit.end));
    let mut code = String::with_capacity(source.len() + 256);
    let mut cursor = 0;
    for edit in rewriter.edits {
        if edit.start < cursor {
            return Err("Overlapping route proxy edits".into());
        }
        code.push_str(&source[cursor..edit.start]);
        code.push_str(&edit.text);
        cursor = edit.end;
    }
    code.push_str(&source[cursor..]);
    code.push_str(&rewriter.suffix);
    if code.starts_with("// @ts-check") {
        code.insert_str("// @ts-check".len(), "\n// @ts-nocheck\n");
    } else {
        code.insert_str(0, "// @ts-nocheck\n");
    }
    Ok(Proxy {
        modified,
        code,
        exports,
        trailing_slash,
        options_unknown,
        star_exports,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn annotated_load_keeps_event_type_and_infers_return() {
        let proxy = transform("import type { PageServerLoad } from './$types'; export const load: PageServerLoad = async ({ locals }) => ({ user: locals.user });", true).unwrap();
        assert!(proxy.exports.contains("load"));
        assert!(
            proxy
                .code
                .contains("({ locals }: Parameters<PageServerLoad>[0])")
        );
        assert!(!proxy.code.contains("load: PageServerLoad"));
    }
    #[test]
    fn single_arrow_and_already_typed_parameter() {
        let proxy = transform(
            "export const load: Load = event => ({ path: event.url.pathname });",
            false,
        )
        .unwrap();
        assert!(proxy.code.contains("(event: Parameters<Load>[0])"));
        let proxy = transform(
            "export const load: Load = (event: Event) => ({ event });",
            false,
        )
        .unwrap();
        assert!(proxy.code.contains("event: Event"));
        assert!(proxy.code.ends_with(";null as any as Load;"));
        let proxy = transform(
            "export const load: Load = (/* event */ event) => ({ event });",
            false,
        )
        .unwrap();
        assert!(
            proxy
                .code
                .contains("(/* event */ event: Parameters<Load>[0])")
        );
    }
    #[test]
    fn typed_actions_keep_individual_return_shapes() {
        let proxy = transform("export const actions: Actions = { save: async ({ request }) => ({ saved: true }), remove: event => ({ removed: event.params.id }) };", true).unwrap();
        assert!(proxy.exports.contains("actions"));
        assert!(
            proxy
                .code
                .contains("({ request }: import('./$types').RequestEvent)")
        );
        assert!(
            proxy
                .code
                .contains("(event: import('./$types').RequestEvent)")
        );
    }
    #[test]
    fn jsdoc_type_is_relocated_without_touching_strings() {
        let proxy = transform("const text = '/** @type {Wrong} */';\n/** @type {import('./$types').PageLoad} */\nexport const load = ({ params }) => ({ id: params.id });", false).unwrap();
        assert!(
            proxy
                .code
                .contains("@param {Parameters<import('./$types').PageLoad>[0]} event")
        );
        assert!(proxy.code.contains("const text = '/** @type {Wrong} */'"));
    }
    #[test]
    fn satisfies_and_nonexported_names_do_not_fake_exports() {
        let proxy = transform(
            "const actions = {}; export const load = (async () => ({ count: 1 })) satisfies Load;",
            true,
        )
        .unwrap();
        assert!(!proxy.modified);
        assert_eq!(proxy.exports.into_iter().collect::<Vec<_>>(), vec!["load"]);
        assert_eq!(
            transform("export * from './other';", false)
                .unwrap()
                .star_exports,
            vec!["./other"]
        );
    }
}
