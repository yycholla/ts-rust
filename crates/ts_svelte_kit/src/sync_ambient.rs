//! Pure SvelteKit declaration generation, modeled on Kit 2.69.2.
//!
//! The host supplies environment values and the route manifest explicitly. This
//! module never reads process environment variables, `.env` files or project
//! configuration, and declarations never contain environment values.
use crate::sync_versions::KitSyncVersion;
use std::collections::BTreeMap;

const GENERATED: &str = "// this file is generated — do not edit it\n";
const ENV_DOCS: &str =
    "// See https://svelte.dev/docs/kit/environment-variables for more information";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnvPrefixes {
    pub public: String,
    pub private: String,
}

impl Default for EnvPrefixes {
    fn default() -> Self {
        Self {
            public: "PUBLIC_".into(),
            private: String::new(),
        }
    }
}

/// Kit deliberately accepts only ASCII environment export identifiers.
pub fn valid_env_identifier(name: &str) -> bool {
    let mut bytes = name.bytes();
    matches!(bytes.next(), Some(b'a'..=b'z' | b'A'..=b'Z' | b'_' | b'$'))
        && bytes.all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'$')
}

/// Explicit environment definitions and runtime static exports reject these.
/// Legacy ambient declaration generation only applies `valid_env_identifier`,
/// matching Kit's separate declaration and runtime rules.
pub fn reserved_env_identifier(name: &str) -> bool {
    matches!(
        name,
        "do" | "if"
            | "in"
            | "for"
            | "let"
            | "new"
            | "try"
            | "var"
            | "case"
            | "else"
            | "enum"
            | "eval"
            | "null"
            | "this"
            | "true"
            | "void"
            | "with"
            | "await"
            | "break"
            | "catch"
            | "class"
            | "const"
            | "false"
            | "super"
            | "throw"
            | "while"
            | "yield"
            | "delete"
            | "export"
            | "import"
            | "public"
            | "return"
            | "static"
            | "switch"
            | "typeof"
            | "default"
            | "extends"
            | "finally"
            | "package"
            | "private"
            | "continue"
            | "debugger"
            | "function"
            | "arguments"
            | "interface"
            | "protected"
            | "implements"
            | "instanceof"
    )
}

fn filtered_names<'a>(
    env: &'a BTreeMap<String, String>,
    allowed: &str,
    disallowed: &str,
) -> Vec<&'a str> {
    env.keys()
        .filter(|key| {
            key.starts_with(allowed)
                && (disallowed.is_empty() || !key.starts_with(disallowed))
                && valid_env_identifier(key)
        })
        .map(String::as_str)
        .collect()
}

fn template_literal_fragment(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('`', "\\`")
        .replace("${", "\\${")
        .replace('\r', "\\r")
        .replace('\n', "\\n")
}

/// Generate `ambient.d.ts`. Values are deliberately ignored; only names matter.
pub fn generate_ambient(
    env: &BTreeMap<String, String>,
    prefixes: &EnvPrefixes,
    explicit_environment_variables: bool,
) -> String {
    let mut output = format!("{GENERATED}\n/// <reference types=\"@sveltejs/kit\" />\n");
    if explicit_environment_variables {
        return output;
    }
    for (kind, allowed, disallowed) in [
        (
            "private",
            prefixes.private.as_str(),
            prefixes.public.as_str(),
        ),
        (
            "public",
            prefixes.public.as_str(),
            prefixes.private.as_str(),
        ),
    ] {
        output.push_str(&format!("\ndeclare module '$env/static/{kind}' {{\n"));
        for key in filtered_names(env, allowed, disallowed) {
            output.push_str(&format!("\texport const {key}: string;\n"));
        }
        output.push_str("}\n");
    }
    for (kind, allowed, disallowed) in [
        (
            "private",
            prefixes.private.as_str(),
            prefixes.public.as_str(),
        ),
        (
            "public",
            prefixes.public.as_str(),
            prefixes.private.as_str(),
        ),
    ] {
        output.push_str(&format!(
            "\ndeclare module '$env/dynamic/{kind}' {{\n\texport const env: {{\n"
        ));
        for key in filtered_names(env, allowed, disallowed) {
            output.push_str(&format!("\t\t{key}: string;\n"));
        }
        if !disallowed.is_empty() {
            output.push_str(&format!(
                "\t\t[key: `{}${{string}}`]: undefined;\n",
                template_literal_fragment(disallowed)
            ));
        }
        output.push_str(&format!(
            "\t\t[key: `{}${{string}}`]: string | undefined;\n\t}}\n}}\n",
            template_literal_fragment(allowed)
        ));
    }
    output
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExplicitEnvVariable {
    pub name: String,
    pub public: bool,
    pub description: Option<String>,
    /// Whether the host-resolved configuration declares a Standard Schema.
    /// Its output type is inferred from the original module, never widened.
    pub has_schema: bool,
}

fn quote(value: &str) -> String {
    // Serializing a Rust string cannot fail.
    serde_json::to_string(value).expect("string serialization")
}

/// Generate `env.d.ts` for experimental explicit environment variables.
///
/// The caller resolves the entry and variable metadata. Arbitrary schema
/// execution and validation are outside this pure declaration generator.
/// # Errors
/// Rejects invalid/reserved or duplicate variable names.
pub fn generate_explicit_env(
    relative_entry: Option<&str>,
    variables: &[ExplicitEnvVariable],
) -> Result<String, String> {
    let Some(entry) = relative_entry else {
        return Ok(ENV_DOCS.into());
    };
    let mut seen = std::collections::BTreeSet::new();
    for variable in variables {
        if !valid_env_identifier(&variable.name) || reserved_env_identifier(&variable.name) {
            return Err(format!(
                "Invalid environment variable name {}",
                quote(&variable.name)
            ));
        }
        if !seen.insert(&variable.name) {
            return Err(format!(
                "Duplicate environment variable name {}",
                quote(&variable.name)
            ));
        }
    }
    let mut output = format!(
        "// This file is generated from {}.\n{ENV_DOCS}",
        entry.replace(['\n', '\r'], " ")
    );
    for public in [false, true] {
        let kind = if public { "public" } else { "private" };
        output.push_str(&format!("\n\ndeclare module '$app/env/{kind}' {{\n"));
        let mut count = 0;
        for variable in variables.iter().filter(|v| v.public == public) {
            count += 1;
            if let Some(description) = variable.description.as_ref().filter(|s| !s.is_empty()) {
                output.push_str("\t/**\n");
                for line in description.split('\n') {
                    output.push_str(&format!("\t * {}\n", line.replace("*/", "*\\/")));
                }
                output.push_str("\t */\n");
            }
            let ty = if variable.has_schema {
                format!(
                    "import('@sveltejs/kit/internal/types').StandardSchemaV1.InferOutput<typeof import({}).variables.{}.schema>",
                    quote(entry),
                    variable.name
                )
            } else {
                "string".into()
            };
            output.push_str(&format!("\texport const {}: {ty};\n", variable.name));
        }
        if count == 0 {
            output.push_str(&format!(
                "\t// no {kind} environment variables were defined\n"
            ));
        }
        output.push('}');
    }
    Ok(output)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppTypeParam {
    pub name: String,
    pub optional: bool,
    pub matcher: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrailingSlash {
    Never,
    Always,
    Ignore,
    /// Unresolved page options allow both variants, as Kit does during sync.
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppTypeRoute {
    pub id: String,
    pub params: Vec<AppTypeParam>,
    /// `None` means this route has no page leaf.
    pub page: Option<TrailingSlash>,
    /// `None` means no endpoint; an endpoint with absent options uses `Never`.
    pub endpoint: Option<TrailingSlash>,
}

/// Kit 3 exposes layout params only for actual layouts, and app route IDs only
/// for pages/endpoints. Kit 2's existing API retains its broader contract.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VersionedAppTypeRoute {
    pub route: AppTypeRoute,
    pub layout: bool,
}

/// Kit 3 removed the legacy `$env` modules and the ambient output file entirely.
#[must_use]
pub fn generate_ambient_for_version(
    env: &BTreeMap<String, String>,
    prefixes: &EnvPrefixes,
    explicit_environment_variables: bool,
    version: &KitSyncVersion,
) -> Option<String> {
    (!version.is_kit3()).then(|| generate_ambient(env, prefixes, explicit_environment_variables))
}

/// Generate Kit 2 `outDir/env.d.ts` or Kit 3
/// `node_modules/$app/types/env.d.ts`. `relative_entry` must be relative to the
/// selected output directory, rather than always relative to Kit's outDir.
/// # Errors
/// Rejects invalid explicit environment names as `generate_explicit_env` does.
pub fn generate_explicit_env_for_version(
    relative_entry: Option<&str>,
    variables: &[ExplicitEnvVariable],
    version: &KitSyncVersion,
) -> Result<String, String> {
    if relative_entry.is_none() && version.is_kit3() {
        return Ok(format!(
            "{ENV_DOCS}\n\ndeclare module '$app/env/private' {{\n\t// no private environment variables were defined\n}}\n\ndeclare module '$app/env/public' {{\n\t// no public environment variables were defined\n}}"
        ));
    }
    generate_explicit_env(relative_entry, variables)
}

const KIT3_HTML_ATTRIBUTES: &str = r#"declare module "svelte/elements" {
	export interface HTMLAttributes<T> {
		'data-sveltekit-preload-code'?: true | false | '' | 'eager' | 'viewport' | 'hover' | 'tap' | undefined | null;
		'data-sveltekit-preload-data'?: true | false | '' | 'hover' | 'tap' | undefined | null;
		'data-sveltekit-reload'?: true | false | '' | undefined | null;
		'data-sveltekit-replacestate'?: true | false | '' | undefined | null;
		'data-sveltekit-reset'?: true | false | '' | undefined | null;
	}
}"#;

fn kit3_matcher_type(matcher: Option<&str>, module: &str) -> String {
    matcher.map_or_else(
        || "string".into(),
        |matcher| {
            format!(
                "import('@sveltejs/kit/params').MatcherParam<(typeof import({}).params)[{}]>",
                quote(module),
                quote(matcher)
            )
        },
    )
}

#[derive(Clone, PartialEq, Eq)]
struct PathPattern {
    literal: String,
    template: String,
}

fn valid_param_content(content: &str) -> bool {
    let mut parts = content.split('=');
    let valid_name = |name: &str| {
        !name.is_empty()
            && name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    };
    valid_name(parts.next().unwrap_or_default())
        && parts.next().is_none_or(valid_name)
        && parts.next().is_none()
}

fn omittable_segment(segment: &str) -> bool {
    segment
        .strip_prefix("[[")
        .and_then(|s| s.strip_suffix("]]"))
        .or_else(|| {
            segment
                .strip_prefix("[...")
                .and_then(|s| s.strip_suffix(']'))
        })
        .is_some_and(valid_param_content)
}

fn append_literal(pattern: &mut PathPattern, text: &str) {
    pattern.literal.push_str(text);
    pattern.template.push_str(&template_literal_fragment(text));
}

fn kit3_segment(segment: &str) -> Result<PathPattern, String> {
    let mut pattern = PathPattern {
        literal: String::new(),
        template: String::new(),
    };
    let mut rest = segment;
    while let Some(start) = rest.find('[') {
        append_literal(&mut pattern, &rest[..start]);
        rest = &rest[start..];
        if let Some(close) = rest.find(']') {
            let inner = &rest[1..close];
            if let Some(code) = inner
                .strip_prefix("x+")
                .or_else(|| inner.strip_prefix("u+"))
            {
                let mut decoded = String::new();
                for codepoint in code.split('-') {
                    let value = u32::from_str_radix(codepoint, 16)
                        .ok()
                        .and_then(char::from_u32)
                        .ok_or_else(|| format!("Invalid route escape sequence {inner}"))?;
                    match value {
                        '%' => decoded.push_str("%25"),
                        '/' => decoded.push_str("%2F"),
                        '?' => decoded.push_str("%3F"),
                        '#' => decoded.push_str("%23"),
                        value => decoded.push(value),
                    }
                }
                append_literal(&mut pattern, &decoded);
                rest = &rest[close + 1..];
                continue;
            }
            let content = inner.strip_prefix('[').unwrap_or(inner);
            let content = content.strip_prefix("...").unwrap_or(content);
            if valid_param_content(content) {
                pattern.literal.push_str("${string}");
                pattern.template.push_str("${string}");
                rest = &rest[close + 1..];
                // Kit's basic segment pattern accepts an optional extra `]`.
                rest = rest.strip_prefix(']').unwrap_or(rest);
                continue;
            }
        }
        append_literal(&mut pattern, "[");
        rest = &rest[1..];
    }
    append_literal(&mut pattern, rest);
    Ok(pattern)
}

fn kit3_path_patterns(id: &str) -> Result<Vec<PathPattern>, String> {
    let pathname = route_pathname(id);
    let mut patterns = vec![PathPattern {
        literal: String::new(),
        template: String::new(),
    }];
    for segment in pathname
        .trim_start_matches('/')
        .split('/')
        .filter(|s| !s.is_empty())
    {
        let omittable = omittable_segment(segment);
        let content = if omittable {
            PathPattern {
                literal: "${string}".into(),
                template: "${string}".into(),
            }
        } else {
            kit3_segment(segment)?
        };
        let mut next = Vec::new();
        for pattern in patterns {
            let join = |path: &str, part: &str| {
                if path.is_empty() {
                    part.to_owned()
                } else {
                    format!("{path}/{part}")
                }
            };
            push_unique(
                &mut next,
                PathPattern {
                    literal: join(&pattern.literal, &content.literal),
                    template: join(&pattern.template, &content.template),
                },
            );
            if omittable {
                push_unique(&mut next, pattern);
            }
        }
        if next.len() > 65_536 {
            return Err(
                "Route optional segments generate more than 65536 pathname patterns".into(),
            );
        }
        patterns = next;
    }
    Ok(patterns)
}

/// Generate Kit 2's `outDir/non-ambient.d.ts` or Kit 3's
/// `node_modules/$app/types/index.d.ts`. The matcher path is a directory for Kit
/// 2, but the resolved params **module file**, relative to the output directory,
/// for Kit 3. Hosts must resolve the installed module extension first.
/// # Errors
/// Rejects malformed route escapes or excessive optional-path expansion.
pub fn generate_non_ambient_for_version(
    routes: &[VersionedAppTypeRoute],
    assets: &[String],
    matcher_path: &str,
    version: &KitSyncVersion,
) -> Result<String, String> {
    if !version.is_kit3() {
        return Ok(generate_non_ambient(
            &routes
                .iter()
                .map(|route| route.route.clone())
                .collect::<Vec<_>>(),
            assets,
            matcher_path,
        ));
    }
    let mut layouts = routes
        .iter()
        .map(|route| {
            if route.layout {
                route
                    .route
                    .params
                    .iter()
                    .map(|p| LayoutParam {
                        name: p.name.clone(),
                        optional: p.optional,
                        matchers: p.matcher.as_ref().map(|m| vec![m.clone()]),
                    })
                    .collect::<Vec<_>>()
            } else {
                Vec::new()
            }
        })
        .collect::<Vec<_>>();
    for child in routes {
        for (index, _) in routes
            .iter()
            .enumerate()
            .filter(|(_, parent)| parent.layout && is_ancestor(&parent.route.id, &child.route.id))
        {
            for param in &child.route.params {
                if let Some(existing) = layouts[index].iter_mut().find(|p| p.name == param.name) {
                    match (&mut existing.matchers, &param.matcher) {
                        (Some(matchers), Some(matcher)) => push_unique(matchers, matcher.clone()),
                        (_, None) => existing.matchers = None,
                        (None, Some(_)) => {}
                    }
                } else {
                    layouts[index].push(LayoutParam {
                        name: param.name.clone(),
                        optional: true,
                        matchers: param.matcher.as_ref().map(|m| vec![m.clone()]),
                    });
                }
            }
        }
    }
    let mut pages = Vec::new();
    let mut endpoints = Vec::new();
    let mut app_routes = Vec::new();
    let mut dynamic_routes = Vec::new();
    let mut layout_routes = Vec::new();
    let mut pathnames = Vec::new();
    for (route, layout_params) in routes.iter().zip(layouts) {
        let data = &route.route;
        if data.page.is_some() {
            pages.push(quote(&data.id));
        }
        if data.endpoint.is_some() {
            endpoints.push(quote(&data.id));
        }
        let app_route = data.page.is_some() || data.endpoint.is_some();
        if app_route {
            app_routes.push(quote(&data.id));
        }
        if app_route && !data.params.is_empty() {
            let fields = data
                .params
                .iter()
                .map(|p| {
                    property(
                        &p.name,
                        p.optional,
                        &kit3_matcher_type(p.matcher.as_deref(), matcher_path),
                    )
                })
                .collect::<Vec<_>>()
                .join("; ");
            dynamic_routes.push(format!("{}: {{ {fields} }}", quote(&data.id)));
        }
        if route.layout {
            let fields = layout_params
                .iter()
                .map(|p| {
                    let ty = p.matchers.as_ref().map_or_else(
                        || "string".into(),
                        |matchers| {
                            matchers
                                .iter()
                                .map(|m| kit3_matcher_type(Some(m), matcher_path))
                                .collect::<Vec<_>>()
                                .join(" | ")
                        },
                    );
                    property(&p.name, p.optional, &ty)
                })
                .collect::<Vec<_>>()
                .join("; ");
            let ty = if fields.is_empty() {
                "Record<string, never>".into()
            } else {
                format!("{{ {fields} }}")
            };
            layout_routes.push(format!("{}: {ty}", quote(&data.id)));
        }
        for pattern in kit3_path_patterns(&data.id)? {
            let serialize = |slash: bool| {
                let suffix = if slash { "/" } else { "" };
                if data.params.is_empty() {
                    quote(&format!("{}{suffix}", pattern.literal))
                } else {
                    format!("`{}{suffix}` & {{}}", pattern.template)
                }
            };
            for policy in [data.page, data.endpoint].into_iter().flatten() {
                if policy != TrailingSlash::Always {
                    push_unique(&mut pathnames, serialize(false));
                }
                if policy != TrailingSlash::Never {
                    push_unique(&mut pathnames, serialize(true));
                }
            }
        }
    }
    let union = |values: Vec<String>| {
        if values.is_empty() {
            "never".into()
        } else {
            values.join(" | ")
        }
    };
    let env_import = if matches!(version, KitSyncVersion::Kit3Prerelease) {
        "./env"
    } else {
        "./env.js"
    };
    Ok(format!(
        "{GENERATED}\nimport '@sveltejs/kit';\nimport '{env_import}';\n\ndeclare module \"$app/types\" {{\n\texport interface AppTypes {{\n\t\tPageRouteId(): {};\n\t\tEndpointRouteId(): {};\n\t\tRouteId(): {};\n\t\tRouteParams(): {{\n\t\t\t{}\n\t\t}};\n\t\tLayoutParams(): {{\n\t\t\t{}\n\t\t}};\n\t\tPath(): {};\n\t\tResolvedPathname(): `${{\"/\" | `/${{string}}/`}}${{ReturnType<AppTypes['Path']>}}`;\n\t\tAssetPath(): {};\n\t}}\n}}\n\n{KIT3_HTML_ATTRIBUTES}\n",
        union(pages),
        union(endpoints),
        union(app_routes),
        dynamic_routes.join(";\n\t\t\t"),
        layout_routes.join(";\n\t\t\t"),
        union(pathnames),
        union(assets.iter().map(|asset| quote(asset)).collect())
    ))
}

const HTML_ATTRIBUTES: &str = r#"declare module "svelte/elements" {
	export interface HTMLAttributes<T> {
		'data-sveltekit-keepfocus'?: true | '' | 'off' | undefined | null;
		'data-sveltekit-noscroll'?: true | '' | 'off' | undefined | null;
		'data-sveltekit-preload-code'?: true | '' | 'eager' | 'viewport' | 'hover' | 'tap' | 'off' | undefined | null;
		'data-sveltekit-preload-data'?: true | '' | 'hover' | 'tap' | 'off' | undefined | null;
		'data-sveltekit-reload'?: true | '' | 'off' | undefined | null;
		'data-sveltekit-replacestate'?: true | '' | 'off' | undefined | null;
	}
}

export {};
"#;

/// Generate the HTML attribute augmentation alone for older Kit profiles.
pub fn generate_html_attributes() -> String {
    format!("{GENERATED}\n{HTML_ATTRIBUTES}")
}

#[derive(Clone)]
struct LayoutParam {
    name: String,
    optional: bool,
    matchers: Option<Vec<String>>,
}

fn push_unique<T: PartialEq>(values: &mut Vec<T>, value: T) {
    if !values.contains(&value) {
        values.push(value);
    }
}

fn is_ancestor(parent: &str, child: &str) -> bool {
    parent == "/"
        || parent == child
        || child
            .strip_prefix(parent)
            .is_some_and(|rest| rest.starts_with('/'))
}

fn matcher_type(matcher: Option<&str>, directory: &str) -> String {
    match matcher {
        None => "string".into(),
        Some(matcher) => {
            let path = if directory.is_empty() {
                format!("{matcher}.js")
            } else {
                format!("{}/{matcher}.js", directory.trim_end_matches('/'))
            };
            format!("MatcherParam<typeof import({}).match>", quote(&path))
        }
    }
}

fn property(name: &str, optional: bool, ty: &str) -> String {
    let name = if valid_env_identifier(name) {
        name.to_owned()
    } else {
        quote(name)
    };
    format!(
        "{name}{}: {ty}{}",
        if optional { "?" } else { "" },
        if optional { " | undefined" } else { "" }
    )
}

fn route_pathname(id: &str) -> String {
    format!(
        "/{}",
        id.trim_start_matches('/')
            .split('/')
            .filter(|part| {
                !part.is_empty()
                    && !part
                        .strip_prefix('(')
                        .and_then(|part| part.strip_suffix(')'))
                        .is_some_and(|group| !group.is_empty() && !group.contains(')'))
            })
            .collect::<Vec<_>>()
            .join("/")
    )
}

// This mirrors Kit's two replacements: /[[optional]] -> ${string}, then
// /[required] -> /${string}. Mixed segments retain their literal suffix.
fn dynamic_pathname(pathname: &str) -> String {
    let mut output = String::new();
    let mut rest = pathname;
    while let Some(start) = rest.find("/[") {
        output.push_str(&template_literal_fragment(&rest[..start]));
        let pattern = &rest[start..];
        let optional = pattern.starts_with("/[[");
        let begin = if optional { 3 } else { 2 };
        if let Some(close) = pattern[begin..].find(']') {
            let end = begin + close + 1;
            if close > 0 && (!optional || pattern[end..].starts_with(']')) {
                output.push_str(if optional { "${string}" } else { "/${string}" });
                rest = &pattern[end + usize::from(optional)..];
                continue;
            }
        }
        output.push_str("/[");
        rest = &pattern[2..];
    }
    output.push_str(&template_literal_fragment(rest));
    output
}

/// Generate `non-ambient.d.ts`, including `$app/types` route/asset contracts.
///
/// Route and asset order is preserved. `matcher_directory` is the slash-separated
/// import path from the output directory to the configured params directory.
pub fn generate_non_ambient(
    routes: &[AppTypeRoute],
    assets: &[String],
    matcher_directory: &str,
) -> String {
    let mut layouts: Vec<Vec<LayoutParam>> = routes
        .iter()
        .map(|route| {
            route
                .params
                .iter()
                .map(|param| LayoutParam {
                    name: param.name.clone(),
                    optional: param.optional,
                    matchers: param.matcher.as_ref().map(|m| vec![m.clone()]),
                })
                .collect()
        })
        .collect();
    for child in routes {
        for (index, _) in routes
            .iter()
            .enumerate()
            .filter(|(_, parent)| is_ancestor(&parent.id, &child.id))
        {
            for param in &child.params {
                if let Some(existing) = layouts[index].iter_mut().find(|p| p.name == param.name) {
                    match (&mut existing.matchers, &param.matcher) {
                        (Some(matchers), Some(matcher)) => push_unique(matchers, matcher.clone()),
                        (_, None) => existing.matchers = None,
                        (None, Some(_)) => {}
                    }
                } else {
                    layouts[index].push(LayoutParam {
                        name: param.name.clone(),
                        optional: true,
                        matchers: param.matcher.as_ref().map(|m| vec![m.clone()]),
                    });
                }
            }
        }
    }
    let mut dynamic_routes = Vec::new();
    let mut layout_routes = Vec::new();
    let mut pathnames = Vec::new();
    for (route, params) in routes.iter().zip(layouts) {
        if !route.params.is_empty() {
            let fields = route
                .params
                .iter()
                .map(|p| {
                    property(
                        &p.name,
                        p.optional,
                        &matcher_type(p.matcher.as_deref(), matcher_directory),
                    )
                })
                .collect::<Vec<_>>()
                .join("; ");
            dynamic_routes.push(format!("{}: {{ {fields} }}", quote(&route.id)));
        }
        let fields = params
            .iter()
            .map(|p| {
                let ty = p.matchers.as_ref().map_or_else(
                    || "string".into(),
                    |matchers| {
                        matchers
                            .iter()
                            .map(|m| matcher_type(Some(m), matcher_directory))
                            .collect::<Vec<_>>()
                            .join(" | ")
                    },
                );
                property(&p.name, p.optional, &ty)
            })
            .collect::<Vec<_>>()
            .join("; ");
        let layout_type = if fields.is_empty() {
            "Record<string, never>".into()
        } else {
            format!("{{ {fields} }}")
        };
        layout_routes.push(format!("{}: {layout_type}", quote(&route.id)));
        let raw_pathname = route_pathname(&route.id);
        let dynamic = !route.params.is_empty();
        let pathname = if dynamic {
            dynamic_pathname(&raw_pathname)
        } else {
            raw_pathname
        };
        let serialise = |value: &str| {
            if dynamic {
                format!("`{value}` & {{}}")
            } else {
                quote(value)
            }
        };
        // Kit always includes the root pathname, including a layout-only root.
        if pathname == "/" {
            push_unique(&mut pathnames, serialise(&pathname));
        } else {
            for policy in [route.page, route.endpoint].into_iter().flatten() {
                if policy != TrailingSlash::Always {
                    push_unique(&mut pathnames, serialise(&pathname));
                }
                if policy != TrailingSlash::Never {
                    push_unique(&mut pathnames, serialise(&format!("{pathname}/")));
                }
            }
        }
    }
    let route_ids = routes.iter().map(|r| quote(&r.id)).collect::<Vec<_>>();
    let mut asset_types = assets
        .iter()
        .map(|asset| quote(&format!("/{asset}")))
        .collect::<Vec<_>>();
    asset_types.push("string & {}".into());
    let union = |types: Vec<String>| {
        if types.is_empty() {
            "never".into()
        } else {
            types.join(" | ")
        }
    };
    format!(
        "{}\ndeclare module \"$app/types\" {{\n\ttype MatcherParam<M> = M extends (param : string) => param is (infer U extends string) ? U : string;\n\n\texport interface AppTypes {{\n\t\tRouteId(): {};\n\t\tRouteParams(): {{\n\t\t\t{}\n\t\t}};\n\t\tLayoutParams(): {{\n\t\t\t{}\n\t\t}};\n\t\tPathname(): {};\n\t\tResolvedPathname(): `${{\"\" | `/${{string}}`}}${{ReturnType<AppTypes['Pathname']>}}`;\n\t\tAsset(): {};\n\t}}\n}}\n",
        generate_html_attributes(),
        union(route_ids),
        dynamic_routes.join(";\n\t\t\t"),
        layout_routes.join(";\n\t\t\t"),
        union(pathnames),
        asset_types.join(" | ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn environment_prefixes_and_values_are_separate() {
        let env = BTreeMap::from([
            ("PUBLIC_NAME".into(), "never write this value".into()),
            ("SECRET".into(), "secret value".into()),
            ("bad-key".into(), "x".into()),
            ("default".into(), "x".into()),
        ]);
        let result = generate_ambient(&env, &EnvPrefixes::default(), false);
        assert!(result.contains("export const PUBLIC_NAME: string;"));
        assert!(result.contains("export const SECRET: string;"));
        assert!(result.contains("export const default: string;"));
        assert!(!result.contains("bad-key"));
        assert!(!result.contains("secret value"));
        assert!(!result.contains("never write this value"));
        assert!(result.contains("[key: `PUBLIC_${string}`]: undefined;"));
        assert!(result.contains("[key: `${string}`]: string | undefined;"));
        assert!(!generate_ambient(&env, &EnvPrefixes::default(), true).contains("declare module"));
    }

    #[test]
    fn intersecting_prefixes_follow_kit_filtering() {
        let env = BTreeMap::from([
            ("APP_PUBLIC_KEY".into(), "".into()),
            ("APP_KEY".into(), "".into()),
        ]);
        let result = generate_ambient(
            &env,
            &EnvPrefixes {
                public: "APP_PUBLIC_".into(),
                private: "APP_".into(),
            },
            false,
        );
        assert!(!result.contains("export const APP_PUBLIC_KEY"));
        assert!(result.contains("export const APP_KEY"));
        assert!(result.contains("[key: `APP_"));
    }

    #[test]
    fn explicit_schema_types_preserve_inference_and_escape_docs() {
        let variable = ExplicitEnvVariable {
            name: "PORT".into(),
            public: false,
            description: Some("A */ port".into()),
            has_schema: true,
        };
        let output = generate_explicit_env(Some("../src/env.ts"), &[variable]).unwrap();
        assert!(output.contains(
            "StandardSchemaV1.InferOutput<typeof import(\"../src/env.ts\").variables.PORT.schema>"
        ));
        assert!(output.contains("A *\\/ port"));
        assert!(output.contains("// no public environment variables were defined"));
        for name in ["default", "1PORT", "a-b", "é"] {
            assert!(
                generate_explicit_env(
                    Some("../src/env.ts"),
                    &[ExplicitEnvVariable {
                        name: name.into(),
                        public: true,
                        description: None,
                        has_schema: false
                    }]
                )
                .is_err()
            );
        }
    }

    fn route(id: &str, params: Vec<AppTypeParam>) -> AppTypeRoute {
        AppTypeRoute {
            id: id.into(),
            params,
            page: Some(TrailingSlash::Unknown),
            endpoint: None,
        }
    }

    #[test]
    fn app_types_cover_layout_descendants_matchers_and_slashes() {
        let routes = vec![
            route("/", vec![]),
            route("/(group)", vec![]),
            route(
                "/(group)/[[lang=language]]/[id=integer]",
                vec![
                    AppTypeParam {
                        name: "lang".into(),
                        optional: true,
                        matcher: Some("language".into()),
                    },
                    AppTypeParam {
                        name: "id".into(),
                        optional: false,
                        matcher: Some("integer".into()),
                    },
                ],
            ),
        ];
        let output = generate_non_ambient(&routes, &["icon.svg".into()], "../src/params");
        assert!(output.contains("`${string}/${string}` & {}"));
        assert!(output.contains("`${string}/${string}/` & {}"));
        assert!(output.contains("\"/\": { lang?: MatcherParam<typeof import(\"../src/params/language.js\").match> | undefined; id?:"));
        assert!(
            output.contains("id: MatcherParam<typeof import(\"../src/params/integer.js\").match>")
        );
        assert!(output.contains("Asset(): \"/icon.svg\" | string & {}"));
    }

    #[test]
    fn layout_matcher_union_and_unmatched_descendant_widening() {
        let param = |matcher: Option<&str>| AppTypeParam {
            name: "id".into(),
            optional: false,
            matcher: matcher.map(str::to_owned),
        };
        let mut routes = vec![
            route("/", vec![]),
            route("/[id=a]", vec![param(Some("a"))]),
            route("/[id=b]", vec![param(Some("b"))]),
        ];
        let output = generate_non_ambient(&routes, &[], "../params");
        assert!(output.contains("\"/\": { id?: MatcherParam<typeof import(\"../params/a.js\").match> | MatcherParam<typeof import(\"../params/b.js\").match> | undefined }"));
        routes.push(route("/plain/[id]", vec![param(None)]));
        assert!(
            generate_non_ambient(&routes, &[], "../params")
                .contains("\"/\": { id?: string | undefined }")
        );
    }

    #[test]
    fn endpoint_slashes_groups_and_empty_manifest() {
        let routes = vec![
            AppTypeRoute {
                id: "/(admin)/health".into(),
                params: vec![],
                page: None,
                endpoint: Some(TrailingSlash::Never),
            },
            AppTypeRoute {
                id: "/about".into(),
                params: vec![],
                page: Some(TrailingSlash::Always),
                endpoint: None,
            },
        ];
        assert!(
            generate_non_ambient(&routes, &[], "")
                .contains("Pathname(): \"/health\" | \"/about/\";")
        );
        let empty = generate_non_ambient(&[], &[], "");
        assert!(empty.contains("RouteId(): never;"));
        assert!(empty.contains("Pathname(): never;"));
    }

    #[test]
    fn kit3_contract_separates_routes_layouts_assets_and_matcher_modules() {
        let routes = vec![
            VersionedAppTypeRoute {
                route: AppTypeRoute {
                    id: "/".into(),
                    params: vec![],
                    page: None,
                    endpoint: None,
                },
                layout: true,
            },
            VersionedAppTypeRoute {
                route: AppTypeRoute {
                    id: "/blog/[post-id=slug]".into(),
                    params: vec![AppTypeParam {
                        name: "post-id".into(),
                        optional: false,
                        matcher: Some("slug".into()),
                    }],
                    page: Some(TrailingSlash::Never),
                    endpoint: None,
                },
                layout: false,
            },
            VersionedAppTypeRoute {
                route: AppTypeRoute {
                    id: "/health".into(),
                    params: vec![],
                    page: None,
                    endpoint: Some(TrailingSlash::Never),
                },
                layout: false,
            },
        ];
        let output = generate_non_ambient_for_version(
            &routes,
            &["icon.svg".into()],
            "../../../src/params.ts",
            &KitSyncVersion::Kit3,
        )
        .unwrap();
        assert!(output.contains("PageRouteId(): \"/blog/[post-id=slug]\";"));
        assert!(output.contains("EndpointRouteId(): \"/health\";"));
        assert!(output.contains("RouteId(): \"/blog/[post-id=slug]\" | \"/health\";"));
        assert!(output.contains(
            "MatcherParam<(typeof import(\"../../../src/params.ts\").params)[\"slug\"]>"
        ));
        assert!(output.contains("\"/\": { \"post-id\"?:"));
        assert!(output.contains("Path(): `blog/${string}` & {} | \"health\";"));
        assert!(output.contains("AssetPath(): \"icon.svg\";"));
        assert!(!output.contains("string & {}"));
        assert!(output.contains("'data-sveltekit-reset'?: true | false"));
        assert!(!output.contains("data-sveltekit-keepfocus"));
        assert!(!output.contains("'off'"));
        assert!(output.contains("import './env.js';"));
        let prerelease = generate_non_ambient_for_version(
            &routes,
            &[],
            "../../../src/params.ts",
            &KitSyncVersion::Kit3Prerelease,
        )
        .unwrap();
        assert!(prerelease.contains("import './env';"));
    }

    #[test]
    fn kit3_optional_rest_and_escaped_pathnames() {
        let patterns = kit3_path_patterns("/(group)/[[locale]]/blog/[...rest]").unwrap();
        assert_eq!(
            patterns.into_iter().map(|p| p.template).collect::<Vec<_>>(),
            vec![
                "${string}/blog/${string}",
                "${string}/blog",
                "blog/${string}",
                "blog"
            ]
        );
        let patterns = kit3_path_patterns("/a[x+2f]b/[u+1f600]/pre[id]-suffix").unwrap();
        assert_eq!(patterns[0].template, "a%2Fb/😀/pre${string}-suffix");
        assert!(kit3_path_patterns("/[u+110000]").is_err());
        let root = generate_non_ambient_for_version(
            &[VersionedAppTypeRoute {
                route: route("/", vec![]),
                layout: true,
            }],
            &[],
            "params.js",
            &KitSyncVersion::Kit3,
        )
        .unwrap();
        assert!(root.contains("Path(): \"\" | \"/\";"));
        assert!(root.contains("AssetPath(): never;"));
    }

    #[test]
    fn kit3_environment_modules_are_explicit_only_and_empty_modules_exist() {
        let env = BTreeMap::from([("PUBLIC_NAME".into(), "".into())]);
        assert!(
            generate_ambient_for_version(
                &env,
                &EnvPrefixes::default(),
                false,
                &KitSyncVersion::Kit3
            )
            .is_none()
        );
        let output = generate_explicit_env_for_version(None, &[], &KitSyncVersion::Kit3).unwrap();
        assert!(output.contains("declare module '$app/env/private'"));
        assert!(output.contains("declare module '$app/env/public'"));
        assert!(!output.contains("$env/"));
        assert_eq!(
            generate_explicit_env_for_version(None, &[], &KitSyncVersion::Kit2LegacyExports)
                .unwrap(),
            ENV_DOCS
        );
        assert_eq!(
            generate_ambient_for_version(
                &env,
                &EnvPrefixes::default(),
                false,
                &KitSyncVersion::Kit2DestructuredExports
            ),
            Some(generate_ambient(&env, &EnvPrefixes::default(), false))
        );
    }
}
