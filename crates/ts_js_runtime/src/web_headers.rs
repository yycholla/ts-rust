//! Native Fetch Headers capability: https://fetch.spec.whatwg.org/#headers-class
//! and Web IDL ByteString/sequence/record conversion. No JavaScript fallback or
//! host I/O. Raw lists preserve duplicate insertion order; exposed iteration
//! sorts lowercase names and combines values, except separate set-cookie values.
//! Constructors use guard `none`; native Fetch constructors can select guards.
//! Request/response/no-cors browser filtering is a separate capability.
//! Bounds: 100,000 headers/steps and 4 MiB accounted list/conversion data.
use boa_engine::{
    Context, JsArgs, JsData, JsError, JsNativeError, JsObject, JsResult, JsString, JsSymbol,
    JsValue, NativeFunction,
    builtins::{iterable::create_iter_result_object, object::OrdinaryObject},
    class::{Class, ClassBuilder},
    object::{FunctionObjectBuilder, builtins::JsArray},
    property::{Attribute, PropertyDescriptor},
};
use boa_gc::{Finalize, Trace};
use std::{cell::RefCell, collections::BTreeMap, rc::Rc};

const MAX_HEADERS: usize = 100_000;
const MAX_BYTES: usize = 4 * 1024 * 1024;
/// Names and values are bytes, not UTF-8. Names returned by this host are ASCII
/// lowercase; duplicate values, including Set-Cookie, remain separate entries.
pub type HeaderList = Vec<(Vec<u8>, Vec<u8>)>;
/// Recorded Fetch guard. This host currently uses the server profile: request,
/// response and no-cors guards retain all valid headers, including Set-Cookie.
/// Only Immutable restricts mutation; this does not claim browser filtering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Trace, Finalize)]
#[boa_gc(empty_trace)]
pub enum HeadersGuard {
    None,
    Request,
    RequestNoCors,
    Response,
    Immutable,
}

/// Fully converted Web IDL initializer, before Fetch's fill algorithm. Each
/// nested sequence retains its original arity and raw ByteStrings. No JS handles
/// are retained, but Trace permits this value in native GC-traced captures.
#[derive(Debug, Clone, Trace, Finalize)]
pub struct ConvertedHeadersInit {
    sequences: Vec<Vec<Vec<u8>>>,
}
impl ConvertedHeadersInit {
    /// Validate pair arity, HTTP names and values only when Fetch reaches fill.
    /// Conversion has already completed, so this operation executes no JS.
    pub fn to_header_list(&self) -> JsResult<HeaderList> {
        let mut list = Vec::with_capacity(self.sequences.len());
        for sequence in &self.sequences {
            if sequence.len() != 2 {
                return Err(type_error(
                    "Each header pair must contain exactly two values",
                ));
            }
            list.push((name(sequence[0].clone())?, normalize(sequence[1].clone())?));
        }
        validate_size(&list)?;
        Ok(list)
    }
}

#[derive(Debug)]
struct Data {
    list: HeaderList,
    guard: HeadersGuard,
    iteration_cache: Option<HeaderList>,
}
type State = Rc<RefCell<Data>>;
#[derive(Debug, Trace, Finalize, JsData)]
struct Headers {
    #[unsafe_ignore_trace]
    state: State,
}
#[derive(Debug, Clone, Copy)]
enum Iteration {
    Keys,
    Values,
    Entries,
}
#[derive(Debug, Trace, Finalize, JsData)]
struct HeadersIterator {
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
    JsNativeError::typ()
        .with_message(message.to_string())
        .into()
}
fn limit_error() -> JsError {
    JsNativeError::range()
        .with_message("Native Headers allocation/iteration limit exceeded")
        .into()
}
fn required(args: &[JsValue], count: usize) -> JsResult<()> {
    if args.len() < count {
        Err(type_error("Insufficient Headers method arguments"))
    } else {
        Ok(())
    }
}
fn state(this: &JsValue) -> JsResult<State> {
    this.as_object()
        .and_then(|object| {
            object
                .downcast_ref::<Headers>()
                .map(|data| data.state.clone())
        })
        .ok_or_else(|| type_error("Headers method requires a Headers receiver"))
}
fn byte_string(value: &JsValue, context: &mut Context) -> JsResult<Vec<u8>> {
    let value = value.to_string(context)?;
    convert_byte_string(value)
}
fn convert_byte_string(value: JsString) -> JsResult<Vec<u8>> {
    if value.len() > MAX_BYTES {
        return Err(limit_error());
    }
    value
        .iter()
        .map(|unit| {
            u8::try_from(unit)
                .map_err(|_| type_error("Headers ByteString contains a character above U+00FF"))
        })
        .collect()
}
fn js_bytes(bytes: &[u8]) -> JsValue {
    JsString::from(
        bytes
            .iter()
            .map(|b| u16::from(*b))
            .collect::<Vec<_>>()
            .as_slice(),
    )
    .into()
}
fn name(mut bytes: Vec<u8>) -> JsResult<Vec<u8>> {
    if bytes.is_empty()
        || bytes.iter().any(|b| {
            !b.is_ascii_alphanumeric()
                && !matches!(
                    b,
                    b'!' | b'#'
                        | b'$'
                        | b'%'
                        | b'&'
                        | b'\''
                        | b'*'
                        | b'+'
                        | b'-'
                        | b'.'
                        | b'^'
                        | b'_'
                        | b'`'
                        | b'|'
                        | b'~'
                )
        })
    {
        return Err(type_error("Invalid HTTP header name"));
    }
    bytes.make_ascii_lowercase();
    Ok(bytes)
}
fn normalize(value: Vec<u8>) -> JsResult<Vec<u8>> {
    let trim = |b: &u8| matches!(b, b' ' | b'\t' | b'\r' | b'\n');
    let start = value.iter().position(|b| !trim(b)).unwrap_or(value.len());
    let end = value
        .iter()
        .rposition(|b| !trim(b))
        .map_or(start, |i| i + 1);
    let value = &value[start..end];
    if value.iter().any(|b| matches!(b, 0 | b'\r' | b'\n')) {
        return Err(type_error("Invalid HTTP header value"));
    }
    Ok(value.to_vec())
}
fn charge(size: &mut usize, bytes: usize) -> JsResult<()> {
    *size = size.checked_add(bytes).ok_or_else(limit_error)?;
    if *size > MAX_BYTES {
        Err(limit_error())
    } else {
        Ok(())
    }
}
fn validate_size(list: &HeaderList) -> JsResult<()> {
    if list.len() > MAX_HEADERS {
        return Err(limit_error());
    }
    let mut size = 0;
    for (name, value) in list {
        charge(&mut size, name.len() + value.len() + 2)?;
    }
    Ok(())
}
fn normalized_list(list: HeaderList) -> JsResult<HeaderList> {
    validate_size(&list)?;
    list.into_iter()
        .map(|(key, value)| Ok((name(key)?, normalize(value)?)))
        .collect()
}
fn mutate(state: &State, edit: impl FnOnce(&mut HeaderList)) -> JsResult<()> {
    let mut data = state.borrow_mut();
    if data.guard == HeadersGuard::Immutable {
        return Err(type_error("Headers are immutable"));
    }
    let mut next = data.list.clone();
    edit(&mut next);
    validate_size(&next)?;
    data.list = next;
    data.iteration_cache = None;
    Ok(())
}
fn combine<'a>(values: impl Iterator<Item = &'a Vec<u8>>) -> Option<Vec<u8>> {
    let mut combined = None::<Vec<u8>>;
    for value in values {
        if let Some(combined) = combined.as_mut() {
            combined.extend_from_slice(b", ");
            combined.extend_from_slice(value);
        } else {
            combined = Some(value.clone());
        }
    }
    combined
}
fn sorted(list: &HeaderList) -> HeaderList {
    let mut names = BTreeMap::<&[u8], Vec<&Vec<u8>>>::new();
    for (name, value) in list {
        names.entry(name).or_default().push(value);
    }
    let mut output = vec![];
    for (name, values) in names {
        if name == b"set-cookie" {
            for value in values {
                output.push((name.to_vec(), value.clone()));
            }
        } else {
            output.push((
                name.to_vec(),
                combine(values.into_iter()).expect("one or more values"),
            ));
        }
    }
    output
}

fn item(state: &State, index: usize) -> Option<(Vec<u8>, Vec<u8>)> {
    let mut data = state.borrow_mut();
    if data.iteration_cache.is_none() {
        data.iteration_cache = Some(sorted(&data.list));
    }
    data.iteration_cache.as_ref().unwrap().get(index).cloned()
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
    fn close_limit<T>(&self, context: &mut Context) -> JsResult<T> {
        // Host-imposed early termination closes acquired iterators, preserving
        // the original RangeError if return's getter/call/result also fails.
        if let Ok(value) = self.iterator.get(JsString::from("return"), context)
            && let Some(method) = value.as_callable()
        {
            let _ = method.call(&self.iterator.clone().into(), &[], context);
        }
        Err(limit_error())
    }
}
fn iterator_method(value: &JsValue, context: &mut Context) -> JsResult<Option<JsObject>> {
    let object = value
        .as_object()
        .ok_or_else(|| type_error("Headers initializer must be an object"))?;
    let method = object.get(JsSymbol::iterator(), context)?;
    if method.is_null_or_undefined() {
        return Ok(None);
    }
    method
        .as_callable()
        .map(Some)
        .ok_or_else(|| type_error("Symbol.iterator must be callable"))
}
// Web IDL converts each complete nested sequence, and the complete outer
// sequence, before Headers' fill algorithm checks pair arity/HTTP validity.
// Abrupt JS conversion/IteratorStepValue errors do not call IteratorClose in
// Web IDL's synchronous sequence algorithm (unlike ECMAScript Array.from).
/// Perform only Web IDL HeadersInit conversion. In RequestInit/ResponseInit,
/// call at the dictionary member's lexicographic position and defer fill until
/// the constructor algorithm reaches it. Undefined/null are not HeadersInit;
/// optional constructor arguments handle omission before calling this helper.
pub fn convert_headers_init(
    init: &JsValue,
    context: &mut Context,
) -> JsResult<ConvertedHeadersInit> {
    let object = init
        .as_object()
        .ok_or_else(|| type_error("Headers initializer must be an object"))?;
    let mut size = 0;
    if let Some(method) = iterator_method(init, context)? {
        let outer = Cursor::new(init, &method, context)?;
        let mut sequences = vec![];
        while let Some(value) = outer.step(context)? {
            if sequences.len() >= MAX_HEADERS {
                return outer.close_limit(context);
            }
            let method = iterator_method(&value, context)?
                .ok_or_else(|| type_error("Each header pair must be iterable"))?;
            let inner = Cursor::new(&value, &method, context)?;
            let mut sequence = vec![];
            while let Some(value) = inner.step(context)? {
                // JS coercion errors propagate unchanged; the separate native
                // size check identifies a host-imposed abort without confusing
                // it with a RangeError deliberately thrown by user coercion.
                let string = value.to_string(context)?;
                if string.len() > MAX_BYTES {
                    let _: JsResult<()> = inner.close_limit(context);
                    return outer.close_limit(context);
                }
                let bytes = convert_byte_string(string)?;
                if sequence.len() >= MAX_HEADERS
                    || charge(&mut size, bytes.len() + std::mem::size_of::<Vec<u8>>()).is_err()
                {
                    let _: JsResult<()> = inner.close_limit(context);
                    return outer.close_limit(context);
                }
                sequence.push(bytes);
            }
            sequences.push(sequence);
        }
        return Ok(ConvertedHeadersInit { sequences });
    }
    let keys = object.own_property_keys(context)?;
    if keys.len() > MAX_HEADERS {
        return Err(limit_error());
    }
    let mut converted = vec![];
    for key in keys {
        let value = JsValue::from(key.clone());
        if !OrdinaryObject::property_is_enumerable(init, std::slice::from_ref(&value), context)?
            .to_boolean()
        {
            continue;
        }
        let key_bytes = byte_string(&value, context)?;
        let value_bytes = byte_string(&object.get(key, context)?, context)?;
        charge(&mut size, key_bytes.len() + value_bytes.len() + 2)?;
        converted.push(vec![key_bytes, value_bytes]);
    }
    Ok(ConvertedHeadersInit {
        sequences: converted,
    })
}
fn init_list(init: &JsValue, context: &mut Context) -> JsResult<HeaderList> {
    if init.is_undefined() {
        return Ok(vec![]);
    }
    convert_headers_init(init, context)?.to_header_list()
}

#[derive(Clone, Copy)]
enum Method {
    Append,
    Delete,
    Get,
    GetSetCookie,
    Has,
    Set,
    ForEach,
}
fn method(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
    operation: Method,
) -> JsResult<JsValue> {
    let state = state(this)?;
    if matches!(operation, Method::GetSetCookie) {
        let values = state
            .borrow()
            .list
            .iter()
            .filter(|(name, _)| name == b"set-cookie")
            .map(|(_, value)| js_bytes(value))
            .collect::<Vec<_>>();
        return Ok(JsArray::from_iter(values, context).into());
    }
    if matches!(operation, Method::ForEach) {
        required(args, 1)?;
        let callback = args[0]
            .as_callable()
            .ok_or_else(|| type_error("Headers forEach callback must be callable"))?;
        let mut index = 0;
        loop {
            let pair = item(&state, index);
            let Some((name, value)) = pair else {
                break;
            };
            if index >= MAX_HEADERS {
                return Err(limit_error());
            }
            callback.call(
                args.get_or_undefined(1),
                &[js_bytes(&value), js_bytes(&name), this.clone()],
                context,
            )?;
            index += 1;
        }
        return Ok(JsValue::undefined());
    }
    let changes_value = matches!(operation, Method::Append | Method::Set);
    required(args, if changes_value { 2 } else { 1 })?;
    let raw_name = byte_string(&args[0], context)?;
    let raw_value = if changes_value {
        Some(byte_string(&args[1], context)?)
    } else {
        None
    };
    let name = name(raw_name)?;
    let value = raw_value.map(normalize).transpose()?;
    match operation {
        Method::Append => mutate(&state, |list| list.push((name, value.unwrap())))?,
        Method::Delete => mutate(&state, |list| list.retain(|(key, _)| key != &name))?,
        Method::Set => mutate(&state, |list| {
            let mut value = Some(value.unwrap());
            list.retain_mut(|(key, old)| {
                if key != &name {
                    return true;
                }
                if let Some(value) = value.take() {
                    *old = value;
                    true
                } else {
                    false
                }
            });
            if let Some(value) = value {
                list.push((name, value));
            }
        })?,
        Method::Get => {
            return Ok(combine(
                state
                    .borrow()
                    .list
                    .iter()
                    .filter(|(key, _)| key == &name)
                    .map(|(_, value)| value),
            )
            .map_or(JsValue::null(), |v| js_bytes(&v)));
        }
        Method::Has => {
            return Ok(state
                .borrow()
                .list
                .iter()
                .any(|(key, _)| key == &name)
                .into());
        }
        _ => unreachable!(),
    }
    Ok(JsValue::undefined())
}
impl Class for Headers {
    const NAME: &'static str = "Headers";
    const ATTRIBUTES: Attribute = Attribute::WRITABLE.union(Attribute::CONFIGURABLE);
    fn data_constructor(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<Self> {
        Ok(Self {
            state: Rc::new(RefCell::new(Data {
                list: init_list(args.get_or_undefined(0), context)?,
                guard: HeadersGuard::None,
                iteration_cache: None,
            })),
        })
    }
    fn init(class: &mut ClassBuilder) -> JsResult<()> {
        for (name, length, operation) in [
            ("append", 2, Method::Append),
            ("delete", 1, Method::Delete),
            ("get", 1, Method::Get),
            ("getSetCookie", 0, Method::GetSetCookie),
            ("has", 1, Method::Has),
            ("set", 2, Method::Set),
            ("forEach", 1, Method::ForEach),
        ] {
            class.method(
                JsString::from(name),
                length,
                NativeFunction::from_copy_closure(move |this, args, context| {
                    method(this, args, context, operation)
                }),
            );
        }
        for (name, kind) in [
            ("keys", Iteration::Keys),
            ("values", Iteration::Values),
            ("entries", Iteration::Entries),
        ] {
            class.method(
                JsString::from(name),
                0,
                NativeFunction::from_copy_closure(move |this, _, context| {
                    iterator(this, kind, context)
                }),
            );
        }
        class.property(
            JsSymbol::to_string_tag(),
            JsString::from("Headers"),
            Attribute::CONFIGURABLE,
        );
        Ok(())
    }
}
fn iterator(this: &JsValue, kind: Iteration, context: &mut Context) -> JsResult<JsValue> {
    let state = state(this)?;
    let prototype = context
        .get_data::<Registration>()
        .ok_or_else(|| type_error("Headers capability is not registered"))?
        .iterator_prototype
        .clone();
    Ok(JsObject::from_proto_and_data(
        prototype,
        HeadersIterator {
            state,
            kind,
            index: 0,
            done: false,
        },
    )
    .into())
}
fn next(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let object = this
        .as_object()
        .ok_or_else(|| type_error("Invalid Headers iterator receiver"))?;
    let (pair, kind) = {
        let mut data = object
            .downcast_mut::<HeadersIterator>()
            .ok_or_else(|| type_error("Invalid Headers iterator receiver"))?;
        let pair = if data.done {
            None
        } else {
            item(&data.state, data.index)
        };
        if pair.is_some() {
            if data.index >= MAX_HEADERS {
                return Err(limit_error());
            }
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
        Iteration::Keys => js_bytes(&name),
        Iteration::Values => js_bytes(&value),
        Iteration::Entries => {
            JsArray::from_iter([js_bytes(&name), js_bytes(&value)], context).into()
        }
    };
    Ok(create_iter_result_object(value, false, context))
}

/// Construct a native Headers object from copied, validated raw bytes. This does
/// not execute the global Headers binding or user-provided constructor code.
pub fn from_header_list(list: HeaderList, context: &mut Context) -> JsResult<JsObject> {
    from_header_list_with_guard(list, HeadersGuard::None, context)
}
/// Build an independent native list with its initial Fetch guard. Initializing
/// immutable generated headers is permitted; subsequent mutations are blocked.
pub fn from_header_list_with_guard(
    list: HeaderList,
    guard: HeadersGuard,
    context: &mut Context,
) -> JsResult<JsObject> {
    let list = normalized_list(list)?;
    Headers::from_data(
        Headers {
            state: Rc::new(RefCell::new(Data {
                list,
                guard,
                iteration_cache: None,
            })),
        },
        context,
    )
}
/// Fill an existing list in order, as Fetch specifies. Each append is atomic;
/// if a later pair fails validation, earlier successful appends remain.
pub fn fill_headers(value: &JsValue, init: &ConvertedHeadersInit) -> JsResult<()> {
    state(value)?;
    for sequence in &init.sequences {
        if sequence.len() != 2 {
            return Err(type_error(
                "Each header pair must contain exactly two values",
            ));
        }
        append_header_bytes(value, &sequence[0], &sequence[1])?;
    }
    Ok(())
}
/// Construct with delayed HTTP validation and the selected server-profile guard.
/// Immutable is applied after initializing the list.
pub fn from_converted_headers_init(
    init: &ConvertedHeadersInit,
    guard: HeadersGuard,
    context: &mut Context,
) -> JsResult<JsObject> {
    let list = init.to_header_list()?;
    from_header_list_with_guard(list, guard, context)
}
/// Native append bypasses mutable JavaScript methods but preserves HTTP byte
/// validation, normalization, allocation bounds, guard and live iterators.
pub fn append_header_bytes(value: &JsValue, raw_name: &[u8], raw_value: &[u8]) -> JsResult<()> {
    let state = state(value)?;
    if raw_name.len() > MAX_BYTES || raw_value.len() > MAX_BYTES {
        return Err(limit_error());
    }
    let name = name(raw_name.to_vec())?;
    let value = normalize(raw_value.to_vec())?;
    mutate(&state, |list| list.push((name, value)))
}
/// Inspect the actual native guard, without replaceable property access.
pub fn header_guard(value: &JsValue) -> JsResult<HeadersGuard> {
    Ok(state(value)?.borrow().guard)
}
/// Make an independent copy of the raw header list. None preserves the guard;
/// Some explicitly selects a new guard (e.g. a constructor rebuilding a response).
pub fn copy_headers(
    value: &JsValue,
    guard: Option<HeadersGuard>,
    context: &mut Context,
) -> JsResult<JsObject> {
    let state = state(value)?;
    let (list, guard) = {
        let data = state.borrow();
        (data.list.clone(), guard.unwrap_or(data.guard))
    };
    from_header_list_with_guard(list, guard, context)
}
/// Snapshot the actual header list without folding duplicates or user getters.
pub fn read_header_list(value: &JsValue) -> JsResult<HeaderList> {
    let state = state(value)?;
    let list = state.borrow().list.clone();
    Ok(list)
}
/// Freeze mutation for generated responses. Reading and copying remain allowed.
pub fn make_immutable(value: &JsValue) -> JsResult<()> {
    state(value)?.borrow_mut().guard = HeadersGuard::Immutable;
    Ok(())
}

/// Install only on explicit host opt-in; never replace an existing global.
pub fn register(context: &mut Context) -> JsResult<()> {
    if context.has_data::<Registration>()
        || context
            .global_object()
            .has_property(JsString::from("Headers"), context)?
    {
        return Err(type_error(
            "Headers capability would overwrite an existing registration/global",
        ));
    }
    context.register_global_class::<Headers>()?;
    let prototype = context
        .get_global_class::<Headers>()
        .expect("registered")
        .prototype();
    for name in [
        "append",
        "delete",
        "get",
        "getSetCookie",
        "has",
        "set",
        "forEach",
        "keys",
        "values",
        "entries",
    ] {
        prototype.define_property_or_throw(
            JsString::from(name),
            PropertyDescriptor::builder().enumerable(true),
            context,
        )?;
    }
    let entries = prototype.get(JsString::from("entries"), context)?;
    prototype.define_property_or_throw(
        JsSymbol::iterator(),
        PropertyDescriptor::builder()
            .value(entries)
            .writable(true)
            .configurable(true)
            .enumerable(false),
        context,
    )?;
    let iterator_prototype = JsObject::from_proto_and_data(
        context.intrinsics().constructors().iterator().prototype(),
        OrdinaryObject,
    );
    let next = FunctionObjectBuilder::new(context.realm(), NativeFunction::from_fn_ptr(next))
        .name(JsString::from("next"))
        .build();
    iterator_prototype.define_property_or_throw(
        JsString::from("next"),
        PropertyDescriptor::builder()
            .value(next)
            .writable(true)
            .configurable(true)
            .enumerable(false),
        context,
    )?;
    iterator_prototype.define_property_or_throw(
        JsSymbol::to_string_tag(),
        PropertyDescriptor::builder()
            .value(JsString::from("Headers Iterator"))
            .configurable(true),
        context,
    )?;
    context.insert_data(Registration { iterator_prototype });
    Ok(())
}
#[cfg(test)]
#[path = "web_headers_tests.rs"]
mod tests;
