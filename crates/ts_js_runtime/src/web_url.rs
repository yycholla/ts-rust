//! Explicit WHATWG URL host capability, implemented in Rust over `url::quirks`.
//! No network, files, environment, or JavaScript implementation is involved.
//! Strings use Web IDL USVString conversion (unpaired surrogates become U+FFFD).
//! Host allocations are bounded to 4 MiB serialized data and 100,000 pairs;
//! exceeding a bound throws RangeError rather than truncating a URL or list.
use boa_engine::{
    Context, JsArgs, JsData, JsError, JsNativeError, JsObject, JsResult, JsString, JsSymbol,
    JsValue, NativeFunction,
    builtins::{iterable::create_iter_result_object, object::OrdinaryObject as Object},
    class::{Class, ClassBuilder},
    object::{FunctionObjectBuilder, builtins::JsArray},
    property::{Attribute, PropertyDescriptor},
};
use boa_gc::{Finalize, Trace};
use std::{cell::RefCell, rc::Rc};
use url::{Url, form_urlencoded, quirks};

const MAX_BYTES: usize = 4 * 1024 * 1024;
const MAX_PAIRS: usize = 100_000;
type Pairs = Vec<(String, String)>;

#[derive(Debug)]
struct Shared {
    url: Option<Url>,
    pairs: Pairs,
}
type State = Rc<RefCell<Shared>>;

#[derive(Debug, Trace, Finalize, JsData)]
struct WebUrl {
    // Contains only native Url/String data, never GC handles.
    #[unsafe_ignore_trace]
    state: State,
    params: Option<JsObject>,
}
#[derive(Debug, Trace, Finalize, JsData)]
struct SearchParams {
    #[unsafe_ignore_trace]
    state: State,
}

/// Read branded URLSearchParams data for Fetch without invoking replaceable
/// JavaScript iterators, getters, or serialization methods.
pub fn search_params_snapshot(value: &JsValue) -> Option<Vec<(String, String)>> {
    let object = value.as_object()?;
    let params = object.downcast_ref::<SearchParams>()?;
    Some(params.state.borrow().pairs.clone())
}
#[derive(Debug, Clone, Copy)]
enum Iteration {
    Entries,
    Keys,
    Values,
}
#[derive(Debug, Trace, Finalize, JsData)]
struct ParamsIterator {
    #[unsafe_ignore_trace]
    state: State,
    #[unsafe_ignore_trace]
    kind: Iteration,
    index: usize,
    done: bool,
}
struct Registration {
    iterator_prototype: JsObject,
}

fn type_error(message: &str) -> JsError {
    JsNativeError::typ().with_message(message.to_owned()).into()
}
fn limit_error() -> JsError {
    JsNativeError::range()
        .with_message("Native URL host allocation/iteration limit exceeded")
        .into()
}
fn bounded(text: &str) -> JsResult<()> {
    if text.len() > MAX_BYTES {
        Err(limit_error())
    } else {
        Ok(())
    }
}
fn string(value: &str) -> JsValue {
    JsString::from(value).into()
}
fn usv(value: &JsValue, context: &mut Context) -> JsResult<String> {
    let text = value.to_string(context)?;
    if text.len() > MAX_BYTES {
        return Err(limit_error());
    }
    let text = text.to_std_string_lossy();
    bounded(&text)?;
    Ok(text)
}
fn required(args: &[JsValue], count: usize) -> JsResult<()> {
    if args.len() < count {
        Err(type_error("Insufficient URL method arguments"))
    } else {
        Ok(())
    }
}
fn url_state(this: &JsValue) -> JsResult<State> {
    this.as_object()
        .and_then(|object| {
            object
                .downcast_ref::<WebUrl>()
                .map(|data| data.state.clone())
        })
        .ok_or_else(|| type_error("URL method requires a URL receiver"))
}
fn params_state(this: &JsValue) -> JsResult<State> {
    this.as_object()
        .and_then(|object| {
            object
                .downcast_ref::<SearchParams>()
                .map(|data| data.state.clone())
        })
        .ok_or_else(|| type_error("URLSearchParams method requires a URLSearchParams receiver"))
}
fn parse_pairs(query: &str) -> JsResult<Pairs> {
    bounded(query)?;
    let pairs = form_urlencoded::parse(query.as_bytes())
        .into_owned()
        .take(MAX_PAIRS + 1)
        .collect::<Pairs>();
    if pairs.len() > MAX_PAIRS {
        return Err(limit_error());
    }
    serialize(&pairs)?;
    Ok(pairs)
}
fn serialize(pairs: &Pairs) -> JsResult<String> {
    if pairs.len() > MAX_PAIRS {
        return Err(limit_error());
    }
    let mut size = 0usize;
    for (name, value) in pairs {
        size = size
            .checked_add(name.len())
            .and_then(|size| size.checked_add(value.len()))
            .ok_or_else(limit_error)?;
        if size > MAX_BYTES {
            return Err(limit_error());
        }
    }
    let text = form_urlencoded::Serializer::new(String::new())
        .extend_pairs(pairs)
        .finish();
    bounded(&text)?;
    Ok(text)
}
fn mutate(state: &State, update: impl FnOnce(&mut Pairs)) -> JsResult<()> {
    let mut current = state.borrow_mut();
    let mut pairs = current.pairs.clone();
    update(&mut pairs);
    let serialized = serialize(&pairs)?;
    let mut url = current.url.clone();
    if let Some(url) = &mut url {
        url.set_query((!serialized.is_empty()).then_some(serialized.as_str()));
        bounded(url.as_str())?;
    }
    current.pairs = pairs;
    current.url = url;
    Ok(())
}

#[derive(Clone, Copy)]
enum Property {
    Href,
    Origin,
    Protocol,
    Username,
    Password,
    Host,
    Hostname,
    Port,
    Pathname,
    Search,
    Hash,
}
fn url_get(this: &JsValue, property: Property) -> JsResult<JsValue> {
    let state = url_state(this)?;
    let state = state.borrow();
    let url = state.url.as_ref().expect("URL owns a native URL");
    Ok(match property {
        Property::Href => string(quirks::href(url)),
        Property::Origin => string(&quirks::origin(url)),
        Property::Protocol => string(quirks::protocol(url)),
        Property::Username => string(quirks::username(url)),
        Property::Password => string(quirks::password(url)),
        Property::Host => string(quirks::host(url)),
        Property::Hostname => string(quirks::hostname(url)),
        Property::Port => string(quirks::port(url)),
        Property::Pathname => string(quirks::pathname(url)),
        Property::Search => string(quirks::search(url)),
        Property::Hash => string(quirks::hash(url)),
    })
}
fn url_set(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
    property: Property,
) -> JsResult<JsValue> {
    let state = url_state(this)?;
    let value = usv(args.get_or_undefined(0), context)?;
    let mut state = state.borrow_mut();
    let mut url = state.url.as_ref().expect("URL owns a native URL").clone();
    // WHATWG component setters silently ignore invalid values; href throws.
    match property {
        Property::Href => {
            quirks::set_href(&mut url, &value).map_err(|_| type_error("Invalid URL"))?
        }
        Property::Protocol => {
            let _ = quirks::set_protocol(&mut url, &value);
        }
        Property::Username => {
            let _ = quirks::set_username(&mut url, &value);
        }
        Property::Password => {
            let _ = quirks::set_password(&mut url, &value);
        }
        Property::Host => {
            let _ = quirks::set_host(&mut url, &value);
        }
        Property::Hostname => {
            let _ = quirks::set_hostname(&mut url, &value);
        }
        Property::Port => {
            let _ = quirks::set_port(&mut url, &value);
        }
        Property::Pathname => quirks::set_pathname(&mut url, &value),
        Property::Search => quirks::set_search(&mut url, &value),
        Property::Hash => quirks::set_hash(&mut url, &value),
        Property::Origin => return Err(type_error("URL origin has no setter")),
    }
    bounded(url.as_str())?;
    if matches!(property, Property::Href | Property::Search) {
        state.pairs = parse_pairs(url.query().unwrap_or_default())?;
    }
    state.url = Some(url);
    Ok(JsValue::undefined())
}
fn parse_url(args: &[JsValue], context: &mut Context) -> JsResult<Option<Url>> {
    required(args, 1)?;
    let input = usv(&args[0], context)?;
    let base = if args.get(1).is_some_and(|base| !base.is_undefined()) {
        let base = usv(&args[1], context)?;
        match Url::parse(&base) {
            Ok(base) => Some(base),
            Err(_) => return Ok(None),
        }
    } else {
        None
    };
    let parsed = Url::options().base_url(base.as_ref()).parse(&input).ok();
    if let Some(url) = &parsed {
        bounded(url.as_str())?;
    }
    Ok(parsed)
}
impl WebUrl {
    fn data(url: Url) -> JsResult<Self> {
        let pairs = parse_pairs(url.query().unwrap_or_default())?;
        Ok(Self {
            state: Rc::new(RefCell::new(Shared {
                url: Some(url),
                pairs,
            })),
            params: None,
        })
    }
}
impl Class for WebUrl {
    const NAME: &'static str = "URL";
    const LENGTH: usize = 1;
    const ATTRIBUTES: Attribute = Attribute::WRITABLE.union(Attribute::CONFIGURABLE);
    fn data_constructor(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<Self> {
        Self::data(parse_url(args, context)?.ok_or_else(|| type_error("Invalid URL"))?)
    }
    fn object_constructor(
        instance: &JsObject<Self>,
        _: &[JsValue],
        context: &mut Context,
    ) -> JsResult<()> {
        let state = instance.borrow().data().state.clone();
        let params = SearchParams::from_data(SearchParams { state }, context)?;
        instance.borrow_mut().data_mut().params = Some(params);
        Ok(())
    }
    fn init(class: &mut ClassBuilder) -> JsResult<()> {
        for (name, property) in [
            ("href", Property::Href),
            ("origin", Property::Origin),
            ("protocol", Property::Protocol),
            ("username", Property::Username),
            ("password", Property::Password),
            ("host", Property::Host),
            ("hostname", Property::Hostname),
            ("port", Property::Port),
            ("pathname", Property::Pathname),
            ("search", Property::Search),
            ("hash", Property::Hash),
        ] {
            let getter = FunctionObjectBuilder::new(
                class.context().realm(),
                NativeFunction::from_copy_closure(move |this, _, _| url_get(this, property)),
            )
            .name(JsString::from(format!("get {name}")))
            .build();
            let setter = if matches!(property, Property::Origin) {
                None
            } else {
                Some(
                    FunctionObjectBuilder::new(
                        class.context().realm(),
                        NativeFunction::from_copy_closure(move |this, args, context| {
                            url_set(this, args, context, property)
                        }),
                    )
                    .name(JsString::from(format!("set {name}")))
                    .length(1)
                    .build(),
                )
            };
            class.accessor(
                JsString::from(name),
                Some(getter),
                setter,
                Attribute::ENUMERABLE.union(Attribute::CONFIGURABLE),
            );
        }
        let getter = FunctionObjectBuilder::new(
            class.context().realm(),
            NativeFunction::from_fn_ptr(|this, _, _| {
                let object = this
                    .as_object()
                    .ok_or_else(|| type_error("Invalid URL receiver"))?;
                let data = object
                    .downcast_ref::<WebUrl>()
                    .ok_or_else(|| type_error("Invalid URL receiver"))?;
                Ok(data
                    .params
                    .clone()
                    .expect("URL initializes searchParams")
                    .into())
            }),
        )
        .name(JsString::from("get searchParams"))
        .build();
        class.accessor(
            JsString::from("searchParams"),
            Some(getter),
            None,
            Attribute::ENUMERABLE.union(Attribute::CONFIGURABLE),
        );
        for name in ["toString", "toJSON"] {
            class.method(
                JsString::from(name),
                0,
                NativeFunction::from_fn_ptr(|this, _, _| url_get(this, Property::Href)),
            );
        }
        class.static_method(
            JsString::from("canParse"),
            1,
            NativeFunction::from_fn_ptr(|_, args, context| {
                Ok(parse_url(args, context)?.is_some().into())
            }),
        );
        class.static_method(
            JsString::from("parse"),
            1,
            NativeFunction::from_fn_ptr(|_, args, context| match parse_url(args, context)? {
                Some(url) => WebUrl::from_data(WebUrl::data(url)?, context).map(Into::into),
                None => Ok(JsValue::null()),
            }),
        );
        class.property(
            JsSymbol::to_string_tag(),
            JsString::from("URL"),
            Attribute::CONFIGURABLE,
        );
        Ok(())
    }
}

struct Cursor {
    iterator: JsObject,
    next: JsObject,
}
impl Cursor {
    fn new(value: &JsValue, method: &JsObject, context: &mut Context) -> JsResult<Self> {
        let iterator = method
            .call(value, &[], context)?
            .as_object()
            .ok_or_else(|| type_error("Iterator must return an object"))?;
        let next = iterator
            .get(JsString::from("next"), context)?
            .as_callable()
            .ok_or_else(|| type_error("Iterator next must be callable"))?;
        Ok(Self { iterator, next })
    }
    fn step(&self, context: &mut Context) -> JsResult<Option<JsValue>> {
        let result = self
            .next
            .call(&self.iterator.clone().into(), &[], context)?
            .as_object()
            .ok_or_else(|| type_error("Iterator result must be an object"))?;
        if result.get(JsString::from("done"), context)?.to_boolean() {
            Ok(None)
        } else {
            result.get(JsString::from("value"), context).map(Some)
        }
    }
    fn close_error<T>(&self, error: JsError, context: &mut Context) -> JsResult<T> {
        // IteratorClose preserves an incoming throw even if return itself fails.
        if let Ok(value) = self.iterator.get(JsString::from("return"), context)
            && let Some(method) = value.as_callable()
        {
            let _ = method.call(&self.iterator.clone().into(), &[], context);
        }
        Err(error)
    }
}
fn iterator_method(value: &JsValue, context: &mut Context) -> JsResult<Option<JsObject>> {
    let method = value
        .to_object(context)?
        .get(JsSymbol::iterator(), context)?;
    if method.is_null_or_undefined() {
        return Ok(None);
    }
    method
        .as_callable()
        .map(Some)
        .ok_or_else(|| type_error("Symbol.iterator must be callable"))
}
fn add_size(size: &mut usize, amount: usize) -> JsResult<()> {
    *size = size.checked_add(amount).ok_or_else(limit_error)?;
    if *size > MAX_BYTES {
        Err(limit_error())
    } else {
        Ok(())
    }
}
fn pair(value: &JsValue, context: &mut Context, size: &mut usize) -> JsResult<Vec<String>> {
    if !value.is_object() {
        return Err(type_error("Each parameter pair must be an iterable object"));
    }
    let method = iterator_method(value, context)?
        .ok_or_else(|| type_error("Each parameter pair must be iterable"))?;
    let iterator = Cursor::new(value, &method, context)?;
    let mut values = vec![];
    while let Some(value) = iterator.step(context)? {
        // Web IDL sequence conversion propagates conversion failures directly;
        // unlike a JavaScript for-of abrupt completion it does not IteratorClose.
        let value = usv(&value, context)?;
        if let Err(error) = add_size(size, value.len() + std::mem::size_of::<String>()) {
            return iterator.close_error(error, context);
        }
        values.push(value);
        if values.len() > MAX_PAIRS {
            return iterator.close_error(limit_error(), context);
        }
    }
    // Web IDL converts the complete nested sequence before the URL algorithm
    // validates pair length. Do not skip later getters/iterator side effects.
    Ok(values)
}
fn constructor_pairs(args: &[JsValue], context: &mut Context) -> JsResult<Pairs> {
    let init = args.get_or_undefined(0);
    if init.is_undefined() {
        return Ok(vec![]);
    }
    if let Some(object) = init.as_object() {
        if let Some(method) = iterator_method(init, context)? {
            let iterator = Cursor::new(init, &method, context)?;
            let mut sequences = vec![];
            let mut size = 0;
            while let Some(value) = iterator.step(context)? {
                let item = pair(&value, context, &mut size)?;
                sequences.push(item);
                if sequences.len() > MAX_PAIRS {
                    return iterator.close_error(limit_error(), context);
                }
            }
            let pairs = sequences
                .into_iter()
                .map(|mut values| {
                    if values.len() != 2 {
                        return Err(type_error(
                            "Parameter pairs must contain exactly two values",
                        ));
                    }
                    Ok((values.remove(0), values.remove(0)))
                })
                .collect::<JsResult<Pairs>>()?;
            serialize(&pairs)?;
            return Ok(pairs);
        }
        let keys = object.own_property_keys(context)?;
        if keys.len() > MAX_PAIRS {
            return Err(limit_error());
        }
        let mut pairs: Pairs = vec![];
        let mut positions = std::collections::BTreeMap::<String, usize>::new();
        let mut size = 0;
        for key in keys {
            let key_value = JsValue::from(key.clone());
            if !Object::property_is_enumerable(init, std::slice::from_ref(&key_value), context)?
                .to_boolean()
            {
                continue;
            }
            let name = usv(&key_value, context)?;
            let value = usv(&object.get(key, context)?, context)?;
            add_size(&mut size, name.len())?;
            add_size(&mut size, value.len())?;
            if let Some(index) = positions.get(&name) {
                pairs[*index].1 = value;
            } else {
                positions.insert(name.clone(), pairs.len());
                pairs.push((name, value));
            }
        }
        serialize(&pairs)?;
        return Ok(pairs);
    }
    let string = usv(init, context)?;
    parse_pairs(string.strip_prefix('?').unwrap_or(&string))
}

#[derive(Clone, Copy)]
enum ParamsMethod {
    Append,
    Delete,
    Get,
    GetAll,
    Has,
    Set,
    Sort,
    String,
    ForEach,
}
fn params_method(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
    method: ParamsMethod,
) -> JsResult<JsValue> {
    let state = params_state(this)?;
    if matches!(method, ParamsMethod::String) {
        return Ok(string(&serialize(&state.borrow().pairs)?));
    }
    if matches!(method, ParamsMethod::Sort) {
        mutate(&state, |pairs| {
            pairs.sort_by(|a, b| a.0.encode_utf16().cmp(b.0.encode_utf16()))
        })?;
        return Ok(JsValue::undefined());
    }
    if matches!(method, ParamsMethod::ForEach) {
        required(args, 1)?;
        let callback = args[0]
            .as_callable()
            .ok_or_else(|| type_error("forEach callback must be callable"))?;
        let mut index = 0;
        loop {
            let pair = state.borrow().pairs.get(index).cloned();
            let Some((name, value)) = pair else { break };
            if index >= MAX_PAIRS {
                return Err(limit_error());
            }
            callback.call(
                args.get_or_undefined(1),
                &[string(&value), string(&name), this.clone()],
                context,
            )?;
            index += 1;
        }
        return Ok(JsValue::undefined());
    }
    required(
        args,
        if matches!(method, ParamsMethod::Append | ParamsMethod::Set) {
            2
        } else {
            1
        },
    )?;
    let name = usv(&args[0], context)?;
    let value = if matches!(method, ParamsMethod::Append | ParamsMethod::Set)
        || args.get(1).is_some_and(|v| !v.is_undefined())
            && matches!(method, ParamsMethod::Delete | ParamsMethod::Has)
    {
        Some(usv(&args[1], context)?)
    } else {
        None
    };
    match method {
        ParamsMethod::Append => mutate(&state, |pairs| pairs.push((name, value.unwrap())))?,
        ParamsMethod::Delete => mutate(&state, |pairs| {
            pairs.retain(|(key, old)| {
                key != &name || value.as_ref().is_some_and(|value| value != old)
            })
        })?,
        ParamsMethod::Set => mutate(&state, |pairs| {
            let mut first = None;
            pairs.retain(|(key, _)| {
                if key == &name {
                    if first.is_none() {
                        first = Some(());
                        true
                    } else {
                        false
                    }
                } else {
                    true
                }
            });
            if let Some((_, old)) = pairs.iter_mut().find(|(key, _)| key == &name) {
                *old = value.unwrap();
            } else {
                pairs.push((name, value.unwrap()));
            }
        })?,
        ParamsMethod::Get => {
            return Ok(state
                .borrow()
                .pairs
                .iter()
                .find(|(key, _)| key == &name)
                .map_or(JsValue::null(), |(_, value)| string(value)));
        }
        ParamsMethod::GetAll => {
            let values = state
                .borrow()
                .pairs
                .iter()
                .filter(|(key, _)| key == &name)
                .map(|(_, value)| string(value))
                .collect::<Vec<_>>();
            return Ok(JsArray::from_iter(values, context).into());
        }
        ParamsMethod::Has => {
            return Ok(state
                .borrow()
                .pairs
                .iter()
                .any(|(key, old)| key == &name && value.as_ref().is_none_or(|value| value == old))
                .into());
        }
        _ => unreachable!(),
    }
    Ok(JsValue::undefined())
}
impl Class for SearchParams {
    const NAME: &'static str = "URLSearchParams";
    const ATTRIBUTES: Attribute = Attribute::WRITABLE.union(Attribute::CONFIGURABLE);
    fn data_constructor(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<Self> {
        Ok(Self {
            state: Rc::new(RefCell::new(Shared {
                url: None,
                pairs: constructor_pairs(args, context)?,
            })),
        })
    }
    fn init(class: &mut ClassBuilder) -> JsResult<()> {
        for (name, length, method) in [
            ("append", 2, ParamsMethod::Append),
            ("delete", 1, ParamsMethod::Delete),
            ("get", 1, ParamsMethod::Get),
            ("getAll", 1, ParamsMethod::GetAll),
            ("has", 1, ParamsMethod::Has),
            ("set", 2, ParamsMethod::Set),
            ("sort", 0, ParamsMethod::Sort),
            ("toString", 0, ParamsMethod::String),
            ("forEach", 1, ParamsMethod::ForEach),
        ] {
            class.method(
                JsString::from(name),
                length,
                NativeFunction::from_copy_closure(move |this, args, context| {
                    params_method(this, args, context, method)
                }),
            );
        }
        for (name, kind) in [
            ("entries", Iteration::Entries),
            ("keys", Iteration::Keys),
            ("values", Iteration::Values),
        ] {
            class.method(
                JsString::from(name),
                0,
                NativeFunction::from_copy_closure(move |this, _, context| {
                    iterator(this, kind, context)
                }),
            );
        }
        let getter = FunctionObjectBuilder::new(
            class.context().realm(),
            NativeFunction::from_fn_ptr(|this, _, _| {
                let state = params_state(this)?;
                let size = state.borrow().pairs.len();
                Ok((size as f64).into())
            }),
        )
        .name(JsString::from("get size"))
        .build();
        class.accessor(
            JsString::from("size"),
            Some(getter),
            None,
            Attribute::ENUMERABLE.union(Attribute::CONFIGURABLE),
        );
        class.property(
            JsSymbol::to_string_tag(),
            JsString::from("URLSearchParams"),
            Attribute::CONFIGURABLE,
        );
        Ok(())
    }
}
fn iterator(this: &JsValue, kind: Iteration, context: &mut Context) -> JsResult<JsValue> {
    let state = params_state(this)?;
    let prototype = context
        .get_data::<Registration>()
        .ok_or_else(|| type_error("URL host is not registered"))?
        .iterator_prototype
        .clone();
    Ok(JsObject::from_proto_and_data(
        prototype,
        ParamsIterator {
            state,
            kind,
            index: 0,
            done: false,
        },
    )
    .into())
}
fn iterator_next(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let object = this
        .as_object()
        .ok_or_else(|| type_error("Invalid URLSearchParams iterator receiver"))?;
    let (pair, kind) = {
        let mut data = object
            .downcast_mut::<ParamsIterator>()
            .ok_or_else(|| type_error("Invalid URLSearchParams iterator receiver"))?;
        let pair = if data.done {
            None
        } else {
            data.state.borrow().pairs.get(data.index).cloned()
        };
        if pair.is_some() {
            data.index += 1;
        } else {
            data.done = true;
        }
        (pair, data.kind)
    };
    let Some((name, value)) = pair else {
        return Ok(create_iter_result_object(
            JsValue::undefined(),
            true,
            context,
        ));
    };
    let value = match kind {
        Iteration::Keys => string(&name),
        Iteration::Values => string(&value),
        Iteration::Entries => JsArray::from_iter([string(&name), string(&value)], context).into(),
    };
    Ok(create_iter_result_object(value, false, context))
}

/// Register only when the host explicitly enables this capability. Existing
/// bindings are rejected so enabling URL cannot overwrite application globals.
pub fn register(context: &mut Context) -> JsResult<()> {
    if context.has_data::<Registration>() {
        return Err(type_error("URL host is already registered"));
    }
    for name in ["URL", "URLSearchParams"] {
        if context
            .global_object()
            .has_property(JsString::from(name), context)?
        {
            return Err(type_error("URL host would overwrite an existing global"));
        }
    }
    context.register_global_class::<SearchParams>()?;
    context.register_global_class::<WebUrl>()?;
    // Web IDL operations are enumerable, unlike ECMAScript built-in methods
    // (the native Class builder defaults to the latter).
    for (prototype, names) in [
        (
            context
                .get_global_class::<WebUrl>()
                .expect("registered")
                .prototype(),
            &["toString", "toJSON"][..],
        ),
        (
            context
                .get_global_class::<SearchParams>()
                .expect("registered")
                .prototype(),
            &[
                "append", "delete", "get", "getAll", "has", "set", "sort", "toString", "forEach",
                "entries", "keys", "values",
            ][..],
        ),
    ] {
        for name in names {
            prototype.define_property_or_throw(
                JsString::from(*name),
                PropertyDescriptor::builder().enumerable(true),
                context,
            )?;
        }
    }
    let url_constructor = context
        .get_global_class::<WebUrl>()
        .expect("registered")
        .constructor();
    for name in ["parse", "canParse"] {
        url_constructor.define_property_or_throw(
            JsString::from(name),
            PropertyDescriptor::builder().enumerable(true),
            context,
        )?;
    }
    let params = context
        .get_global_class::<SearchParams>()
        .expect("registered")
        .prototype();
    let entries = params.get(JsString::from("entries"), context)?;
    params.define_property_or_throw(
        JsSymbol::iterator(),
        PropertyDescriptor::builder()
            .value(entries)
            .writable(true)
            .configurable(true)
            .enumerable(false),
        context,
    )?;
    let prototype = JsObject::from_proto_and_data(
        context.intrinsics().constructors().iterator().prototype(),
        boa_engine::builtins::object::OrdinaryObject,
    );
    let next =
        FunctionObjectBuilder::new(context.realm(), NativeFunction::from_fn_ptr(iterator_next))
            .name(JsString::from("next"))
            .build();
    prototype.define_property_or_throw(
        JsString::from("next"),
        PropertyDescriptor::builder()
            .value(next)
            .writable(true)
            .configurable(true)
            .enumerable(false),
        context,
    )?;
    prototype.define_property_or_throw(
        JsSymbol::to_string_tag(),
        PropertyDescriptor::builder()
            .value(JsString::from("URLSearchParams Iterator"))
            .configurable(true),
        context,
    )?;
    context.insert_data(Registration {
        iterator_prototype: prototype,
    });
    Ok(())
}

#[cfg(test)]
#[path = "web_url_tests.rs"]
mod tests;
