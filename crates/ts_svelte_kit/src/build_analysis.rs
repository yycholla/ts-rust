//! Static Kit build metadata from immutable module snapshots. This never executes
//! application code. Known facts assume successful module initialization; any
//! unsupported initialization or value is recorded as a runtime requirement.
//!
//! Semantics follow installed Kit 2.67–2.70 and Kit 3.0.1/next.32
//! `core/postbuild/analyse.js`, `utils/page_nodes.js` and `utils/routing.js`.
//! Kit 2 merges config universal then server; Kit 3 server then universal.
use crate::sync_versions::KitSyncVersion;
use oxc_allocator::Allocator;
use oxc_ast::ast::*;
use oxc_parser::Parser;
use oxc_span::{GetSpan, SourceType};
use oxc_syntax::operator::{LogicalOperator, UnaryOperator};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    cell::Cell,
    collections::{BTreeMap, BTreeSet},
};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeRequirement {
    pub module: String,
    pub subject: String,
    pub reason: String,
}
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "camelCase")]
pub enum Fact<T> {
    Known {
        value: T,
    },
    Unknown {
        requirements: Vec<RuntimeRequirement>,
    },
}
impl<T> Fact<T> {
    fn known(value: T) -> Self {
        Self::Known { value }
    }
}
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "camelCase")]
pub enum StaticValue {
    Undefined,
    Json(Value),
    Function,
    Object(BTreeMap<String, StaticValue>),
    Array(Vec<StaticValue>),
    Unknown(Vec<RuntimeRequirement>),
}
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModuleSnapshot {
    pub source: String,
    /// Exact resolver output for each import specifier. Missing entries are
    /// unresolved; no filesystem, extension or package resolution is guessed.
    #[serde(default)]
    pub imports: BTreeMap<String, String>,
}
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AnalysisNode {
    pub universal: Option<String>,
    pub server: Option<String>,
}
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AnalysisPage {
    pub layouts: Vec<Option<usize>>,
    pub leaf: usize,
}
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AnalysisRoute {
    pub id: String,
    pub page: Option<AnalysisPage>,
    pub endpoint: Option<String>,
}
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AnalysisInput {
    pub kit_version: String,
    pub modules: BTreeMap<String, ModuleSnapshot>,
    pub nodes: Vec<AnalysisNode>,
    pub routes: Vec<AnalysisRoute>,
}
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeAnalysis {
    /// Exact Kit predicate: load !== undefined || trailingSlash !== undefined.
    pub has_server_load: Fact<bool>,
    pub has_universal_load: Fact<bool>,
    /// Separate from Kit's presence predicate: null/false/non-functions are not
    /// silently reported as callable handlers.
    pub server_load_callable: Fact<bool>,
    pub universal_load_callable: Fact<bool>,
}
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RouteAnalysis {
    pub csr: Fact<bool>,
    pub ssr: Fact<bool>,
    pub prerender: Fact<Value>,
    pub trailing_slash: Fact<String>,
    pub config: Fact<Value>,
    pub page_methods: Fact<Vec<String>>,
    pub endpoint_methods: Fact<Vec<String>>,
    pub methods: Fact<Vec<String>>,
    pub actions: Fact<BTreeMap<String, bool>>,
    /// Entries callbacks are identified, never executed or presented as their
    /// unknown returned prerender parameter sets.
    pub entries: StaticValue,
}
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AnalysisOutput {
    pub modules: BTreeMap<String, BTreeMap<String, StaticValue>>,
    pub nodes: Vec<NodeAnalysis>,
    pub routes: BTreeMap<String, RouteAnalysis>,
    pub requirements: Vec<RuntimeRequirement>,
    pub errors: Vec<String>,
}
#[derive(Debug, Clone)]
enum Expr {
    Value(StaticValue),
    Ref(String, u32),
    Import(String, String),
    Member(Box<Expr>, String),
    Object(Vec<(Option<String>, Expr)>),
    Array(Vec<Expr>),
    Conditional(Box<Expr>, Box<Expr>, Box<Expr>),
    Logical(LogicalOperator, Box<Expr>, Box<Expr>),
    Unary(UnaryOperator, Box<Expr>),
    Default(Box<Expr>, Box<Expr>),
}
#[derive(Debug)]
struct Binding {
    value: Expr,
    start: u32,
    hoisted: bool,
}
#[derive(Debug, Default)]
struct Module {
    bindings: BTreeMap<String, Binding>,
    exports: BTreeMap<String, Expr>,
    stars: Vec<String>,
    effects: Vec<RuntimeRequirement>,
}
fn unknown(module: &str, subject: &str, reason: impl Into<String>) -> StaticValue {
    StaticValue::Unknown(vec![RuntimeRequirement {
        module: module.into(),
        subject: subject.into(),
        reason: reason.into(),
    }])
}
fn unsupported(module: &str, expr: &Expression<'_>, reason: &str) -> Expr {
    Expr::Value(unknown(
        module,
        &format!("bytes {}..{}", expr.span().start, expr.span().end),
        reason,
    ))
}
fn lower(module: &str, expr: &Expression<'_>, depth: usize) -> Expr {
    if depth > 96 {
        return unsupported(
            module,
            expr,
            "Expression nesting exceeds native analysis limit",
        );
    }
    let next = |expr: &Expression<'_>| lower(module, expr, depth + 1);
    match expr {
        Expression::BooleanLiteral(v) => Expr::Value(StaticValue::Json(json!(v.value))),
        Expression::StringLiteral(v) => Expr::Value(StaticValue::Json(json!(v.value.as_str()))),
        Expression::NumericLiteral(v) if v.value.is_finite() => {
            Expr::Value(StaticValue::Json(json!(v.value)))
        }
        Expression::NullLiteral(_) => Expr::Value(StaticValue::Json(Value::Null)),
        Expression::Identifier(v) => Expr::Ref(v.name.to_string(), v.span.start),
        Expression::FunctionExpression(_) | Expression::ArrowFunctionExpression(_) => {
            Expr::Value(StaticValue::Function)
        }
        Expression::ParenthesizedExpression(v) => next(&v.expression),
        Expression::TSAsExpression(v) => next(&v.expression),
        Expression::TSSatisfiesExpression(v) => next(&v.expression),
        Expression::TSNonNullExpression(v) => next(&v.expression),
        Expression::TSTypeAssertion(v) => next(&v.expression),
        Expression::ObjectExpression(v) => {
            let mut properties = vec![];
            for property in &v.properties {
                match property {
                    ObjectPropertyKind::SpreadProperty(v) => {
                        properties.push((None, next(&v.argument)))
                    }
                    ObjectPropertyKind::ObjectProperty(v) => {
                        let Some(key) = v.key.static_name().filter(|_| {
                            !v.computed
                                || matches!(
                                    &v.key,
                                    PropertyKey::StringLiteral(_) | PropertyKey::NumericLiteral(_)
                                )
                        }) else {
                            return unsupported(
                                module,
                                expr,
                                "Computed object keys require runtime evaluation",
                            );
                        };
                        if key == "__proto__" && !v.computed && !v.shorthand {
                            return unsupported(
                                module,
                                expr,
                                "Object prototype setters require runtime evaluation",
                            );
                        }
                        if v.kind != PropertyKind::Init {
                            return unsupported(
                                module,
                                expr,
                                "Object getters/setters require runtime evaluation",
                            );
                        }
                        properties.push((Some(key.into_owned()), next(&v.value)));
                    }
                }
            }
            Expr::Object(properties)
        }
        Expression::ArrayExpression(v) => {
            let mut entries = vec![];
            for entry in &v.elements {
                if let Some(expr) = entry.as_expression() {
                    entries.push(next(expr));
                } else {
                    return unsupported(
                        module,
                        expr,
                        "Array holes/spreads require runtime evaluation",
                    );
                }
            }
            Expr::Array(entries)
        }
        Expression::StaticMemberExpression(v) if !v.optional => {
            Expr::Member(Box::new(next(&v.object)), v.property.name.to_string())
        }
        Expression::ComputedMemberExpression(v) if !v.optional => {
            if let Expression::StringLiteral(key) = &v.expression {
                Expr::Member(Box::new(next(&v.object)), key.value.to_string())
            } else {
                unsupported(
                    module,
                    expr,
                    "Computed property access requires a static string",
                )
            }
        }
        Expression::ConditionalExpression(v) => Expr::Conditional(
            Box::new(next(&v.test)),
            Box::new(next(&v.consequent)),
            Box::new(next(&v.alternate)),
        ),
        Expression::LogicalExpression(v) => Expr::Logical(
            v.operator,
            Box::new(next(&v.left)),
            Box::new(next(&v.right)),
        ),
        Expression::UnaryExpression(v)
            if matches!(
                v.operator,
                UnaryOperator::LogicalNot
                    | UnaryOperator::Void
                    | UnaryOperator::UnaryNegation
                    | UnaryOperator::UnaryPlus
                    | UnaryOperator::Typeof
            ) =>
        {
            Expr::Unary(v.operator, Box::new(next(&v.argument)))
        }
        Expression::TemplateLiteral(v) if v.expressions.is_empty() => {
            Expr::Value(StaticValue::Json(json!(
                v.quasis
                    .first()
                    .and_then(|q| q.value.cooked.as_ref())
                    .map(|s| s.as_str())
                    .unwrap_or("")
            )))
        }
        _ => unsupported(
            module,
            expr,
            "Calls, runtime expressions and unsupported operators are not executed",
        ),
    }
}
fn bind(
    module: &mut Module,
    id: &str,
    pattern: &BindingPattern<'_>,
    value: Expr,
    start: u32,
    exported: bool,
) {
    match pattern {
        BindingPattern::BindingIdentifier(name) => {
            let name = name.name.to_string();
            module.bindings.insert(
                name.clone(),
                Binding {
                    value,
                    start,
                    hoisted: false,
                },
            );
            if exported {
                module
                    .exports
                    .insert(name.clone(), Expr::Ref(name, u32::MAX));
            }
        }
        BindingPattern::ObjectPattern(object) => {
            for property in &object.properties {
                let value = if !property.computed {
                    property
                        .key
                        .static_name()
                        .map(|key| Expr::Member(Box::new(value.clone()), key.into_owned()))
                        .unwrap_or_else(|| {
                            Expr::Value(unknown(id, "destructuring", "Unknown property key"))
                        })
                } else {
                    Expr::Value(unknown(id, "destructuring", "Computed destructuring key"))
                };
                bind(module, id, &property.value, value, start, exported);
            }
            if let Some(rest) = &object.rest {
                bind(
                    module,
                    id,
                    &rest.argument,
                    Expr::Value(unknown(
                        id,
                        "destructuring",
                        "Object rest requires runtime evaluation",
                    )),
                    start,
                    exported,
                );
            }
        }
        BindingPattern::ArrayPattern(array) => {
            for (index, pattern) in array.elements.iter().enumerate() {
                if let Some(pattern) = pattern {
                    bind(
                        module,
                        id,
                        pattern,
                        Expr::Member(Box::new(value.clone()), index.to_string()),
                        start,
                        exported,
                    );
                }
            }
            if let Some(rest) = &array.rest {
                bind(
                    module,
                    id,
                    &rest.argument,
                    Expr::Value(unknown(
                        id,
                        "destructuring",
                        "Array rest requires runtime evaluation",
                    )),
                    start,
                    exported,
                );
            }
        }
        BindingPattern::AssignmentPattern(pattern) => bind(
            module,
            id,
            &pattern.left,
            Expr::Default(Box::new(value), Box::new(lower(id, &pattern.right, 0))),
            start,
            exported,
        ),
    }
}
fn variables(module: &mut Module, id: &str, declaration: &VariableDeclaration<'_>, exported: bool) {
    if declaration.declare {
        return;
    }
    for variable in &declaration.declarations {
        let value = if declaration.kind == VariableDeclarationKind::Const {
            variable
                .init
                .as_ref()
                .map(|expr| lower(id, expr, 0))
                .unwrap_or(Expr::Value(StaticValue::Undefined))
        } else {
            Expr::Value(unknown(
                id,
                "mutable binding",
                "let/var bindings require runtime evaluation",
            ))
        };
        bind(
            module,
            id,
            &variable.id,
            value,
            variable.span.start,
            exported,
        );
    }
}
fn function(module: &mut Module, function: &Function<'_>, exported: bool) {
    if function.body.is_none() || function.declare {
        return;
    }
    if let Some(id) = &function.id {
        let name = id.name.to_string();
        module.bindings.insert(
            name.clone(),
            Binding {
                value: Expr::Value(StaticValue::Function),
                start: 0,
                hoisted: true,
            },
        );
        if exported {
            module
                .exports
                .insert(name.clone(), Expr::Ref(name, u32::MAX));
        }
    }
}
fn parse(id: &str, snapshot: &ModuleSnapshot) -> Result<Module, String> {
    let arena = Allocator::default();
    let source_type = SourceType::from_path(id)
        .unwrap_or(SourceType::ts())
        .with_module(true);
    let parsed = Parser::new(&arena, &snapshot.source, source_type).parse();
    if !parsed.diagnostics.is_empty() {
        return Err(format!(
            "Cannot statically parse {id}: {:?}",
            parsed.diagnostics[0]
        ));
    }
    let mut module = Module::default();
    let imported = |specifier: &str, name: &str| {
        snapshot
            .imports
            .get(specifier)
            .map(|target| Expr::Import(target.clone(), name.into()))
            .unwrap_or_else(|| {
                Expr::Value(unknown(
                    id,
                    name,
                    format!("Unresolved import {specifier:?}"),
                ))
            })
    };
    for statement in &parsed.program.body {
        match statement {
            Statement::VariableDeclaration(v) => variables(&mut module, id, v, false),
            Statement::FunctionDeclaration(v) => function(&mut module, v, false),
            Statement::ExportDeclaration(v) => match &v.declaration {
                Declaration::VariableDeclaration(v) => variables(&mut module, id, v, true),
                Declaration::FunctionDeclaration(v) => function(&mut module, v, true),
                Declaration::TSTypeAliasDeclaration(_) | Declaration::TSInterfaceDeclaration(_) => {
                }
                _ => module.effects.push(RuntimeRequirement {
                    module: id.into(),
                    subject: "module initialization".into(),
                    reason: "Unsupported exported runtime declaration".into(),
                }),
            },
            Statement::ExportNamedDeclaration(v) if !v.export_kind.is_type() => {
                for specifier in &v.specifiers {
                    if !specifier.export_kind.is_type() {
                        module.exports.insert(
                            specifier.exported.name().to_string(),
                            Expr::Ref(specifier.local.name().to_string(), u32::MAX),
                        );
                    }
                }
            }
            Statement::ExportFromDeclaration(v) if !v.export_kind.is_type() => {
                for specifier in &v.specifiers {
                    if !specifier.export_kind.is_type() {
                        module.exports.insert(
                            specifier.exported.name().to_string(),
                            imported(v.source.value.as_str(), specifier.local.name().as_str()),
                        );
                    }
                }
            }
            Statement::ExportAllDeclaration(v) if !v.export_kind.is_type() => {
                if let Some(exported) = &v.exported {
                    module.exports.insert(
                        exported.name().to_string(),
                        Expr::Value(unknown(
                            id,
                            exported.name().as_str(),
                            "Namespace reexports require runtime namespace analysis",
                        )),
                    );
                } else {
                    module.stars.push(
                        snapshot
                            .imports
                            .get(v.source.value.as_str())
                            .cloned()
                            .unwrap_or_else(|| format!("unresolved:{}:{}", id, v.source.value)),
                    );
                }
            }
            Statement::ImportDeclaration(v) if !v.import_kind.is_type() => {
                for specifier in v.specifiers.iter().flatten() {
                    let (local, value) = match specifier {
                        ImportDeclarationSpecifier::ImportSpecifier(v)
                            if !v.import_kind.is_type() =>
                        {
                            (
                                v.local.name.to_string(),
                                imported(v_import_source(statement), v.imported.name().as_str()),
                            )
                        }
                        ImportDeclarationSpecifier::ImportDefaultSpecifier(local) => (
                            local.local.name.to_string(),
                            imported(v.source.value.as_str(), "default"),
                        ),
                        ImportDeclarationSpecifier::ImportNamespaceSpecifier(local) => (
                            local.local.name.to_string(),
                            Expr::Value(unknown(
                                id,
                                local.local.name.as_str(),
                                "Namespace imports require runtime namespace analysis",
                            )),
                        ),
                        _ => continue,
                    };
                    module.bindings.insert(
                        local,
                        Binding {
                            value,
                            start: 0,
                            hoisted: true,
                        },
                    );
                }
            }
            Statement::ExportDefaultDeclaration(v) => {
                if matches!(
                    &v.declaration,
                    ExportDefaultDeclarationKind::TSInterfaceDeclaration(_)
                ) || matches!(&v.declaration,ExportDefaultDeclarationKind::FunctionDeclaration(function) if function.body.is_none() || function.declare)
                {
                    continue;
                }
                let value = match &v.declaration {
                    ExportDefaultDeclarationKind::FunctionDeclaration(_) => {
                        Expr::Value(StaticValue::Function)
                    }
                    _ => v
                        .declaration
                        .as_expression()
                        .map(|expr| lower(id, expr, 0))
                        .unwrap_or_else(|| {
                            Expr::Value(unknown(id, "default", "Unsupported default export"))
                        }),
                };
                module.exports.insert("default".into(), value);
            }
            Statement::EmptyStatement(_)
            | Statement::TSTypeAliasDeclaration(_)
            | Statement::TSInterfaceDeclaration(_)
            | Statement::ImportDeclaration(_)
            | Statement::ExportNamedDeclaration(_)
            | Statement::ExportFromDeclaration(_)
            | Statement::ExportAllDeclaration(_) => {}
            _ => module.effects.push(RuntimeRequirement {
                module: id.into(),
                subject: "module initialization".into(),
                reason: format!(
                    "Executable statement at {}..{} may mutate exports or throw",
                    statement.span().start,
                    statement.span().end
                ),
            }),
        }
    }
    Ok(module)
}
fn v_import_source<'a>(statement: &'a Statement<'_>) -> &'a str {
    if let Statement::ImportDeclaration(v) = statement {
        v.source.value.as_str()
    } else {
        unreachable!()
    }
}
struct Evaluator {
    modules: BTreeMap<String, Module>,
    fuel: Cell<usize>,
}
impl Evaluator {
    fn export(
        &self,
        module: &str,
        name: &str,
        stack: &mut BTreeSet<(String, String)>,
    ) -> StaticValue {
        let key = (module.to_string(), format!("export:{name}"));
        if stack.len() > 128 || !stack.insert(key.clone()) {
            return unknown(module, name, "Cyclic or excessively deep export graph");
        }
        let value = if let Some(ir) = self.modules.get(module) {
            if !ir.effects.is_empty() {
                StaticValue::Unknown(ir.effects.clone())
            } else if let Some(expr) = ir.exports.get(name) {
                self.eval(module, expr, stack)
            } else if name == "default" {
                StaticValue::Undefined
            } else {
                let mut candidates = vec![];
                for target in &ir.stars {
                    let value = self.export(target, name, stack);
                    if !matches!(value, StaticValue::Undefined) {
                        candidates.push(value);
                    }
                }
                match candidates.len() {
                    0 => StaticValue::Undefined,
                    1 => candidates.remove(0),
                    _ => unknown(
                        module,
                        name,
                        "Ambiguous star reexport requires module binding identity resolution",
                    ),
                }
            }
        } else {
            unknown(module, name, "Imported module snapshot is unavailable")
        };
        stack.remove(&key);
        value
    }
    fn eval(
        &self,
        module: &str,
        expr: &Expr,
        stack: &mut BTreeSet<(String, String)>,
    ) -> StaticValue {
        if self.fuel.get() == 0 {
            return unknown(
                module,
                "evaluation",
                "Static evaluation step budget exhausted",
            );
        }
        self.fuel.set(self.fuel.get() - 1);
        match expr {
            Expr::Value(value) => value.clone(),
            Expr::Ref(name, at) => {
                let ir = &self.modules[module];
                if let Some(binding) = ir.bindings.get(name) {
                    if !binding.hoisted && *at < binding.start {
                        return unknown(
                            module,
                            name,
                            "Reference precedes lexical initialization (temporal dead zone)",
                        );
                    }
                    let key = (module.to_string(), format!("binding:{name}"));
                    if stack.len() > 128 || !stack.insert(key.clone()) {
                        return unknown(module, name, "Cyclic or excessively deep local binding");
                    }
                    let result = self.eval(module, &binding.value, stack);
                    stack.remove(&key);
                    result
                } else if name == "undefined" {
                    StaticValue::Undefined
                } else {
                    unknown(module, name, "Unbound or ambient runtime identifier")
                }
            }
            Expr::Import(target, name) => self.export(target, name, stack),
            Expr::Member(object, name) => match self.eval(module, object, stack) {
                StaticValue::Object(properties) => {
                    properties.get(name).cloned().unwrap_or_else(|| {
                        if [
                            "__proto__",
                            "constructor",
                            "toString",
                            "valueOf",
                            "hasOwnProperty",
                            "isPrototypeOf",
                            "propertyIsEnumerable",
                            "toLocaleString",
                            "__defineGetter__",
                            "__defineSetter__",
                            "__lookupGetter__",
                            "__lookupSetter__",
                        ]
                        .contains(&name.as_str())
                        {
                            unknown(
                                module,
                                name,
                                "Inherited properties require runtime object semantics",
                            )
                        } else {
                            StaticValue::Undefined
                        }
                    })
                }
                StaticValue::Array(values) if name == "length" => {
                    StaticValue::Json(json!(values.len()))
                }
                StaticValue::Array(values) => match name.parse::<usize>() {
                    Ok(index) if index.to_string() == *name => {
                        values.get(index).cloned().unwrap_or(StaticValue::Undefined)
                    }
                    _ => unknown(
                        module,
                        name,
                        "Array member requires runtime property semantics",
                    ),
                },
                value @ StaticValue::Unknown(_) => value,
                _ => unknown(
                    module,
                    name,
                    "Member access requires a known plain object/array",
                ),
            },
            Expr::Object(properties) => {
                let mut result = BTreeMap::new();
                for (name, value) in properties {
                    let value = self.eval(module, value, stack);
                    if let Some(name) = name {
                        result.insert(name.clone(), value);
                    } else {
                        match value {
                            StaticValue::Object(properties) => result.extend(properties),
                            StaticValue::Undefined | StaticValue::Json(Value::Null) => {}
                            value @ StaticValue::Unknown(_) => return value,
                            _ => {
                                return unknown(
                                    module,
                                    "object spread",
                                    "Spread value is not a known plain object",
                                );
                            }
                        }
                    }
                }
                StaticValue::Object(result)
            }
            Expr::Array(values) => StaticValue::Array(
                values
                    .iter()
                    .map(|value| self.eval(module, value, stack))
                    .collect(),
            ),
            Expr::Conditional(test, yes, no) => match truth(&self.eval(module, test, stack)) {
                Fact::Known { value } => self.eval(module, if value { yes } else { no }, stack),
                Fact::Unknown { requirements } => StaticValue::Unknown(requirements),
            },
            Expr::Logical(operator, left, right) => {
                let left = self.eval(module, left, stack);
                if *operator == LogicalOperator::Coalesce {
                    return coalesce(left, || self.eval(module, right, stack));
                }
                match truth(&left) {
                    Fact::Known { value } => {
                        if value == (*operator == LogicalOperator::And) {
                            self.eval(module, right, stack)
                        } else {
                            left
                        }
                    }
                    Fact::Unknown { requirements } => StaticValue::Unknown(requirements),
                }
            }
            Expr::Default(left, right) => match self.eval(module, left, stack) {
                StaticValue::Undefined => self.eval(module, right, stack),
                value => value,
            },
            Expr::Unary(operator, argument) => {
                let value = self.eval(module, argument, stack);
                if matches!(value, StaticValue::Unknown(_)) {
                    return value;
                }
                match operator {
                    UnaryOperator::LogicalNot => match truth(&value) {
                        Fact::Known { value } => StaticValue::Json(json!(!value)),
                        Fact::Unknown { requirements } => StaticValue::Unknown(requirements),
                    },
                    UnaryOperator::Void => StaticValue::Undefined,
                    UnaryOperator::Typeof => StaticValue::Json(json!(match &value {
                        StaticValue::Undefined => "undefined",
                        StaticValue::Function => "function",
                        StaticValue::Json(Value::Bool(_)) => "boolean",
                        StaticValue::Json(Value::String(_)) => "string",
                        StaticValue::Json(Value::Number(_)) => "number",
                        _ => "object",
                    })),
                    UnaryOperator::UnaryNegation | UnaryOperator::UnaryPlus => {
                        if let StaticValue::Json(Value::Number(number)) = value {
                            StaticValue::Json(json!(
                                number.as_f64().unwrap()
                                    * if *operator == UnaryOperator::UnaryNegation {
                                        -1.0
                                    } else {
                                        1.0
                                    }
                            ))
                        } else {
                            unknown(
                                module,
                                "unary operator",
                                "Numeric coercion requires runtime evaluation",
                            )
                        }
                    }
                    _ => unreachable!(),
                }
            }
        }
    }
    fn get(&self, module: Option<&String>, name: &str) -> StaticValue {
        module.map_or(StaticValue::Undefined, |module| {
            self.export(module, name, &mut BTreeSet::new())
        })
    }
    fn names(
        &self,
        module: &str,
        seen: &mut BTreeSet<String>,
        names: &mut BTreeSet<String>,
    ) -> bool {
        if !seen.insert(module.into()) {
            return true;
        }
        let Some(ir) = self.modules.get(module) else {
            return false;
        };
        names.extend(ir.exports.keys().cloned());
        let mut complete = true;
        for target in &ir.stars {
            let mut other = BTreeSet::new();
            complete &= self.names(target, seen, &mut other);
            names.extend(other.into_iter().filter(|name| name != "default"));
        }
        complete
    }
}
fn truth(value: &StaticValue) -> Fact<bool> {
    match value {
        StaticValue::Unknown(requirements) => Fact::Unknown {
            requirements: requirements.clone(),
        },
        StaticValue::Undefined => Fact::known(false),
        StaticValue::Json(v) => Fact::known(match v {
            Value::Null => false,
            Value::Bool(v) => *v,
            Value::Number(v) => v.as_f64() != Some(0.0),
            Value::String(v) => !v.is_empty(),
            _ => true,
        }),
        _ => Fact::known(true),
    }
}
fn defined(value: &StaticValue) -> Fact<bool> {
    match value {
        StaticValue::Unknown(requirements) => Fact::Unknown {
            requirements: requirements.clone(),
        },
        value => Fact::known(!matches!(value, StaticValue::Undefined)),
    }
}
fn callable(value: &StaticValue) -> Fact<bool> {
    match value {
        StaticValue::Unknown(requirements) => Fact::Unknown {
            requirements: requirements.clone(),
        },
        value => Fact::known(matches!(value, StaticValue::Function)),
    }
}
fn coalesce(value: StaticValue, fallback: impl FnOnce() -> StaticValue) -> StaticValue {
    if matches!(
        value,
        StaticValue::Undefined | StaticValue::Json(Value::Null)
    ) {
        fallback()
    } else {
        value
    }
}
fn either(left: Fact<bool>, right: Fact<bool>) -> Fact<bool> {
    match (left, right) {
        (Fact::Known { value: true }, _) | (_, Fact::Known { value: true }) => Fact::known(true),
        (Fact::Known { value: false }, right) => right,
        (left, Fact::Known { value: false }) => left,
        (
            Fact::Unknown { mut requirements },
            Fact::Unknown {
                requirements: other,
            },
        ) => {
            requirements.extend(other);
            Fact::Unknown { requirements }
        }
    }
}
fn requirements(value: &StaticValue, out: &mut BTreeSet<RuntimeRequirement>) {
    match value {
        StaticValue::Unknown(reasons) => out.extend(reasons.iter().cloned()),
        StaticValue::Object(values) => {
            for value in values.values() {
                requirements(value, out)
            }
        }
        StaticValue::Array(values) => {
            for value in values {
                requirements(value, out)
            }
        }
        _ => {}
    }
}
fn json_value(value: &StaticValue) -> Fact<Value> {
    match value {
        StaticValue::Unknown(requirements) => Fact::Unknown {
            requirements: requirements.clone(),
        },
        StaticValue::Json(value) => Fact::known(value.clone()),
        StaticValue::Undefined => Fact::known(Value::Null),
        StaticValue::Function => Fact::Unknown {
            requirements: vec![RuntimeRequirement {
                module: "<configuration>".into(),
                subject: "config".into(),
                reason: "Function-valued configuration requires a resolved host contract".into(),
            }],
        },
        StaticValue::Object(values) => {
            let mut object = serde_json::Map::new();
            let mut reasons = vec![];
            for (key, value) in values {
                if matches!(value, StaticValue::Undefined) {
                    continue;
                }
                match json_value(value) {
                    Fact::Known { value } => {
                        object.insert(key.clone(), value);
                    }
                    Fact::Unknown { requirements } => reasons.extend(requirements),
                }
            }
            if reasons.is_empty() {
                Fact::known(Value::Object(object))
            } else {
                Fact::Unknown {
                    requirements: reasons,
                }
            }
        }
        StaticValue::Array(values) => {
            let mut array = vec![];
            let mut reasons = vec![];
            for value in values {
                match json_value(value) {
                    Fact::Known { value } => array.push(value),
                    Fact::Unknown { requirements } => reasons.extend(requirements),
                }
            }
            if reasons.is_empty() {
                Fact::known(Value::Array(array))
            } else {
                Fact::Unknown {
                    requirements: reasons,
                }
            }
        }
    }
}

fn typed_bool(value: &StaticValue, module: &str, option: &str) -> Fact<bool> {
    match value {
        StaticValue::Json(Value::Bool(value)) => Fact::known(*value),
        StaticValue::Unknown(requirements) => Fact::Unknown {
            requirements: requirements.clone(),
        },
        _ => Fact::Unknown {
            requirements: vec![RuntimeRequirement {
                module: module.into(),
                subject: option.into(),
                reason: "Option is not a boolean; runtime validation required".into(),
            }],
        },
    }
}
fn methods(evaluator: &Evaluator, module: Option<&String>) -> Fact<Vec<String>> {
    let mut methods = vec![];
    let mut reasons = vec![];
    for name in [
        "GET", "POST", "PUT", "PATCH", "DELETE", "OPTIONS", "HEAD", "fallback",
    ] {
        match truth(&evaluator.get(module, name)) {
            Fact::Known { value: true } => {
                methods.push(if name == "fallback" { "*" } else { name }.into())
            }
            Fact::Unknown { requirements } => reasons.extend(requirements),
            _ => {}
        }
    }
    if reasons.is_empty() {
        Fact::known(methods)
    } else {
        Fact::Unknown {
            requirements: reasons,
        }
    }
}
fn collect_fact<T>(fact: &Fact<T>, all: &mut BTreeSet<RuntimeRequirement>) {
    if let Fact::Unknown { requirements } = fact {
        all.extend(requirements.iter().cloned());
    }
}

fn validate_exports(
    evaluator: &Evaluator,
    module: Option<&String>,
    role: &str,
    errors: &mut Vec<String>,
) {
    let Some(module) = module else {
        return;
    };
    let mut names = BTreeSet::new();
    evaluator.names(module, &mut BTreeSet::new(), &mut names);
    let allowed: &[&str] = match role {
        "endpoint" => &[
            "GET",
            "POST",
            "PUT",
            "PATCH",
            "DELETE",
            "OPTIONS",
            "HEAD",
            "fallback",
            "prerender",
            "trailingSlash",
            "config",
            "entries",
        ],
        "pageServer" => &[
            "load",
            "prerender",
            "csr",
            "ssr",
            "trailingSlash",
            "config",
            "entries",
            "actions",
        ],
        "page" => &[
            "load",
            "prerender",
            "csr",
            "ssr",
            "trailingSlash",
            "config",
            "entries",
        ],
        _ => &["load", "prerender", "csr", "ssr", "trailingSlash", "config"],
    };
    for name in names {
        if !name.starts_with('_') && !allowed.contains(&name.as_str()) {
            let error = format!("Invalid {role} export {name:?} in {module}");
            if !errors.contains(&error) {
                errors.push(error);
            }
        }
    }
}

/// Analyze resolved nodes/routes in one bounded, deterministic snapshot graph.
/// Errors are syntax/contract failures; unknown runtime facts remain in output.
pub fn analyze(input: &AnalysisInput) -> Result<AnalysisOutput, String> {
    let version = KitSyncVersion::parse(&input.kit_version)?;
    if input.modules.len() > 4096 || input.nodes.len() > 65536 || input.routes.len() > 65536 {
        return Err("Static build analysis graph exceeds supported limits".into());
    }
    let mut total = 0usize;
    let mut parsed = BTreeMap::new();
    for (id, snapshot) in &input.modules {
        total = total
            .checked_add(snapshot.source.len())
            .ok_or("Source size overflow")?;
        if snapshot.source.len() > 4 * 1024 * 1024 || total > 64 * 1024 * 1024 {
            return Err("Static build analysis source limit exceeded".into());
        }
        parsed.insert(id.clone(), parse(id, snapshot)?);
    }
    let mut evaluator = Evaluator {
        modules: parsed,
        fuel: Cell::new(500_000),
    };
    // An unexecuted initializer may mutate another exported binding, including
    // a const object's properties. Never retain a stale literal snapshot.
    let mut initializer_effects = BTreeMap::new();
    for (id, module) in &evaluator.modules {
        for (name, binding) in &module.bindings {
            if binding.hoisted {
                continue;
            }
            let value = evaluator.eval(id, &binding.value, &mut BTreeSet::new());
            let mut reasons = BTreeSet::new();
            requirements(&value, &mut reasons);
            if !reasons.is_empty() {
                initializer_effects.entry(id.clone()).or_insert_with(Vec::new).push(RuntimeRequirement {module:id.clone(),subject:format!("initializer {name}"),reason:"Initializer is not fully statically evaluable; it may affect module exports".into()});
            }
        }
    }
    for (id, effects) in initializer_effects {
        evaluator
            .modules
            .get_mut(&id)
            .unwrap()
            .effects
            .extend(effects);
    }

    let mut output = AnalysisOutput {
        modules: BTreeMap::new(),
        nodes: vec![],
        routes: BTreeMap::new(),
        requirements: vec![],
        errors: vec![],
    };
    let mut all = BTreeSet::new();
    for (id, module) in &evaluator.modules {
        all.extend(module.effects.iter().cloned());
        let mut names = BTreeSet::new();
        if !evaluator.names(id, &mut BTreeSet::new(), &mut names) {
            all.insert(RuntimeRequirement {
                module: id.clone(),
                subject: "export namespace".into(),
                reason: "Unresolved star export may introduce additional runtime exports".into(),
            });
        }
        let exports = names
            .into_iter()
            .map(|name| {
                let value = evaluator.get(Some(id), &name);
                requirements(&value, &mut all);
                (name, value)
            })
            .collect();
        output.modules.insert(id.clone(), exports);
    }
    for node in &input.nodes {
        let server = evaluator.get(node.server.as_ref(), "load");
        let universal = evaluator.get(node.universal.as_ref(), "load");
        let node = NodeAnalysis {
            has_server_load: either(
                defined(&server),
                defined(&evaluator.get(node.server.as_ref(), "trailingSlash")),
            ),
            has_universal_load: defined(&universal),
            server_load_callable: callable(&server),
            universal_load_callable: callable(&universal),
        };
        collect_fact(&node.has_server_load, &mut all);
        collect_fact(&node.has_universal_load, &mut all);
        output.nodes.push(node);
    }
    for route in &input.routes {
        if output.routes.contains_key(&route.id) {
            return Err(format!("Duplicate route {}", route.id));
        }
        validate_exports(
            &evaluator,
            route.endpoint.as_ref(),
            "endpoint",
            &mut output.errors,
        );
        let mut chain = vec![];
        if let Some(page) = &route.page {
            for index in page.layouts.iter().flatten() {
                let node = input
                    .nodes
                    .get(*index)
                    .ok_or_else(|| format!("Invalid layout index {index}"))?;
                validate_exports(
                    &evaluator,
                    node.universal.as_ref(),
                    "layout",
                    &mut output.errors,
                );
                validate_exports(
                    &evaluator,
                    node.server.as_ref(),
                    "layoutServer",
                    &mut output.errors,
                );
            }
            let leaf = input
                .nodes
                .get(page.leaf)
                .ok_or_else(|| format!("Invalid leaf index {}", page.leaf))?;
            validate_exports(
                &evaluator,
                leaf.universal.as_ref(),
                "page",
                &mut output.errors,
            );
            validate_exports(
                &evaluator,
                leaf.server.as_ref(),
                "pageServer",
                &mut output.errors,
            );
            for index in page
                .layouts
                .iter()
                .flatten()
                .chain(std::iter::once(&page.leaf))
            {
                chain.push(
                    input
                        .nodes
                        .get(*index)
                        .ok_or_else(|| format!("Invalid node index {index} in {}", route.id))?,
                );
            }
        }
        let option = |name: &str, default: StaticValue| {
            let mut current = default;
            for node in &chain {
                let value = coalesce(evaluator.get(node.universal.as_ref(), name), || {
                    evaluator.get(node.server.as_ref(), name)
                });
                current = coalesce(value, || current);
            }
            current
        };
        let csr = typed_bool(
            &option("csr", StaticValue::Json(json!(true))),
            &route.id,
            "csr",
        );
        let ssr = typed_bool(
            &option("ssr", StaticValue::Json(json!(true))),
            &route.id,
            "ssr",
        );
        let page_prerender = option("prerender", StaticValue::Json(json!(false)));
        let endpoint_prerender =
            coalesce(evaluator.get(route.endpoint.as_ref(), "prerender"), || {
                StaticValue::Json(json!(false))
            });
        let prerender_value = if route.page.is_some() {
            page_prerender.clone()
        } else {
            endpoint_prerender.clone()
        };
        let prerender = match &prerender_value {
            StaticValue::Json(Value::Bool(_)) => json_value(&prerender_value),
            StaticValue::Json(Value::String(value)) if value == "auto" => {
                json_value(&prerender_value)
            }
            StaticValue::Unknown(_) => json_value(&prerender_value),
            _ => Fact::Unknown {
                requirements: vec![RuntimeRequirement {
                    module: route.id.clone(),
                    subject: "prerender".into(),
                    reason: "Expected false, true or auto".into(),
                }],
            },
        };
        let trailing = if route.page.is_some() {
            option("trailingSlash", StaticValue::Json(json!("never")))
        } else {
            coalesce(
                evaluator.get(route.endpoint.as_ref(), "trailingSlash"),
                || StaticValue::Json(json!("never")),
            )
        };
        let trailing_slash = match trailing {
            StaticValue::Json(Value::String(value))
                if ["never", "always", "ignore"].contains(&value.as_str()) =>
            {
                Fact::known(value)
            }
            StaticValue::Unknown(requirements) => Fact::Unknown { requirements },
            _ => Fact::Unknown {
                requirements: vec![RuntimeRequirement {
                    module: route.id.clone(),
                    subject: "trailingSlash".into(),
                    reason: "Invalid trailingSlash option".into(),
                }],
            },
        };
        let mut page_config = StaticValue::Object(BTreeMap::new());
        for node in &chain {
            let pair = if version.is_kit3() {
                [node.server.as_ref(), node.universal.as_ref()]
            } else {
                [node.universal.as_ref(), node.server.as_ref()]
            };
            for module in pair {
                page_config = merge_config(page_config, evaluator.get(module, "config"), &route.id);
            }
        }
        if matches!(&page_config,StaticValue::Object(value) if value.is_empty()) {
            page_config = StaticValue::Undefined;
        }
        let endpoint_config = evaluator.get(route.endpoint.as_ref(), "config");
        if route.page.is_some() && route.endpoint.is_some() {
            if let (Fact::Known { value: true }, Fact::Known { value: true }) =
                (truth(&page_prerender), truth(&endpoint_prerender))
            {
                output.errors.push(format!(
                    "Cannot prerender both page and endpoint for {}",
                    route.id
                ));
            }
            if let (Fact::Known { value: true }, Fact::Known { value: true }) =
                (truth(&page_config), truth(&endpoint_config))
            {
                if let (Fact::Known { value: a }, Fact::Known { value: b }) =
                    (json_value(&page_config), json_value(&endpoint_config))
                {
                    if a != b {
                        output
                            .errors
                            .push(format!("Page and endpoint config differ for {}", route.id));
                    }
                }
            }
        }
        let config = json_value(&coalesce(page_config, || {
            coalesce(endpoint_config, || StaticValue::Object(BTreeMap::new()))
        }));
        let leaf = chain.last();
        let actions_value = evaluator.get(leaf.and_then(|node| node.server.as_ref()), "actions");
        let page_methods = if route.page.is_none() {
            Fact::known(vec![])
        } else {
            match truth(&actions_value) {
                Fact::Known { value } => Fact::known(if value {
                    vec!["GET".into(), "POST".into()]
                } else {
                    vec!["GET".into()]
                }),
                Fact::Unknown { requirements } => Fact::Unknown { requirements },
            }
        };
        let actions = action_names(&actions_value, &route.id);
        let endpoint_methods = methods(&evaluator, route.endpoint.as_ref());
        if let Fact::Known { value: true } = truth(&endpoint_prerender) {
            if let Fact::Known { value } = &endpoint_methods {
                if value
                    .iter()
                    .any(|method| ["POST", "PUT", "PATCH", "DELETE"].contains(&method.as_str()))
                {
                    output.errors.push(format!(
                        "Cannot prerender body-dependent endpoint methods for {}",
                        route.id
                    ));
                }
            }
        }
        let methods = match (&page_methods, &endpoint_methods) {
            (Fact::Known { value: page }, Fact::Known { value: api }) => {
                let mut values = page.clone();
                for value in api {
                    if !values.contains(value) {
                        values.push(value.clone());
                    }
                }
                Fact::known(values)
            }
            _ => {
                let mut reasons = BTreeSet::new();
                collect_fact(&page_methods, &mut reasons);
                collect_fact(&endpoint_methods, &mut reasons);
                Fact::Unknown {
                    requirements: reasons.into_iter().collect(),
                }
            }
        };
        let entries = if let Some(leaf) = leaf {
            coalesce(evaluator.get(leaf.universal.as_ref(), "entries"), || {
                evaluator.get(leaf.server.as_ref(), "entries")
            })
        } else {
            evaluator.get(route.endpoint.as_ref(), "entries")
        };
        let entries = coalesce(entries, || {
            evaluator.get(route.endpoint.as_ref(), "entries")
        });
        if matches!(truth(&entries), Fact::Known { value: true }) {
            all.insert(RuntimeRequirement {module:route.id.clone(),subject:"entries".into(),reason:"Prerender entries callback results require runtime execution or host-resolved entries".into()});
        }
        requirements(&entries, &mut all);
        collect_fact(&csr, &mut all);
        collect_fact(&ssr, &mut all);
        collect_fact(&prerender, &mut all);
        collect_fact(&trailing_slash, &mut all);
        collect_fact(&config, &mut all);
        collect_fact(&methods, &mut all);
        collect_fact(&actions, &mut all);
        output.routes.insert(
            route.id.clone(),
            RouteAnalysis {
                csr,
                ssr,
                prerender,
                trailing_slash,
                config,
                page_methods,
                endpoint_methods,
                methods,
                actions,
                entries,
            },
        );
    }
    output.requirements = all.into_iter().collect();
    Ok(output)
}
fn merge_config(current: StaticValue, next: StaticValue, route: &str) -> StaticValue {
    match (current, next) {
        (current, StaticValue::Undefined | StaticValue::Json(Value::Null)) => current,
        (StaticValue::Object(mut current), StaticValue::Object(next)) => {
            current.extend(next);
            StaticValue::Object(current)
        }
        (value @ StaticValue::Unknown(_), _) | (_, value @ StaticValue::Unknown(_)) => value,
        (_, _) => unknown(
            route,
            "config",
            "Configuration spread is not a known plain object",
        ),
    }
}
fn action_names(value: &StaticValue, route: &str) -> Fact<BTreeMap<String, bool>> {
    match value {
        StaticValue::Undefined | StaticValue::Json(Value::Null) => Fact::known(BTreeMap::new()),
        StaticValue::Object(actions) => {
            let mut result = BTreeMap::new();
            let mut reasons = vec![];
            for (name, value) in actions {
                match callable(value) {
                    Fact::Known { value } => {
                        result.insert(name.clone(), value);
                    }
                    Fact::Unknown { requirements } => reasons.extend(requirements),
                }
            }
            if reasons.is_empty() {
                Fact::known(result)
            } else {
                Fact::Unknown {
                    requirements: reasons,
                }
            }
        }
        StaticValue::Unknown(requirements) => Fact::Unknown {
            requirements: requirements.clone(),
        },
        _ => Fact::Unknown {
            requirements: vec![RuntimeRequirement {
                module: route.into(),
                subject: "actions".into(),
                reason: "Actions export is not a known plain object".into(),
            }],
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn input(sources: &[(&str, &str)]) -> AnalysisInput {
        AnalysisInput {
            kit_version: "2.69.2".into(),
            modules: sources
                .iter()
                .map(|(id, source)| {
                    (
                        id.to_string(),
                        ModuleSnapshot {
                            source: source.to_string(),
                            imports: BTreeMap::new(),
                        },
                    )
                })
                .collect(),
            nodes: vec![AnalysisNode {
                universal: None,
                server: Some(sources[0].0.into()),
            }],
            routes: vec![AnalysisRoute {
                id: "/".into(),
                page: Some(AnalysisPage {
                    layouts: vec![],
                    leaf: 0,
                }),
                endpoint: None,
            }],
        }
    }
    #[test]
    fn values_and_functions_are_not_inferred_from_export_names() {
        let output=analyze(&input(&[("/page.ts","const enabled = false; export {enabled as csr}; export const load = undefined; export const actions = {save: async () => ({})};")])).unwrap();
        assert_eq!(output.nodes[0].has_server_load, Fact::known(false));
        assert_eq!(output.routes["/"].csr, Fact::known(false));
        assert_eq!(
            output.routes["/"].methods,
            Fact::known(vec!["GET".into(), "POST".into()])
        );
        assert!(output.requirements.is_empty());
        let output = analyze(&input(&[(
            "/page.ts",
            "export const load=null;export const trailingSlash='always';",
        )]))
        .unwrap();
        assert_eq!(output.nodes[0].has_server_load, Fact::known(true));
        assert_eq!(output.nodes[0].server_load_callable, Fact::known(false));
    }
    #[test]
    fn config_order_is_versioned_and_scalars_prefer_universal() {
        let mut input = input(&[
            (
                "/server.ts",
                "export const config={runtime:'server',a:1};export const csr=false;",
            ),
            (
                "/universal.ts",
                "export const config={runtime:'universal',b:2};export const csr=true;",
            ),
        ]);
        input.nodes[0].universal = Some("/universal.ts".into());
        assert_eq!(
            analyze(&input).unwrap().routes["/"].config,
            Fact::known(json!({"runtime":"server","a":1.0,"b":2.0}))
        );
        input.kit_version = "3.0.1".into();
        let output = analyze(&input).unwrap();
        assert_eq!(
            output.routes["/"].config,
            Fact::known(json!({"runtime":"universal","a":1.0,"b":2.0}))
        );
        assert_eq!(output.routes["/"].csr, Fact::known(true));
    }
    #[test]
    fn explicit_reexports_and_cycles_are_deterministic() {
        let mut input = input(&[
            (
                "/page.ts",
                "export {handler as load, enabled as ssr} from './shared';",
            ),
            (
                "/shared.ts",
                "export async function handler(){} export const enabled=false;",
            ),
        ]);
        input
            .modules
            .get_mut("/page.ts")
            .unwrap()
            .imports
            .insert("./shared".into(), "/shared.ts".into());
        let output = analyze(&input).unwrap();
        assert_eq!(output.nodes[0].server_load_callable, Fact::known(true));
        assert_eq!(output.routes["/"].ssr, Fact::known(false));
        input.modules.get_mut("/shared.ts").unwrap().source =
            "export {load as handler} from './page';".into();
        input
            .modules
            .get_mut("/shared.ts")
            .unwrap()
            .imports
            .insert("./page".into(), "/page.ts".into());
        let output = analyze(&input).unwrap();
        assert!(matches!(
            output.nodes[0].has_server_load,
            Fact::Unknown { .. }
        ));
        assert_eq!(
            serde_json::to_value(&output).unwrap(),
            serde_json::to_value(analyze(&input).unwrap()).unwrap()
        );
    }
    #[test]
    fn runtime_mutation_tdz_imports_and_entries_stay_unknown() {
        for source in [
            "export const csr=process.env.CSR;",
            "export let csr=false;",
            "const value=later;const later=false;export {value as csr};",
            "export const csr=false;globalThis.run();",
        ] {
            let output = analyze(&input(&[("/page.ts", source)])).unwrap();
            assert!(
                matches!(output.routes["/"].csr, Fact::Unknown { .. }),
                "{source}"
            );
            assert!(!output.requirements.is_empty());
        }
        let output = analyze(&input(&[(
            "/page.ts",
            "export const entries=()=>[{id:'x'}];",
        )]))
        .unwrap();
        assert!(
            output
                .requirements
                .iter()
                .any(|requirement| requirement.subject == "entries")
        );
    }
    #[test]
    fn endpoint_methods_use_values_and_prerender_rejects_mutative_methods() {
        let mut input = input(&[(
            "/server.ts",
            "export const GET=undefined;export function POST(){}export const prerender=true;",
        )]);
        input.routes[0].page = None;
        input.routes[0].endpoint = Some("/server.ts".into());
        let output = analyze(&input).unwrap();
        assert_eq!(output.routes["/"].methods, Fact::known(vec!["POST".into()]));
        assert_eq!(output.errors.len(), 1);
    }
    #[test]
    fn destructuring_and_short_circuiting_preserve_known_values() {
        let output=analyze(&input(&[("/page.ts","const settings={csr:false,ssr:true};export const {csr,ssr}=settings;export const prerender=false && run();")])).unwrap();
        assert_eq!(output.routes["/"].csr, Fact::known(false));
        assert_eq!(output.routes["/"].ssr, Fact::known(true));
        assert_eq!(output.routes["/"].prerender, Fact::known(json!(false)));
    }
    #[test]
    fn initializer_mutation_and_nonserializable_config_cannot_be_reported_resolved() {
        let output=analyze(&input(&[("/page.ts","export const config={runtime:'node'};const ignored=Object.assign(config,{runtime:'edge'});")])).unwrap();
        assert!(matches!(output.routes["/"].config, Fact::Unknown { .. }));
        let output = analyze(&input(&[(
            "/page.ts",
            "export const config={choose:()=>true};",
        )]))
        .unwrap();
        assert!(matches!(output.routes["/"].config, Fact::Unknown { .. }));
    }
    #[test]
    fn inheritance_skips_holes_and_child_values_override_unknown_parents() {
        let mut input = input(&[
            (
                "/root.ts",
                "export const csr=runtime();export const prerender=true;",
            ),
            (
                "/leaf.ts",
                "export const csr=false;export const prerender=false;",
            ),
        ]);
        input.nodes.push(AnalysisNode {
            universal: Some("/leaf.ts".into()),
            server: None,
        });
        input.routes[0].page = Some(AnalysisPage {
            layouts: vec![Some(0), None],
            leaf: 1,
        });
        let output = analyze(&input).unwrap();
        assert_eq!(output.routes["/"].csr, Fact::known(false));
        assert_eq!(output.routes["/"].prerender, Fact::known(json!(false)));
        assert!(!output.requirements.is_empty());
    }
    #[test]
    fn type_declarations_do_not_create_handlers_and_invalid_exports_are_reported() {
        let output = analyze(&input(&[(
            "/page.ts",
            "export declare function load():void;export type Thing=string;export const wrong=1;",
        )]))
        .unwrap();
        assert_eq!(output.nodes[0].has_server_load, Fact::known(false));
        assert_eq!(output.errors.len(), 1);
        assert!(output.errors[0].contains("wrong"));
    }
}
