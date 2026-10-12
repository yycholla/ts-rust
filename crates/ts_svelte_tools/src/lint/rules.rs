use super::config::{Config, canonical};
use oxc_ast::ast::*;
use oxc_ast_visit::{Visit, walk};
use oxc_semantic::{IsGlobalReference, Scoping};
use oxc_span::{GetSpan, Span};
use oxc_syntax::operator::{BinaryOperator, UnaryOperator};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use ts_svelte::{ByteRange, Document};

pub(super) struct Rules<'d> {
    pub document: &'d Document,
    pub source: &'d str,
    pub offset: usize,
    pub config: &'d Config,
    pub scoping: &'d Scoping,
    pub module_bindings: &'d BTreeSet<String>,
    pub module_type_bindings: &'d BTreeSet<String>,
    pub comments: &'d [Comment],
    pub svelte: bool,
    pub diagnostics: Vec<Value>,
}

impl Rules<'_> {
    fn option(&self, rule: &str, name: &str, fallback: bool) -> bool {
        self.config
            .rules
            .get(rule)
            .is_some_and(|setting| setting.bool_option(name, fallback))
    }

    fn global(&self, expression: &Expression<'_>, name: &str) -> bool {
        match expression.get_inner_expression() {
            Expression::Identifier(id) => {
                id.name == name
                    && !self.module_bindings.contains(name)
                    && id.is_global_reference(self.scoping)
            }
            _ => false,
        }
    }

    fn disabled(&self, rule: &str, span: Span) -> bool {
        let line = self
            .document
            .position_at(self.offset + span.start as usize)
            .ok()
            .map(|p| p.line);
        let mut disabled = false;
        for comment in self.comments {
            let raw = &self.source
                [comment.content_span().start as usize..comment.content_span().end as usize];
            let text = raw.trim().trim_start_matches('*').trim();
            let directive = [
                "eslint-disable-next-line",
                "eslint-disable-line",
                "eslint-disable",
                "eslint-enable",
            ]
            .into_iter()
            .find(|prefix| {
                text.strip_prefix(prefix)
                    .is_some_and(|tail| tail.is_empty() || tail.starts_with(char::is_whitespace))
            });
            let Some(directive) = directive else {
                continue;
            };
            let list = text[directive.len()..]
                .split("--")
                .next()
                .unwrap_or("")
                .trim();
            let applies = list.is_empty()
                || list
                    .split(',')
                    .map(str::trim)
                    .any(|name| canonical(name) == rule);
            if !applies {
                continue;
            }
            let comment_line = self
                .document
                .position_at(self.offset + comment.span.start as usize)
                .ok()
                .map(|p| p.line);
            match directive {
                "eslint-disable-next-line"
                    if comment_line.and_then(|line| line.checked_add(1)) == line =>
                {
                    return true;
                }
                "eslint-disable-line" if comment_line == line => return true,
                "eslint-disable" if comment.span.end <= span.start => disabled = true,
                "eslint-enable" if comment.span.end <= span.start => disabled = false,
                _ => {}
            }
        }
        disabled
    }

    fn report(&mut self, rule: &str, message: &str, span: Span) {
        let Some(setting) = self.config.rules.get(rule) else {
            return;
        };
        if self.disabled(rule, span) {
            return;
        }
        let bytes = ByteRange {
            start: self.offset + span.start as usize,
            end: self.offset + span.end as usize,
        };
        self.diagnostics.push(json!({"code":setting.code,"message":message,"severity":setting.severity,"uri":self.document.snapshot().uri,"bytes":bytes,"range":self.document.editor_range(bytes).ok()}));
    }

    fn console(&mut self, object: &Expression<'_>, property: Option<&str>, span: Span) {
        if self.global(object, "console")
            && self.config.rules.get("no-console").is_some_and(|setting| {
                !property.is_some_and(|name| setting.strings("allow").contains(&name))
            })
        {
            self.report("no-console", "Unexpected console statement.", span);
        }
    }

    fn negative_zero(expression: &Expression<'_>) -> bool {
        matches!(expression.get_inner_expression(), Expression::UnaryExpression(unary) if unary.operator == UnaryOperator::UnaryNegation && matches!(unary.argument.get_inner_expression(), Expression::NumericLiteral(number) if number.value == 0.0))
    }

    fn nan(&self, expression: &Expression<'_>) -> bool {
        self.global(expression, "NaN")
            || matches!(expression.get_inner_expression(), Expression::StaticMemberExpression(member) if member.property.name == "NaN" && self.global(&member.object, "Number"))
            || matches!(expression.get_inner_expression(), Expression::ComputedMemberExpression(member) if matches!(member.expression.get_inner_expression(), Expression::StringLiteral(value) if value.value == "NaN") && self.global(&member.object, "Number"))
    }

    fn literal_type(expression: &Expression<'_>) -> Option<u8> {
        match expression.get_inner_expression() {
            Expression::StringLiteral(_) => Some(0),
            Expression::NumericLiteral(_) => Some(1),
            Expression::BooleanLiteral(_) => Some(2),
            Expression::BigIntLiteral(_) => Some(3),
            Expression::NullLiteral(_) => Some(4),
            _ => None,
        }
    }

    fn equality(&mut self, node: &BinaryExpression<'_>) {
        let Some(setting) = self.config.rules.get("eqeqeq") else {
            return;
        };
        if !node.operator.is_equality() {
            return;
        }
        let loose = matches!(
            node.operator,
            BinaryOperator::Equality | BinaryOperator::Inequality
        );
        let null = matches!(node.left.get_inner_expression(), Expression::NullLiteral(_))
            || matches!(
                node.right.get_inner_expression(),
                Expression::NullLiteral(_)
            );
        let mode = setting
            .options
            .first()
            .and_then(Value::as_str)
            .unwrap_or("always");
        let null_mode = setting
            .options
            .get(1)
            .and_then(|v| v.get("null"))
            .and_then(Value::as_str)
            .unwrap_or("always");
        if null && (mode == "allow-null" || mode == "smart" || null_mode == "ignore") {
            return;
        }
        if null && null_mode == "never" {
            if !loose {
                self.report(
                    "eqeqeq",
                    "Use loose equality when comparing against null.",
                    node.span,
                );
            }
            return;
        }
        if !loose {
            return;
        }
        if mode == "smart" {
            let typeof_operand = |expression: &Expression<'_>| matches!(expression.get_inner_expression(), Expression::UnaryExpression(unary) if unary.operator == UnaryOperator::Typeof);
            if typeof_operand(&node.left)
                || typeof_operand(&node.right)
                || Self::literal_type(&node.left)
                    .is_some_and(|kind| Self::literal_type(&node.right) == Some(kind))
            {
                return;
            }
        }
        self.report("eqeqeq", "Use strict equality or inequality.", node.span);
    }

    fn typeof_comparison(&mut self, value: &Expression<'_>, other: &Expression<'_>) {
        if !matches!(value.get_inner_expression(), Expression::UnaryExpression(unary) if unary.operator == UnaryOperator::Typeof)
        {
            return;
        }
        let valid = [
            "undefined",
            "object",
            "boolean",
            "number",
            "string",
            "function",
            "symbol",
            "bigint",
        ];
        if let Expression::StringLiteral(literal) = other.get_inner_expression() {
            if !valid.contains(&literal.value.as_str()) {
                self.report(
                    "valid-typeof",
                    "Invalid typeof comparison value.",
                    literal.span,
                );
            }
        } else if let Expression::TemplateLiteral(template) = other.get_inner_expression()
            && template.expressions.is_empty()
        {
            if !template
                .quasis
                .first()
                .and_then(|element| element.value.cooked.as_ref())
                .is_some_and(|value| valid.contains(&value.as_str()))
            {
                self.report(
                    "valid-typeof",
                    "Invalid typeof comparison value.",
                    template.span,
                );
            }
        } else if matches!(
            other.get_inner_expression(),
            Expression::NumericLiteral(_)
                | Expression::BooleanLiteral(_)
                | Expression::NullLiteral(_)
                | Expression::BigIntLiteral(_)
                | Expression::RegExpLiteral(_)
        ) || self.global(other, "undefined")
        {
            self.report(
                "valid-typeof",
                "Invalid typeof comparison value.",
                other.span(),
            );
        } else if self.option("valid-typeof", "requireStringLiterals", false) {
            if !matches!(other.get_inner_expression(), Expression::UnaryExpression(unary) if unary.operator == UnaryOperator::Typeof)
            {
                self.report(
                    "valid-typeof",
                    "Typeof comparisons should be to string literals.",
                    other.span(),
                );
            }
        }
    }

    fn array_constructor(
        &mut self,
        callee: &Expression<'_>,
        arguments: &[Argument<'_>],
        typed: bool,
        span: Span,
    ) {
        if !typed
            && self.global(callee, "Array")
            && (arguments.len() != 1
                || matches!(arguments.first(), Some(Argument::SpreadElement(_))))
        {
            self.report(
                "no-array-constructor",
                "The array literal notation [] is preferable.",
                span,
            );
        }
    }

    fn wrapper(&mut self, name: &IdentifierReference<'_>) {
        if ["Boolean", "Number", "String", "Symbol", "BigInt", "Object"]
            .contains(&name.name.as_str())
            && name.is_global_reference(self.scoping)
            && !self.module_type_bindings.contains(name.name.as_str())
        {
            self.report("@typescript-eslint/no-wrapper-object-types", &format!("Prefer using the primitive '{}' as a type name, rather than the upper-cased '{}'.", name.name.to_lowercase(), name.name), name.span);
        }
    }

    fn internal_import(&mut self, value: &StringLiteral<'_>) {
        if value.value == "svelte/internal" || value.value.starts_with("svelte/internal/") {
            self.report(
                "svelte/no-svelte-internal",
                "Do not import from Svelte's internal modules.",
                value.span,
            );
        }
    }

    fn optional_non_null(&mut self, expression: &Expression<'_>, optional: bool) {
        if optional
            && let Expression::TSNonNullExpression(assertion) = expression.without_parentheses()
        {
            self.report(
                "@typescript-eslint/no-extra-non-null-assertion",
                "Forbidden extra non-null assertion.",
                assertion.span,
            );
        }
    }
}

impl<'a> Visit<'a> for Rules<'_> {
    fn visit_debugger_statement(&mut self, node: &DebuggerStatement) {
        self.report("no-debugger", "Unexpected debugger statement.", node.span);
    }

    fn visit_variable_declaration(&mut self, node: &VariableDeclaration<'a>) {
        if node.kind == VariableDeclarationKind::Var {
            self.report(
                "no-var",
                "Unexpected var, use let or const instead.",
                node.span,
            );
        }
        walk::walk_variable_declaration(self, node);
    }

    fn visit_variable_declarator(&mut self, node: &VariableDeclarator<'a>) {
        if node.init.as_ref().is_some_and(|init| {
            matches!(init.get_inner_expression(), Expression::ThisExpression(_))
        }) && let Some(setting) = self.config.rules.get("@typescript-eslint/no-this-alias")
        {
            let allowed = match &node.id {
                BindingPattern::BindingIdentifier(name) => setting
                    .strings("allowedNames")
                    .contains(&name.name.as_str()),
                _ => setting.bool_option("allowDestructuring", true),
            };
            if !allowed {
                self.report(
                    "@typescript-eslint/no-this-alias",
                    "Unexpected aliasing of 'this' to local variable.",
                    node.id.span(),
                );
            }
        }
        walk::walk_variable_declarator(self, node);
    }

    fn visit_static_member_expression(&mut self, node: &StaticMemberExpression<'a>) {
        self.optional_non_null(&node.object, node.optional);
        self.console(&node.object, Some(node.property.name.as_str()), node.span);
        walk::walk_static_member_expression(self, node);
    }

    fn visit_computed_member_expression(&mut self, node: &ComputedMemberExpression<'a>) {
        self.optional_non_null(&node.object, node.optional);
        let property = match node.expression.get_inner_expression() {
            Expression::StringLiteral(value) => Some(value.value.as_str()),
            _ => None,
        };
        self.console(&node.object, property, node.span);
        walk::walk_computed_member_expression(self, node);
    }

    fn visit_binary_expression(&mut self, node: &BinaryExpression<'a>) {
        self.equality(node);
        if node.operator.is_equality() || node.operator.is_compare() {
            if Self::negative_zero(&node.left) || Self::negative_zero(&node.right) {
                self.report(
                    "no-compare-neg-zero",
                    "Do not use the equality operator to compare against -0.",
                    node.span,
                );
            }
            if self.nan(&node.left) || self.nan(&node.right) {
                self.report(
                    "use-isnan",
                    "Use the isNaN function to compare with NaN.",
                    node.span,
                );
            }
        }
        if node.operator.is_equality() {
            self.typeof_comparison(&node.left, &node.right);
            self.typeof_comparison(&node.right, &node.left);
        }
        if matches!(
            node.operator,
            BinaryOperator::In | BinaryOperator::Instanceof
        ) || (node.operator.is_compare()
            && self.option("no-unsafe-negation", "enforceForOrderingRelations", false))
        {
            // Parentheses around !a deliberately opt into (!a) in object.
            if matches!(&node.left, Expression::UnaryExpression(unary) if unary.operator == UnaryOperator::LogicalNot)
            {
                self.report(
                    "no-unsafe-negation",
                    "Unexpected negating the left operand of a relational expression.",
                    node.left.span(),
                );
            }
        }
        walk::walk_binary_expression(self, node);
    }

    fn visit_new_expression(&mut self, node: &NewExpression<'a>) {
        self.array_constructor(
            &node.callee,
            &node.arguments,
            node.type_arguments.is_some(),
            node.span,
        );
        if self.global(&node.callee, "Promise")
            && let Some(argument) = node.arguments.first()
        {
            let asynchronous = match argument {
                Argument::ArrowFunctionExpression(function) => function.r#async,
                Argument::FunctionExpression(function) => function.r#async,
                _ => false,
            };
            if asynchronous {
                self.report(
                    "no-async-promise-executor",
                    "Promise executor functions should not be async.",
                    argument.span(),
                );
            }
        }
        walk::walk_new_expression(self, node);
    }

    fn visit_call_expression(&mut self, node: &CallExpression<'a>) {
        self.optional_non_null(&node.callee, node.optional);
        self.array_constructor(
            &node.callee,
            &node.arguments,
            node.type_arguments.is_some(),
            node.span,
        );
        if self.global(&node.callee, "require") {
            self.report(
                "@typescript-eslint/no-require-imports",
                "A require() style import is forbidden.",
                node.span,
            );
            if let Some(Argument::StringLiteral(value)) = node.arguments.first() {
                self.internal_import(value);
            }
        }
        if self.svelte
            && (self.global(&node.callee, "$inspect")
                || matches!(node.callee.get_inner_expression(), Expression::StaticMemberExpression(member) if member.property.name == "trace" && self.global(&member.object, "$inspect")))
        {
            self.report("svelte/no-inspect", "Unexpected $inspect call.", node.span);
        }
        if self.option("use-isnan", "enforceForIndexOf", false) {
            let member = match node.callee.get_inner_expression() {
                Expression::StaticMemberExpression(member) => Some(member.property.name.as_str()),
                _ => None,
            };
            if matches!(member, Some("indexOf" | "lastIndexOf"))
                && node
                    .arguments
                    .first()
                    .and_then(Argument::as_expression)
                    .is_some_and(|value| self.nan(value))
            {
                self.report(
                    "use-isnan",
                    "Array indexOf cannot find NaN; use a NaN-aware comparison.",
                    node.span,
                );
            }
        }
        walk::walk_call_expression(self, node);
    }

    fn visit_array_expression(&mut self, node: &ArrayExpression<'a>) {
        if node
            .elements
            .iter()
            .any(|element| matches!(element, ArrayExpressionElement::Elision(_)))
        {
            self.report(
                "no-sparse-arrays",
                "Unexpected comma in middle of array.",
                node.span,
            );
        }
        walk::walk_array_expression(self, node);
    }

    fn visit_object_pattern(&mut self, node: &ObjectPattern<'a>) {
        if node.properties.is_empty() && node.rest.is_none() {
            self.report(
                "no-empty-pattern",
                "Unexpected empty object pattern.",
                node.span,
            );
        }
        walk::walk_object_pattern(self, node);
    }

    fn visit_array_pattern(&mut self, node: &ArrayPattern<'a>) {
        if node.elements.is_empty() && node.rest.is_none() {
            self.report(
                "no-empty-pattern",
                "Unexpected empty array pattern.",
                node.span,
            );
        }
        walk::walk_array_pattern(self, node);
    }

    fn visit_static_block(&mut self, node: &StaticBlock<'a>) {
        if node.body.is_empty()
            && !self.comments.iter().any(|comment| {
                node.span.start < comment.span.start && comment.span.end < node.span.end
            })
        {
            self.report(
                "no-empty-static-block",
                "Unexpected empty static block.",
                node.span,
            );
        }
        walk::walk_static_block(self, node);
    }

    fn visit_switch_statement(&mut self, node: &SwitchStatement<'a>) {
        if self.option("use-isnan", "enforceForSwitchCase", true) && self.nan(&node.discriminant) {
            self.report(
                "use-isnan",
                "Use the isNaN function instead of a switch discriminant of NaN.",
                node.discriminant.span(),
            );
        }
        walk::walk_switch_statement(self, node);
    }

    fn visit_switch_case(&mut self, node: &SwitchCase<'a>) {
        if self.option("use-isnan", "enforceForSwitchCase", true)
            && node.test.as_ref().is_some_and(|test| self.nan(test))
        {
            self.report(
                "use-isnan",
                "Use the isNaN function to compare with NaN.",
                node.test.as_ref().expect("case test").span(),
            );
        }
        for statement in &node.consequent {
            let lexical = match statement {
                Statement::VariableDeclaration(declaration) => {
                    declaration.kind != VariableDeclarationKind::Var
                }
                Statement::FunctionDeclaration(_) | Statement::ClassDeclaration(_) => true,
                _ => false,
            };
            if lexical {
                self.report(
                    "no-case-declarations",
                    "Unexpected lexical declaration in case block.",
                    statement.span(),
                );
            }
        }
        walk::walk_switch_case(self, node);
    }

    fn visit_catch_clause(&mut self, node: &CatchClause<'a>) {
        if let Some(param) = &node.param
            && let BindingPattern::BindingIdentifier(binding) = &param.pattern
            && let [Statement::ThrowStatement(statement)] = node.body.body.as_slice()
            && let Expression::Identifier(thrown) = statement.argument.get_inner_expression()
            && thrown.name == binding.name
        {
            self.report("no-useless-catch", "Unnecessary catch clause.", node.span);
        }
        walk::walk_catch_clause(self, node);
    }

    fn visit_ts_any_keyword(&mut self, node: &TSAnyKeyword) {
        self.report(
            "@typescript-eslint/no-explicit-any",
            "Unexpected any. Specify a different type.",
            node.span,
        );
    }

    fn visit_ts_non_null_expression(&mut self, node: &TSNonNullExpression<'a>) {
        if matches!(
            node.expression.without_parentheses(),
            Expression::TSNonNullExpression(_)
        ) {
            self.report(
                "@typescript-eslint/no-extra-non-null-assertion",
                "Forbidden extra non-null assertion.",
                node.span,
            );
        }
        let optional = match node.expression.without_parentheses() {
            Expression::ChainExpression(_) => true,
            Expression::StaticMemberExpression(member) => member.optional,
            Expression::ComputedMemberExpression(member) => member.optional,
            Expression::CallExpression(call) => call.optional,
            _ => false,
        };
        if optional {
            self.report("@typescript-eslint/no-non-null-asserted-optional-chain", "Optional chain expressions can return undefined; using a non-null assertion is unsafe.", node.span);
        }
        walk::walk_ts_non_null_expression(self, node);
    }

    fn visit_ts_type_parameter(&mut self, node: &TSTypeParameter<'a>) {
        if let Some(constraint) = &node.constraint
            && matches!(
                constraint,
                TSType::TSAnyKeyword(_) | TSType::TSUnknownKeyword(_)
            )
        {
            self.report(
                "@typescript-eslint/no-unnecessary-type-constraint",
                "Constraining a generic type parameter to any or unknown is unnecessary.",
                constraint.span(),
            );
        }
        walk::walk_ts_type_parameter(self, node);
    }

    fn visit_ts_type_reference(&mut self, node: &TSTypeReference<'a>) {
        if let TSTypeName::IdentifierReference(name) = &node.type_name {
            self.wrapper(name);
        }
        walk::walk_ts_type_reference(self, node);
    }

    fn visit_ts_interface_heritage(&mut self, node: &TSInterfaceHeritage<'a>) {
        if let TSTypeName::IdentifierReference(name) = &node.type_name {
            self.wrapper(name);
        }
        walk::walk_ts_interface_heritage(self, node);
    }

    fn visit_ts_class_implements(&mut self, node: &TSClassImplements<'a>) {
        if let TSTypeName::IdentifierReference(name) = &node.expression {
            self.wrapper(name);
        }
        walk::walk_ts_class_implements(self, node);
    }

    fn visit_import_expression(&mut self, node: &ImportExpression<'a>) {
        if let Expression::StringLiteral(value) = node.source.get_inner_expression() {
            self.internal_import(value);
        }
        walk::walk_import_expression(self, node);
    }

    fn visit_ts_import_equals_declaration(&mut self, node: &TSImportEqualsDeclaration<'a>) {
        if !node.import_kind.is_type()
            && matches!(
                node.module_reference,
                TSModuleReference::ExternalModuleReference(_)
            )
            && !self.option(
                "@typescript-eslint/no-require-imports",
                "allowAsImport",
                false,
            )
        {
            self.report(
                "@typescript-eslint/no-require-imports",
                "A require() style import is forbidden.",
                node.span,
            );
        }
        walk::walk_ts_import_equals_declaration(self, node);
    }

    fn visit_import_declaration(&mut self, node: &ImportDeclaration<'a>) {
        self.internal_import(&node.source);
        walk::walk_import_declaration(self, node);
    }
    fn visit_export_from_declaration(&mut self, node: &ExportFromDeclaration<'a>) {
        self.internal_import(&node.source);
        walk::walk_export_from_declaration(self, node);
    }
    fn visit_export_all_declaration(&mut self, node: &ExportAllDeclaration<'a>) {
        self.internal_import(&node.source);
        walk::walk_export_all_declaration(self, node);
    }
}
