//! Native port of SvelteKit's postbuild route analysis (MIT, Svelte contributors).
//! References: Kit 2.67.0–2.70.3 and 3.0.0-next.32/3.0.1, core/postbuild/analyse.js,
//! utils/page_nodes.js, utils/exports.js and utils/routing.js. Profiles are pinned:
//! Kit 3 reverses config precedence, adds QUERY, forbids prerendered fallback,
//! expands route escapes and accepts scalar parameter values.
//!
//! This operation analyses actual loaded modules, awaits actual endpoint/entries
//! functions, and retains JS values. Adapter feature checks and remote-function
//! metadata are separate operations and are not claimed by this result. Inputs
//! must belong to this session. No application loads/actions/handlers are invoked.
use crate::{Result, get, invalid, object};
use boa_engine::{
    Context, JsNativeError, JsObject, JsString, JsValue, NativeFunction,
    builtins::object::OrdinaryObject as Object,
    object::{FunctionObjectBuilder, builtins::JsArray},
    property::PropertyKey,
};
use boa_gc::{Finalize, GcRefCell, Trace};
use serde::{Deserialize, Serialize};
use ts_js_runtime::native_session::NativeSession;

const MAX_ITEMS: usize = 16_384;
const MAX_STRING: usize = 1024 * 1024;
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub enum MetadataProfile {
    #[serde(rename = "2.67.0")]
    Kit267,
    #[serde(rename = "2.68.0")]
    Kit268,
    #[serde(rename = "2.69.2")]
    Kit2692,
    #[serde(rename = "2.69.3")]
    Kit2693,
    #[serde(rename = "2.70.3")]
    Kit2703,
    #[serde(rename = "3.0.0-next.32")]
    Kit3Next32,
    #[serde(rename = "3.0.1")]
    Kit301,
}
impl MetadataProfile {
    pub fn is_kit3(self) -> bool {
        matches!(self, Self::Kit3Next32 | Self::Kit301)
    }
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MetadataOptions {
    pub profile: MetadataProfile,
    #[serde(default)]
    pub hash: bool,
}
/// These are native engine values, not JSON substitutes. Keep them in the owning
/// runtime; normalizing to JSON is an explicit host-boundary operation.
pub struct MetadataAnalysis {
    pub routes: Vec<RouteMetadata>,
}
pub struct RouteMetadata {
    pub id: String,
    pub config: JsValue,
    pub methods: Vec<String>,
    pub page_methods: Vec<String>,
    pub api_methods: Vec<String>,
    pub prerender: JsValue,
    /// Undefined, a falsy export, or the actual mapped entries array (with holes).
    pub entries: JsValue,
    /// Retained ancestry for explicit supplementary page_options/page_actions.
    pub page_nodes: Option<Vec<Option<JsObject>>>,
    /// Real endpoint module for supplementary options (no synthetic page defaults).
    pub endpoint: Option<JsObject>,
}
pub struct PageOptions {
    pub csr: JsValue,
    pub ssr: JsValue,
    pub prerender: JsValue,
    pub trailing_slash: JsValue,
}
#[derive(Debug, Serialize)]
pub struct ActionFact {
    pub name: String,
    pub callable: bool,
}
/// Supplementary action inventory. This does not invoke actions and does not
/// replace Kit's runtime action validation. Absent/falsy actions yield no names.
pub fn page_actions(
    s: &mut NativeSession<'_>,
    nodes: &[Option<JsObject>],
) -> Result<Vec<ActionFact>> {
    let server = node_module(s, nodes.last().and_then(Option::as_ref), "server")?;
    let actions = prop(s, &server, "actions")?;
    if !actions.to_boolean() {
        return Ok(vec![]);
    }
    let mut result = vec![];
    for name in own_keys(s, &actions)? {
        let callable = prop(s, &actions, &name)?.is_callable();
        result.push(ActionFact { name, callable });
    }
    Ok(result)
}
struct Part {
    config: JsValue,
    entries: JsValue,
    methods: Vec<String>,
    prerender: JsValue,
}
fn text(value: &JsString) -> Result<String> {
    if value.len() > MAX_STRING {
        return Err(invalid("Metadata string exceeds native limit"));
    }
    value
        .to_std_string()
        .map_err(|_| invalid("Metadata identity contains unpaired UTF-16 surrogate"))
}
fn prop(s: &mut NativeSession<'_>, value: &JsValue, name: &str) -> Result<JsValue> {
    if value.is_null_or_undefined() {
        return Ok(JsValue::undefined());
    }
    Ok(value
        .to_object(s.context())?
        .get(JsString::from(name), s.context())?)
}
fn own_keys(s: &mut NativeSession<'_>, value: &JsValue) -> Result<Vec<String>> {
    let keys = Object::keys(
        &JsValue::undefined(),
        std::slice::from_ref(value),
        s.context(),
    )?;
    let keys = JsArray::from_object(object(keys, "Object.keys result")?)?;
    let len = keys.length(s.context())?;
    if len > MAX_ITEMS as u64 {
        return Err(invalid("Metadata export count exceeds native limit"));
    }
    (0..len)
        .map(|i| text(&keys.get(i, s.context())?.as_string().unwrap()))
        .collect()
}
/// Enumerate string exports, including inherited enumerable exports. A shadowing
/// non-enumerable property suppresses its inherited namesake, as for-in does.
fn exported_keys(s: &mut NativeSession<'_>, value: &JsValue) -> Result<Vec<String>> {
    if !value.to_boolean() {
        return Ok(vec![]);
    }
    let mut current = Some(value.to_object(s.context())?);
    let mut visited = Vec::<JsString>::new();
    let mut result = vec![];
    let mut depth = 0;
    while let Some(obj) = current {
        depth += 1;
        if depth > 128 {
            return Err(invalid(
                "Metadata export prototype depth exceeds native limit",
            ));
        }
        for key in obj.own_property_keys(s.context())? {
            if matches!(key, PropertyKey::Symbol(_)) {
                continue;
            }
            let key_value: JsValue = key.into();
            let key = key_value.to_string(s.context())?;
            if visited.contains(&key) {
                continue;
            }
            if visited.len() == MAX_ITEMS {
                return Err(invalid("Metadata export count exceeds native limit"));
            }
            let descriptor = Object::get_own_property_descriptor(
                &JsValue::undefined(),
                &[obj.clone().into(), key.clone().into()],
                s.context(),
            )?;
            if descriptor.is_undefined() {
                continue;
            }
            visited.push(key.clone());
            if prop(s, &descriptor, "enumerable")?.to_boolean() {
                result.push(text(&key)?);
            }
        }
        current = Object::get_prototype_of(&JsValue::undefined(), &[obj.into()], s.context())?
            .as_object();
    }
    Ok(result)
}
const BASE: &[&str] = &["load", "prerender", "csr", "ssr", "trailingSlash", "config"];
const ENDPOINT: &[&str] = &[
    "GET",
    "POST",
    "PATCH",
    "PUT",
    "DELETE",
    "OPTIONS",
    "HEAD",
    "fallback",
    "prerender",
    "trailingSlash",
    "config",
    "entries",
];
fn allowed(kind: usize, kit3: bool) -> Vec<&'static str> {
    let mut result = if kind == 4 {
        ENDPOINT.to_vec()
    } else {
        BASE.to_vec()
    };
    if kind == 1 || kind == 3 {
        result.push("entries");
    }
    if kind == 3 {
        result.insert(BASE.len(), "actions");
    }
    if kind == 4 && kit3 {
        result.insert(7, "QUERY");
    }
    result
}
fn validate(
    s: &mut NativeSession<'_>,
    module: &JsValue,
    file: &JsValue,
    kind: usize,
    kit3: bool,
) -> Result<()> {
    let expected = allowed(kind, kit3);
    for key in exported_keys(s, module)? {
        if key.starts_with('_') || expected.contains(&key.as_str()) {
            continue;
        }
        let file = if file.to_boolean() {
            text(&file.to_string(s.context())?)?
        } else {
            String::new()
        };
        let ext = if file.is_empty() {
            ".js"
        } else {
            file.rfind('.').map(|i| &file[i..]).unwrap_or("")
        };
        let locations: Vec<_> = [
            "+layout",
            "+page",
            "+layout.server",
            "+page.server",
            "+server",
        ]
        .into_iter()
        .enumerate()
        .filter(|(kind, _)| allowed(*kind, kit3).contains(&key.as_str()))
        .map(|(_, name)| format!("{name}{ext}"))
        .collect();
        let hint = if locations.is_empty() {
            format!(
                "valid exports are {}, or anything with a '_' prefix",
                expected.join(", ")
            )
        } else {
            let (last, others) = locations.split_last().unwrap();
            format!(
                "'{key}' is a valid export in {}{last}",
                if others.is_empty() {
                    String::new()
                } else {
                    format!("{} or ", others.join(", "))
                }
            )
        };
        return Err(invalid(format!(
            "Invalid export '{key}'{} ({hint})",
            if file.is_empty() {
                String::new()
            } else {
                format!(" in {file}")
            }
        )));
    }
    Ok(())
}
fn node_module(
    s: &mut NativeSession<'_>,
    node: Option<&JsObject>,
    module: &str,
) -> Result<JsValue> {
    match node {
        Some(node) => get(s, node, module),
        None => Ok(JsValue::undefined()),
    }
}
fn option(
    s: &mut NativeSession<'_>,
    nodes: &[Option<JsObject>],
    name: &str,
    default: JsValue,
) -> Result<JsValue> {
    let mut result = default;
    for node in nodes {
        let universal = node_module(s, node.as_ref(), "universal")?;
        let value = prop(s, &universal, name)?;
        if !value.is_null_or_undefined() {
            result = value;
            continue;
        }
        let server = node_module(s, node.as_ref(), "server")?;
        let value = prop(s, &server, name)?;
        if !value.is_null_or_undefined() {
            result = value;
        }
    }
    Ok(result)
}
/// PageNodes option inheritance, separately callable without adding observable
/// property reads to postbuild analysis (which only reads prerender).
pub fn page_options(s: &mut NativeSession<'_>, nodes: &[Option<JsObject>]) -> Result<PageOptions> {
    Ok(PageOptions {
        csr: option(s, nodes, "csr", true.into())?,
        ssr: option(s, nodes, "ssr", true.into())?,
        prerender: option(s, nodes, "prerender", false.into())?,
        trailing_slash: option(s, nodes, "trailingSlash", JsString::from("never").into())?,
    })
}
fn page_config(
    s: &mut NativeSession<'_>,
    nodes: &[Option<JsObject>],
    kit3: bool,
) -> Result<JsValue> {
    let mut current = JsObject::with_object_proto(s.context().intrinsics());
    for node in nodes {
        let universal = node_module(s, node.as_ref(), "universal")?;
        if !prop(s, &universal, "config")?.to_boolean() {
            let server = node_module(s, node.as_ref(), "server")?;
            if !prop(s, &server, "config")?.to_boolean() {
                continue;
            }
        }
        let next = JsObject::with_object_proto(s.context().intrinsics());
        next.copy_data_properties(&current.into(), Vec::<PropertyKey>::new(), s.context())?;
        for module in if kit3 {
            ["server", "universal"]
        } else {
            ["universal", "server"]
        } {
            let module = node_module(s, node.as_ref(), module)?;
            let config = prop(s, &module, "config")?;
            next.copy_data_properties(&config, Vec::<PropertyKey>::new(), s.context())?;
        }
        current = next;
    }
    if own_keys(s, &current.clone().into())?.is_empty() {
        Ok(JsValue::undefined())
    } else {
        Ok(current.into())
    }
}
fn analyse_page(s: &mut NativeSession<'_>, nodes: &[Option<JsObject>], kit3: bool) -> Result<Part> {
    let leaf = nodes.last().and_then(Option::as_ref);
    let mut methods = vec!["GET".into()];
    let server = node_module(s, leaf, "server")?;
    if prop(s, &server, "actions")?.to_boolean() {
        methods.push("POST".into());
    }
    for (index, node) in nodes.iter().enumerate() {
        let Some(node) = node else { continue };
        let page = index + 1 == nodes.len();
        for (module, kind) in [
            ("server", if page { 3 } else { 2 }),
            ("universal", if page { 1 } else { 0 }),
        ] {
            let value = get(s, node, module)?;
            let file = get(s, node, &format!("{module}_id"))?;
            validate(s, &value, &file, kind, kit3)?;
        }
    }
    let config = page_config(s, nodes, kit3)?;
    let universal = node_module(s, leaf, "universal")?;
    let mut entries = prop(s, &universal, "entries")?;
    if entries.is_null_or_undefined() {
        let server = node_module(s, leaf, "server")?;
        entries = prop(s, &server, "entries")?;
    }
    Ok(Part {
        config,
        entries,
        methods,
        prerender: option(s, nodes, "prerender", false.into())?,
    })
}
fn analyse_endpoint(
    s: &mut NativeSession<'_>,
    module: &JsObject,
    id: &str,
    kit3: bool,
) -> Result<Part> {
    validate(
        s,
        &module.clone().into(),
        &JsString::from(id).into(),
        4,
        kit3,
    )?;
    if get(s, module, "prerender")?.to_boolean() {
        let mut forbidden = kit3 && get(s, module, "fallback")?.to_boolean();
        for method in if kit3 {
            &["POST", "PUT", "PATCH", "DELETE", "QUERY"][..]
        } else {
            &["POST", "PATCH", "PUT", "DELETE"][..]
        } {
            if forbidden {
                break;
            }
            forbidden = get(s, module, method)?.to_boolean();
        }
        if forbidden {
            return Err(invalid(format!(
                "Cannot prerender a +server file with {} ({id})",
                if kit3 {
                    "POST, PUT, PATCH, DELETE, QUERY, or fallback"
                } else {
                    "POST, PATCH, PUT, or DELETE"
                }
            )));
        }
    }
    let mut methods = vec![];
    for method in [
        "GET", "POST", "PUT", "PATCH", "DELETE", "OPTIONS", "HEAD", "QUERY",
    ] {
        if method == "QUERY" && !kit3 {
            continue;
        }
        if get(s, module, method)?.to_boolean() {
            methods.push(method.into());
        }
    }
    if get(s, module, "fallback")?.to_boolean() {
        methods.push("*".into());
    }
    let config = get(s, module, "config")?;
    let entries = get(s, module, "entries")?;
    let prerender = get(s, module, "prerender")?;
    Ok(Part {
        config,
        entries,
        methods,
        prerender: if prerender.is_null_or_undefined() {
            false.into()
        } else {
            prerender
        },
    })
}
fn stringify(s: &mut NativeSession<'_>, value: JsValue) -> Result<JsValue> {
    let global = s.context().global_object();
    let json = object(get(s, &global, "JSON")?, "JSON")?;
    let function = get(s, &json, "stringify")?;
    s.call(&function, &json.into(), &[value])
}
fn match_configs(
    s: &mut NativeSession<'_>,
    page: &JsValue,
    endpoint: &JsValue,
    id: &str,
) -> Result<()> {
    if !page.to_boolean() || !endpoint.to_boolean() {
        return Ok(());
    }
    let union = JsObject::with_object_proto(s.context().intrinsics());
    union.copy_data_properties(page, Vec::<PropertyKey>::new(), s.context())?;
    union.copy_data_properties(endpoint, Vec::<PropertyKey>::new(), s.context())?;
    for key in exported_keys(s, &union.into())? {
        let a = prop(s, page, &key)?;
        let a = stringify(s, a)?;
        let b = prop(s, endpoint, &key)?;
        let b = stringify(s, b)?;
        if !JsValue::strict_equals(&a, &b) {
            return Err(invalid(format!(
                "Mismatched route config for {id} — the +page and +server files must export the same config, if any"
            )));
        }
    }
    Ok(())
}
fn array(s: &mut NativeSession<'_>, value: JsValue, label: &str) -> Result<(JsArray, u64)> {
    let array = JsArray::from_object(object(value, label)?)?;
    let len = array.length(s.context())?;
    if len > MAX_ITEMS as u64 {
        return Err(invalid(format!("{label} exceeds native limit")));
    }
    Ok((array, len))
}
fn node_index(value: JsValue, nodes: &[JsObject], optional: bool) -> Result<Option<JsObject>> {
    if optional && value.is_undefined() {
        return Ok(None);
    }
    let n = value
        .as_number()
        .ok_or_else(|| invalid("Manifest node index must be an integer"))?;
    if n < 0.0 || n.fract() != 0.0 || n >= nodes.len() as f64 {
        return Err(invalid("Manifest node index is outside loaded nodes"));
    }
    Ok(Some(nodes[n as usize].clone()))
}
/// Analyse all routes in manifest order. Call after the initialized runtime has
/// loaded nodes; feature/remote checks remain explicit separate responsibilities.
pub fn analyse(
    s: &mut NativeSession<'_>,
    manifest: &JsObject,
    nodes: &[JsObject],
    options: &MetadataOptions,
) -> Result<MetadataAnalysis> {
    let kit3 = options.profile.is_kit3();
    if nodes.len() > 4096 {
        return Err(invalid("Node count exceeds 4096"));
    }
    if options.hash {
        for node in nodes {
            let universal = get(s, node, "universal")?;
            if !universal.to_boolean() {
                continue;
            }
            let keys: Vec<_> = own_keys(s, &universal)?
                .into_iter()
                .filter(|s| s != "load")
                .collect();
            if !keys.is_empty() {
                let file = get(s, node, "universal_id")?;
                let file = text(&file.to_string(s.context())?)?;
                return Err(invalid(format!(
                    "Page options are ignored when `router.type === 'hash'` ({file} has {})",
                    keys.iter()
                        .map(|key| format!("'{key}'"))
                        .collect::<Vec<_>>()
                        .join(", ")
                )));
            }
        }
    }
    let container = if kit3 {
        manifest.clone()
    } else {
        object(get(s, manifest, "_")?, "Manifest internals")?
    };
    let route_values = get(s, &container, "routes")?;
    let (routes, len) = array(s, route_values, "Manifest routes")?;
    let mut output = vec![];
    for i in 0..len {
        let route = object(routes.get(i, s.context())?, "Manifest route")?;
        let id = get(s, &route, "id")?;
        let id = text(
            &id.as_string()
                .ok_or_else(|| invalid("Route id must be a string"))?,
        )?;
        let mut page_nodes = None;
        let mut endpoint_module = None;
        let page = get(s, &route, "page")?;
        let page = if page.to_boolean() {
            let page = object(page, "Route page")?;
            let layouts = get(s, &page, "layouts")?;
            let (layouts, len) = array(s, layouts, "Route layouts")?;
            let mut ancestors = vec![];
            for i in 0..len {
                ancestors.push(node_index(layouts.get(i, s.context())?, nodes, true)?);
            }
            ancestors.push(node_index(get(s, &page, "leaf")?, nodes, false)?);
            let result = analyse_page(s, &ancestors, kit3)?;
            page_nodes = Some(ancestors);
            Some(result)
        } else {
            None
        };
        let endpoint = get(s, &route, "endpoint")?;
        let endpoint = if endpoint.to_boolean() {
            let pending = s.call(&endpoint, &route.clone().into(), &[])?;
            let module = object(s.settle(pending)?, "Endpoint module")?;
            let result = analyse_endpoint(s, &module, &id, kit3)?;
            endpoint_module = Some(module);
            Some(result)
        } else {
            None
        };
        if let (Some(page), Some(endpoint)) = (&page, &endpoint) {
            if page.prerender.to_boolean() && endpoint.prerender.to_boolean() {
                return Err(invalid(format!(
                    "Cannot prerender a route with both +page and +server files ({id})"
                )));
            }
            match_configs(s, &page.config, &endpoint.config, &id)?;
        }
        let choose = |page: Option<&JsValue>, endpoint: Option<&JsValue>, default: JsValue| {
            page.filter(|x| !x.is_null_or_undefined())
                .or(endpoint.filter(|x| !x.is_null_or_undefined()))
                .cloned()
                .unwrap_or(default)
        };
        let config = choose(
            page.as_ref().map(|p| &p.config),
            endpoint.as_ref().map(|p| &p.config),
            JsObject::with_object_proto(s.context().intrinsics()).into(),
        );
        let prerender = choose(
            page.as_ref().map(|p| &p.prerender),
            endpoint.as_ref().map(|p| &p.prerender),
            JsValue::undefined(),
        );
        let entries = page
            .as_ref()
            .map(|p| &p.entries)
            .filter(|v| !v.is_null_or_undefined())
            .or_else(|| endpoint.as_ref().map(|p| &p.entries))
            .cloned()
            .unwrap_or_else(JsValue::undefined);
        let entries = if entries.to_boolean() {
            let pending = s.call(&entries, &JsValue::undefined(), &[])?;
            let value = s.settle(pending)?;
            if let Some(value) = value.as_object().filter(|value| value.is_array())
                && JsArray::from_object(value)?.length(s.context())? > MAX_ITEMS as u64
            {
                return Err(invalid("Entries array exceeds native limit"));
            }
            // Call the actual map method. This preserves sparse arrays, inherited
            // entries, Array subclasses/species and explicitly overridden map.
            let map = value
                .to_object(s.context())?
                .get(JsString::from("map"), s.context())?;
            let callback = FunctionObjectBuilder::new(
                s.context().realm(),
                NativeFunction::from_copy_closure_with_captures(
                    |_, args, capture, context| {
                        let mut count = capture.count.borrow_mut();
                        if *count >= MAX_ITEMS {
                            return Err(JsNativeError::range()
                                .with_message("Entries exceed native limit")
                                .into());
                        }
                        *count += 1;
                        drop(count);
                        resolve_route_context(
                            context,
                            &capture.id,
                            args.first().unwrap_or(&JsValue::undefined()),
                            capture.profile,
                        )
                        .map(JsValue::from)
                        .map_err(|error| JsNativeError::error().with_message(error.message).into())
                    },
                    EntryMapper {
                        id: id.clone(),
                        profile: options.profile,
                        count: GcRefCell::new(0),
                    },
                ),
            )
            .length(1)
            .build();
            s.call(&map, &value, &[callback.into()])?
        } else {
            entries
        };
        let page_methods = page.map(|p| p.methods).unwrap_or_default();
        let api_methods = endpoint.map(|p| p.methods).unwrap_or_default();
        let mut methods = page_methods.clone();
        for method in &api_methods {
            if !methods.contains(method) {
                methods.push(method.clone());
            }
        }
        output.push(RouteMetadata {
            id,
            config,
            methods,
            page_methods,
            api_methods,
            prerender,
            entries,
            page_nodes,
            endpoint: endpoint_module,
        });
    }
    Ok(MetadataAnalysis { routes: output })
}

#[derive(Trace, Finalize)]
struct EntryMapper {
    #[unsafe_ignore_trace]
    id: String,
    #[unsafe_ignore_trace]
    profile: MetadataProfile,
    count: GcRefCell<usize>,
}
fn call(
    context: &mut Context,
    function: &JsValue,
    this: &JsValue,
    args: &[JsValue],
) -> Result<JsValue> {
    Ok(function
        .as_callable()
        .ok_or_else(|| invalid("Expected callable route operation"))?
        .call(this, args, context)?)
}

// Equivalent to the pinned basic_param_pattern, whose \w is ASCII in JS.
fn param_at(segment: &str, start: usize) -> Option<(usize, bool, bool, &str)> {
    let bytes = segment.as_bytes();
    let mut i = start + 1;
    let optional = bytes.get(i) == Some(&b'[');
    if optional {
        i += 1;
    }
    let rest = segment[i..].starts_with("...");
    if rest {
        i += 3;
    }
    let begin = i;
    while bytes
        .get(i)
        .is_some_and(|c| c.is_ascii_alphanumeric() || *c == b'_' || *c == b'-')
    {
        i += 1;
    }
    if i == begin {
        return None;
    }
    let name = &segment[begin..i];
    if bytes.get(i) == Some(&b'=') {
        i += 1;
        let matcher = i;
        while bytes
            .get(i)
            .is_some_and(|c| c.is_ascii_alphanumeric() || *c == b'_' || *c == b'-')
        {
            i += 1;
        }
        if matcher == i {
            return None;
        }
    }
    if bytes.get(i) != Some(&b']') {
        return None;
    }
    i += 1;
    if bytes.get(i) == Some(&b']') {
        i += 1;
    }
    Some((i, optional, rest, name))
}
/// Kit's resolve_route does not URL-encode supplied parameters. Kit 3 only
/// encodes reserved characters produced by static [x+]/[u+] route escapes.
pub fn resolve_route(
    s: &mut NativeSession<'_>,
    id: &str,
    params: &JsValue,
    profile: MetadataProfile,
) -> Result<JsString> {
    resolve_route_context(s.context(), id, params, profile)
}
fn resolve_route_context(
    context: &mut Context,
    id: &str,
    params: &JsValue,
    profile: MetadataProfile,
) -> Result<JsString> {
    if !id.starts_with('/') || id.len() > MAX_STRING {
        return Err(invalid("Invalid route identity"));
    }
    let mut result = vec![b'/' as u16];
    let mut emitted = false;
    for segment in id[1..].split('/') {
        if segment.is_empty()
            || (segment.starts_with('(')
                && segment.ends_with(')')
                && segment.len() > 2
                && !segment[1..segment.len() - 1].contains(')'))
        {
            continue;
        }
        let mut rendered = Vec::<u16>::new();
        let mut i = 0;
        while i < segment.len() {
            if segment.as_bytes()[i] == b'[' {
                if profile.is_kit3()
                    && (segment[i..].starts_with("[x+") || segment[i..].starts_with("[u+"))
                    && let Some(end) = segment[i + 3..].find(']')
                    && end > 0
                {
                    let code = &segment[i + 3..i + 3 + end];
                    for point in code.split('-') {
                        // Upstream parseInt(codepoint,16), including numeric-prefix parsing.
                        let global = context.global_object();
                        let parse = global.get(JsString::from("parseInt"), context)?;
                        let value = call(
                            context,
                            &parse,
                            &JsValue::undefined(),
                            &[JsString::from(point).into(), 16.into()],
                        )?;
                        let n = value
                            .as_number()
                            .ok_or_else(|| invalid("Invalid route escape"))?;
                        if !n.is_finite() || n < 0.0 || n > 0x10ffff as f64 {
                            return Err(invalid("Invalid route escape codepoint"));
                        }
                        let n = n as u32;
                        match n {
                            37 => rendered.extend("%25".encode_utf16()),
                            47 => rendered.extend("%2F".encode_utf16()),
                            63 => rendered.extend("%3F".encode_utf16()),
                            35 => rendered.extend("%23".encode_utf16()),
                            0..=0xffff => rendered.push(n as u16),
                            _ => {
                                let n = n - 0x10000;
                                rendered.push(0xd800 + (n >> 10) as u16);
                                rendered.push(0xdc00 + (n & 1023) as u16);
                            }
                        }
                    }
                    i += end + 4;
                    continue;
                }
                if let Some((end, optional, rest, name)) = param_at(segment, i) {
                    // Not optional chaining: undefined/null entry objects throw.
                    let params = params.to_object(context)?;
                    let value = params.get(JsString::from(name), context)?;
                    let missing = if profile.is_kit3() {
                        value.is_undefined() || value.as_string().is_some_and(|v| v.is_empty())
                    } else {
                        !value.to_boolean()
                    };
                    if missing {
                        if optional || (rest && !value.is_undefined()) {
                            i = end;
                            continue;
                        }
                        return Err(invalid(format!("Missing parameter '{name}' in route {id}")));
                    }
                    if profile.is_kit3()
                        && !value.is_string()
                        && !value.is_number()
                        && !value.is_boolean()
                        && !value.is_bigint()
                    {
                        return Err(invalid(format!(
                            "Invalid value for parameter '{name}' in route {id}"
                        )));
                    }
                    let slash = if let Some(value) = value.as_string() {
                        value.iter().next() == Some(b'/' as u16)
                            || value.iter().last() == Some(b'/' as u16)
                    } else if !profile.is_kit3() {
                        let starts = value
                            .to_object(context)?
                            .get(JsString::from("startsWith"), context)?;
                        let starts = call(context, &starts, &value, &[JsString::from("/").into()])?
                            .to_boolean();
                        if starts {
                            true
                        } else {
                            let ends = value
                                .to_object(context)?
                                .get(JsString::from("endsWith"), context)?;
                            call(context, &ends, &value, &[JsString::from("/").into()])?
                                .to_boolean()
                        }
                    } else {
                        false
                    };
                    if slash {
                        return Err(invalid(format!(
                            "Parameter '{name}' in route {id} cannot start or end with a slash -- this would cause an invalid route like foo//bar"
                        )));
                    }
                    let value = value.to_string(context)?;
                    if rendered.len().saturating_add(value.len()) > MAX_STRING {
                        return Err(invalid("Resolved route exceeds native limit"));
                    }
                    rendered.extend(value.iter());
                    i = end;
                    continue;
                }
            }
            let ch = segment[i..].chars().next().unwrap();
            let mut units = [0u16; 2];
            rendered.extend_from_slice(ch.encode_utf16(&mut units));
            i += ch.len_utf8();
        }
        if !rendered.is_empty() {
            if emitted {
                result.push(b'/' as u16);
            }
            result.extend(rendered);
            emitted = true;
        }
        if result.len() > MAX_STRING {
            return Err(invalid("Resolved route exceeds native limit"));
        }
    }
    if id != "/" && id.ends_with('/') {
        result.push(b'/' as u16);
    }
    Ok(JsString::from(result.as_slice()))
}
#[cfg(test)]
#[path = "metadata_tests.rs"]
mod tests;
