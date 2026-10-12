//! Native Fetch Request/Response classes with an explicit server Headers profile.
//!
//! Implements Fetch §5.4/5.5 and Web IDL conversion order, using native Body,
//! Headers, URL and dependent-signal operations. There is deliberately no global
//! `fetch`: transport, redirects, caching and network response filtering belong
//! to a separate host capability. No application config, .env, filesystem or
//! network is consulted here. Register prerequisites (Text, URL, Blob, FormData,
//! Headers, Abort, Streams) before this capability and before application code.
//!
//! Multipart encoding requires an explicit host entropy callback. Each boundary
//! consumes 16 fresh bytes, hex-encoded without reducing their entropy; duplicate
//! output and field collisions trigger bounded retries. The host is responsible
//! for supplying unpredictable bytes. No deterministic source is called random.
//! Constructor nesting is limited to 64; strings/URLs to 1 MiB; 10,000 distinct
//! multipart boundaries per context. Body/Headers impose their own byte limits.
//!
//! Sources: https://fetch.spec.whatwg.org/#request-class,
//! https://fetch.spec.whatwg.org/#response-class and installed Undici 7.23.0.
use crate::{
    web_abort,
    web_fetch_body::{
        Body, BodyInit, BodyIntrinsics, BodySource, ConsumptionKind, ExtractedBody,
        ExtractionOptions,
    },
    web_headers::{self, ConvertedHeadersInit, HeadersGuard},
};
use boa_engine::{
    Context, JsArgs, JsData, JsError, JsNativeError, JsObject, JsResult, JsString, JsSymbol,
    JsValue, NativeFunction,
    class::{Class, ClassBuilder},
    object::{
        FunctionObjectBuilder,
        builtins::{JsPromise, JsUint8Array},
    },
    property::{Attribute, PropertyDescriptor},
};
use boa_gc::{Finalize, Gc, GcRefCell, Trace};
use std::{
    cell::{Cell, RefCell},
    collections::BTreeSet,
    rc::Rc,
};
use url::Url;

const MAX_STRING: usize = 1024 * 1024;
const MAX_BOUNDARIES: usize = 10_000;
/// One-thread native entropy provider. Errors propagate; no implicit I/O source
/// is installed. Host closures should retain native state, not untraced JS refs.
pub type MultipartEntropy = Rc<dyn Fn(&mut [u8]) -> Result<(), String>>;
#[derive(Clone, Default)]
pub struct FetchOptions {
    /// Absolute API base URL. None means relative Request/redirect URLs fail.
    pub base_url: Option<String>,
    /// Serialized environment origin. None derives it from base_url; "null"
    /// denotes opaque. This controls referrer semantics, not network authority.
    pub origin: Option<String>,
    pub multipart_entropy: Option<MultipartEntropy>,
}
struct Registration {
    intrinsics: BodyIntrinsics,
    base: Option<Url>,
    origin: Option<String>,
    entropy: Option<MultipartEntropy>,
    boundaries: RefCell<BTreeSet<String>>,
    depth: Rc<Cell<usize>>,
}
struct Depth(Rc<Cell<usize>>);
impl Drop for Depth {
    fn drop(&mut self) {
        self.0.set(self.0.get() - 1);
    }
}
fn enter(c: &Context) -> JsResult<Depth> {
    let depth = registration(c)?.depth.clone();
    if depth.get() >= 64 {
        return Err(limit("Fetch constructor nesting"));
    }
    depth.set(depth.get() + 1);
    Ok(Depth(depth))
}
fn registration(c: &Context) -> JsResult<&Registration> {
    c.get_data::<Registration>()
        .ok_or_else(|| typ("Fetch classes are not registered"))
}
fn typ(message: &str) -> JsError {
    JsNativeError::typ().with_message(message.to_owned()).into()
}
fn range(message: &str) -> JsError {
    JsNativeError::range()
        .with_message(message.to_owned())
        .into()
}
fn limit(what: &str) -> JsError {
    range(&format!("Native {what} limit exceeded"))
}
fn bound(size: usize) -> JsResult<()> {
    if size > MAX_STRING {
        Err(limit("Fetch string"))
    } else {
        Ok(())
    }
}
fn dom(value: &JsValue, c: &mut Context) -> JsResult<JsString> {
    let s = value.to_string(c)?;
    bound(s.len())?;
    Ok(s)
}
fn usv(value: &JsValue, c: &mut Context) -> JsResult<String> {
    let text = dom(value, c)?.to_std_string_lossy();
    bound(text.len())?;
    Ok(text)
}
fn bytes(value: &JsValue, c: &mut Context) -> JsResult<Vec<u8>> {
    dom(value, c)?
        .iter()
        .map(|v| {
            u8::try_from(v).map_err(|_| typ("Fetch ByteString contains a character above U+00FF"))
        })
        .collect()
}
fn js_bytes(value: &[u8]) -> JsValue {
    JsString::from(
        value
            .iter()
            .map(|v| *v as u16)
            .collect::<Vec<_>>()
            .as_slice(),
    )
    .into()
}
fn uint16(value: &JsValue, c: &mut Context) -> JsResult<u16> {
    let number = value.to_number(c)?;
    if !number.is_finite() || number == 0.0 {
        Ok(0)
    } else {
        Ok(number.trunc().rem_euclid(65536.0) as u16)
    }
}
fn dictionary(value: &JsValue) -> JsResult<Option<JsObject>> {
    if value.is_null_or_undefined() {
        Ok(None)
    } else {
        value
            .as_object()
            .map(Some)
            .ok_or_else(|| typ("Fetch initializer must be an object, null or undefined"))
    }
}
fn field(object: &Option<JsObject>, key: &str, c: &mut Context) -> JsResult<JsValue> {
    object
        .as_ref()
        .map_or(Ok(JsValue::undefined()), |o| o.get(JsString::from(key), c))
}
fn enumeration(value: &JsValue, allowed: &[&str], c: &mut Context) -> JsResult<String> {
    let value = dom(value, c)?.to_std_string_escaped();
    if allowed.contains(&value.as_str()) {
        Ok(value)
    } else {
        Err(typ("Invalid Fetch enum value"))
    }
}
fn parse_url(value: &str, c: &Context) -> JsResult<Url> {
    bound(value.len())?;
    let base = registration(c)?.base.as_ref();
    Url::options()
        .base_url(base)
        .parse(value)
        .map_err(|_| typ("Invalid Fetch URL"))
}
fn token(bytes: &[u8]) -> bool {
    !bytes.is_empty()
        && bytes
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(b))
}
fn method(mut value: Vec<u8>) -> JsResult<Vec<u8>> {
    if !token(&value) {
        return Err(typ("Invalid HTTP method"));
    }
    let upper = value.to_ascii_uppercase();
    if [b"CONNECT".as_slice(), b"TRACE", b"TRACK"].contains(&upper.as_slice()) {
        return Err(typ("Forbidden HTTP method"));
    }
    if [
        b"DELETE".as_slice(),
        b"GET",
        b"HEAD",
        b"OPTIONS",
        b"POST",
        b"PUT",
    ]
    .contains(&upper.as_slice())
    {
        value = upper;
    }
    Ok(value)
}
fn is_null_body(status: u16) -> bool {
    matches!(status, 101 | 103 | 204 | 205 | 304)
}
fn is_redirect(status: u16) -> bool {
    matches!(status, 301 | 302 | 303 | 307 | 308)
}
fn reason_phrase(value: &[u8]) -> bool {
    value
        .iter()
        .all(|b| matches!(*b,b'\t'|b' '..=b'~'|0x80..=0xff))
}

#[derive(Clone, Trace, Finalize)]
struct RequestData {
    url: String,
    method: Vec<u8>,
    headers: JsObject,
    body: Option<Body>,
    signal: JsObject,
    destination: String,
    referrer: String,
    referrer_policy: String,
    mode: String,
    credentials: String,
    cache: String,
    redirect: String,
    integrity: JsString,
    keepalive: bool,
    reload: bool,
    history: bool,
    priority: String,
}
#[derive(Trace, Finalize, JsData)]
struct Request {
    data: Gc<GcRefCell<RequestData>>,
}
#[derive(Clone, Trace, Finalize)]
struct ResponseData {
    status: u16,
    status_text: Vec<u8>,
    headers: JsObject,
    body: Option<Body>,
    kind: String,
    url_list: Vec<String>,
}
#[derive(Trace, Finalize, JsData)]
struct Response {
    data: Gc<GcRefCell<ResponseData>>,
}
fn request(value: &JsValue) -> JsResult<Gc<GcRefCell<RequestData>>> {
    value
        .as_object()
        .and_then(|v| v.downcast_ref::<Request>().map(|v| v.data.clone()))
        .ok_or_else(|| typ("Request receiver required"))
}
fn response(value: &JsValue) -> JsResult<Gc<GcRefCell<ResponseData>>> {
    value
        .as_object()
        .and_then(|v| v.downcast_ref::<Response>().map(|v| v.data.clone()))
        .ok_or_else(|| typ("Response receiver required"))
}
/// Native brand checks for a future transport, without instanceof or JS getters.
pub fn is_request(value: &JsValue) -> bool {
    request(value).is_ok()
}
pub fn is_response(value: &JsValue) -> bool {
    response(value).is_ok()
}
/// Read the actual body handle. Cloning this handle does not clone/consume bytes.
pub fn request_body(value: &JsValue) -> JsResult<Option<Body>> {
    Ok(request(value)?.borrow().body.clone())
}
pub fn response_body(value: &JsValue) -> JsResult<Option<Body>> {
    Ok(response(value)?.borrow().body.clone())
}

/// Native Response metadata without JavaScript property access or coercion.
/// ByteStrings and the raw ordered header list preserve non-ASCII bytes and
/// duplicate fields. Header bytes are copied; the body is a shared identity
/// handle, not a clone or a consumed body.
#[derive(Clone)]
pub struct ResponseSnapshot {
    pub status: u16,
    pub status_text: Vec<u8>,
    pub headers: web_headers::HeaderList,
    pub body: Option<Body>,
}

/// Read branded internal state even when application code overrides response
/// accessors, methods, prototypes, or global constructor bindings.
pub fn response_snapshot(value: &JsValue) -> JsResult<ResponseSnapshot> {
    let data = response(value)?;
    let (status, status_text, headers, body) = {
        let data = data.borrow();
        (
            data.status,
            data.status_text.clone(),
            data.headers.clone(),
            data.body.clone(),
        )
    };
    Ok(ResponseSnapshot {
        status,
        status_text,
        headers: web_headers::read_header_list(&headers.into())?,
        body,
    })
}

#[derive(Default)]
struct RequestInit {
    nonempty: bool,
    body: Option<BodyInit>,
    cache: Option<String>,
    credentials: Option<String>,
    duplex: Option<String>,
    headers: Option<ConvertedHeadersInit>,
    integrity: Option<JsString>,
    keepalive: Option<bool>,
    method: Option<Vec<u8>>,
    mode: Option<String>,
    priority: Option<String>,
    redirect: Option<String>,
    referrer: Option<String>,
    referrer_policy: Option<String>,
    signal: Option<Option<JsObject>>,
    window: Option<JsValue>,
}
fn request_init(
    value: &JsValue,
    intrinsics: &BodyIntrinsics,
    c: &mut Context,
) -> JsResult<RequestInit> {
    let object = dictionary(value)?;
    let mut init = RequestInit::default();
    // Web IDL converts each property before getting the next, including inherited
    // getters. Unknown properties do not make the resulting dictionary nonempty.
    for key in [
        "body",
        "cache",
        "credentials",
        "duplex",
        "headers",
        "integrity",
        "keepalive",
        "method",
        "mode",
        "priority",
        "redirect",
        "referrer",
        "referrerPolicy",
        "signal",
        "window",
    ] {
        let value = field(&object, key, c)?;
        if value.is_undefined() {
            continue;
        }
        init.nonempty = true;
        match key {
            "body" => init.body = intrinsics.convert_nullable(&value, c)?,
            "cache" => {
                init.cache = Some(enumeration(
                    &value,
                    &[
                        "default",
                        "no-store",
                        "reload",
                        "no-cache",
                        "force-cache",
                        "only-if-cached",
                    ],
                    c,
                )?)
            }
            "credentials" => {
                init.credentials =
                    Some(enumeration(&value, &["omit", "same-origin", "include"], c)?)
            }
            "duplex" => init.duplex = Some(enumeration(&value, &["half"], c)?),
            "headers" => init.headers = Some(web_headers::convert_headers_init(&value, c)?),
            "integrity" => init.integrity = Some(dom(&value, c)?),
            "keepalive" => init.keepalive = Some(value.to_boolean()),
            "method" => init.method = Some(bytes(&value, c)?),
            "mode" => {
                init.mode = Some(enumeration(
                    &value,
                    &["navigate", "same-origin", "no-cors", "cors"],
                    c,
                )?)
            }
            "priority" => init.priority = Some(enumeration(&value, &["high", "low", "auto"], c)?),
            "redirect" => {
                init.redirect = Some(enumeration(&value, &["follow", "error", "manual"], c)?)
            }
            "referrer" => init.referrer = Some(usv(&value, c)?),
            "referrerPolicy" => {
                init.referrer_policy = Some(enumeration(
                    &value,
                    &[
                        "",
                        "no-referrer",
                        "no-referrer-when-downgrade",
                        "same-origin",
                        "origin",
                        "strict-origin",
                        "origin-when-cross-origin",
                        "strict-origin-when-cross-origin",
                        "unsafe-url",
                    ],
                    c,
                )?)
            }
            "signal" => {
                init.signal = Some(if value.is_null() {
                    None
                } else {
                    Some(web_abort::validate_signal(&value)?)
                })
            }
            "window" => init.window = Some(value),
            _ => unreachable!(),
        }
    }
    Ok(init)
}
struct ResponseInit {
    headers: Option<ConvertedHeadersInit>,
    status: u16,
    status_text: Vec<u8>,
}
fn response_init(value: &JsValue, c: &mut Context) -> JsResult<ResponseInit> {
    let object = dictionary(value)?;
    let headers = field(&object, "headers", c)?;
    let headers = if headers.is_undefined() {
        None
    } else {
        Some(web_headers::convert_headers_init(&headers, c)?)
    };
    let status = field(&object, "status", c)?;
    let status = if status.is_undefined() {
        200
    } else {
        uint16(&status, c)?
    };
    let status_text = field(&object, "statusText", c)?;
    let status_text = if status_text.is_undefined() {
        vec![]
    } else {
        bytes(&status_text, c)?
    };
    Ok(ResponseInit {
        headers,
        status,
        status_text,
    })
}
fn boundary(c: &Context) -> JsResult<String> {
    let registration = registration(c)?;
    let entropy = registration
        .entropy
        .clone()
        .ok_or_else(|| typ("Multipart FormData requires explicit host entropy"))?;
    if registration.boundaries.borrow().len() >= MAX_BOUNDARIES {
        return Err(limit("multipart boundary count"));
    }
    for _ in 0..8 {
        let mut bytes = [0u8; 16];
        entropy(&mut bytes)
            .map_err(|error| typ(&format!("Multipart entropy source failed: {error}")))?;
        let mut boundary = String::from("----ts-rust-");
        const HEX: &[u8; 16] = b"0123456789abcdef";
        for byte in bytes {
            boundary.push(HEX[(byte >> 4) as usize] as char);
            boundary.push(HEX[(byte & 15) as usize] as char);
        }
        if registration
            .boundaries
            .borrow_mut()
            .insert(boundary.clone())
        {
            return Ok(boundary);
        }
    }
    Err(typ("Multipart entropy source repeatedly reused a boundary"))
}
fn extract(
    intrinsics: &BodyIntrinsics,
    init: BodyInit,
    keepalive: bool,
    c: &mut Context,
) -> JsResult<ExtractedBody> {
    if !init.is_form_data() {
        return intrinsics.extract(
            init,
            ExtractionOptions {
                keepalive,
                multipart_boundary: None,
            },
            c,
        );
    }
    for _ in 0..8 {
        let boundary = boundary(c)?;
        match intrinsics.extract(
            init.clone(),
            ExtractionOptions {
                keepalive,
                multipart_boundary: Some(&boundary),
            },
            c,
        ) {
            Ok(body) => return Ok(body),
            Err(error) => {
                // This error comes exclusively from the native encoder; no JS
                // coercion or arbitrary user exception is inspected/reclassified.
                let collision = error.as_native().is_some_and(|e| {
                    e.message()
                        == "Multipart boundary collides with field data; choose another boundary"
                });
                if !collision {
                    return Err(error);
                }
            }
        }
    }
    Err(typ("Multipart boundary collisions exceeded retry limit"))
}
fn insert_type(headers: &JsObject, value: Option<&str>) -> JsResult<()> {
    if let Some(value) = value
        && !web_headers::read_header_list(&headers.clone().into())?
            .iter()
            .any(|(n, _)| n == b"content-type")
    {
        web_headers::append_header_bytes(
            &headers.clone().into(),
            b"content-type",
            value.as_bytes(),
        )?;
    }
    Ok(())
}
fn initialize_response(
    init: ResponseInit,
    body: Option<ExtractedBody>,
    c: &mut Context,
) -> JsResult<ResponseData> {
    if !(200..=599).contains(&init.status) {
        return Err(range("Response status must be between 200 and 599"));
    }
    if !reason_phrase(&init.status_text) {
        return Err(typ("Invalid Response statusText"));
    }
    let headers = web_headers::from_header_list_with_guard(vec![], HeadersGuard::Response, c)?;
    if let Some(init) = &init.headers {
        web_headers::fill_headers(&headers.clone().into(), init)?;
    }
    if body.is_some() && is_null_body(init.status) {
        return Err(typ("Response status cannot have a body"));
    }
    if let Some(body) = &body {
        insert_type(&headers, body.content_type.as_deref())?;
    }
    Ok(ResponseData {
        status: init.status,
        status_text: init.status_text,
        headers,
        body: body.map(|b| b.body),
        kind: "default".into(),
        url_list: vec![],
    })
}
fn construct_request(args: &[JsValue], c: &mut Context) -> JsResult<RequestData> {
    let _depth = enter(c)?;
    if args.is_empty() {
        return Err(typ("Request requires an input"));
    }
    let intrinsics = registration(c)?.intrinsics.clone();
    let input = args.get_or_undefined(0);
    let existing = request(input).ok();
    let input_url = if existing.is_none() {
        Some(usv(input, c)?)
    } else {
        None
    };
    let init = request_init(args.get_or_undefined(1), &intrinsics, c)?;
    let (mut data, mut source_signal) = if let Some(existing) = existing {
        let data = existing.borrow().clone();
        let signal = Some(data.signal.clone());
        (data, signal)
    } else {
        let url = parse_url(input_url.as_ref().unwrap(), c)?;
        if !url.username().is_empty() || url.password().is_some() {
            return Err(typ("Request URL cannot contain credentials"));
        }
        // The initial placeholders are native objects, never public constructors.
        let headers = web_headers::from_header_list(vec![], c)?;
        let signal = web_abort::new_signal(c)?;
        (
            RequestData {
                url: url.to_string(),
                method: b"GET".to_vec(),
                headers,
                body: None,
                signal,
                destination: String::new(),
                referrer: "about:client".into(),
                referrer_policy: String::new(),
                mode: "cors".into(),
                credentials: "same-origin".into(),
                cache: "default".into(),
                redirect: "follow".into(),
                integrity: JsString::default(),
                keepalive: false,
                reload: false,
                history: false,
                priority: "auto".into(),
            },
            None,
        )
    };
    if init.window.as_ref().is_some_and(|v| !v.is_null()) {
        return Err(typ("RequestInit window must be null"));
    }
    if init.nonempty {
        if data.mode == "navigate" {
            data.mode = "same-origin".into();
        }
        data.reload = false;
        data.history = false;
        data.referrer = "about:client".into();
        data.referrer_policy.clear();
    }
    if let Some(value) = init.referrer {
        data.referrer = if value.is_empty() {
            String::new()
        } else {
            let parsed = parse_url(&value, c)?;
            let origin = parsed.origin().ascii_serialization();
            if (parsed.scheme() == "about" && parsed.path() == "client")
                || origin == "null"
                || registration(c)?.origin.as_deref() != Some(origin.as_str())
            {
                "about:client".into()
            } else {
                parsed.to_string()
            }
        };
    }
    if let Some(value) = init.referrer_policy {
        data.referrer_policy = value;
    }
    if let Some(value) = init.mode {
        if value == "navigate" {
            return Err(typ("Request mode cannot be navigate"));
        }
        data.mode = value;
    }
    if let Some(value) = init.credentials {
        data.credentials = value;
    }
    if let Some(value) = init.cache {
        data.cache = value;
    }
    if data.cache == "only-if-cached" && data.mode != "same-origin" {
        return Err(typ("only-if-cached requires same-origin mode"));
    }
    if let Some(value) = init.redirect {
        data.redirect = value;
    }
    if let Some(value) = init.integrity {
        data.integrity = value;
    }
    if let Some(value) = init.keepalive {
        data.keepalive = value;
    }
    if let Some(value) = init.method {
        data.method = method(value)?;
    }
    if let Some(value) = init.signal {
        source_signal = value;
    }
    if let Some(value) = init.priority {
        data.priority = value;
    }
    data.signal = web_abort::dependent_signal(&source_signal.into_iter().collect::<Vec<_>>(), c)?;
    let guard = if data.mode == "no-cors" {
        if ![b"GET".as_slice(), b"HEAD", b"POST"].contains(&data.method.as_slice()) {
            return Err(typ("Method is not CORS-safelisted"));
        }
        HeadersGuard::RequestNoCors
    } else {
        HeadersGuard::Request
    };
    data.headers = if let Some(headers) = init.headers {
        web_headers::from_converted_headers_init(&headers, guard, c)?
    } else {
        web_headers::copy_headers(&data.headers.clone().into(), Some(guard), c)?
    };
    let input_body = data.body.clone();
    if (init.body.is_some() || input_body.is_some())
        && (data.method == b"GET" || data.method == b"HEAD")
    {
        return Err(typ("GET and HEAD requests cannot have a body"));
    }
    let init_body = if let Some(init) = init.body {
        let extracted = extract(&intrinsics, init, data.keepalive, c)?;
        insert_type(&data.headers, extracted.content_type.as_deref())?;
        Some(extracted.body)
    } else {
        None
    };
    let body = init_body.as_ref().or(input_body.as_ref());
    if body.is_some_and(|b| matches!(b.source(), BodySource::Null)) {
        if init_body.is_some() && init.duplex.is_none() {
            return Err(typ("Streaming Request body requires duplex: half"));
        }
        if data.mode != "same-origin" && data.mode != "cors" {
            return Err(typ(
                "Streaming Request body requires same-origin or cors mode",
            ));
        }
    }
    data.body = if let Some(body) = init_body {
        Some(body)
    } else {
        input_body.map(|body| body.proxy(c)).transpose()?
    };
    Ok(data)
}
fn clone_request(value: &JsValue, c: &mut Context) -> JsResult<JsValue> {
    let _depth = enter(c)?;
    let mut data = request(value)?.borrow().clone();
    data.body = data
        .body
        .as_ref()
        .map(|body| body.clone_body(c))
        .transpose()?;
    data.signal = web_abort::dependent_signal(&[data.signal.clone()], c)?;
    data.headers = web_headers::copy_headers(&data.headers.clone().into(), None, c)?;
    Ok(Request::from_data(
        Request {
            data: Gc::new(GcRefCell::new(data)),
        },
        c,
    )?
    .into())
}
fn clone_response(value: &JsValue, c: &mut Context) -> JsResult<JsValue> {
    let _depth = enter(c)?;
    let mut data = response(value)?.borrow().clone();
    data.body = data
        .body
        .as_ref()
        .map(|body| body.clone_body(c))
        .transpose()?;
    data.headers = web_headers::copy_headers(&data.headers.clone().into(), None, c)?;
    Ok(Response::from_data(
        Response {
            data: Gc::new(GcRefCell::new(data)),
        },
        c,
    )?
    .into())
}
fn body_parts(value: &JsValue, is_request: bool) -> JsResult<(Option<Body>, JsObject)> {
    if is_request {
        let data = request(value)?;
        let data = data.borrow();
        Ok((data.body.clone(), data.headers.clone()))
    } else {
        let data = response(value)?;
        let data = data.borrow();
        Ok((data.body.clone(), data.headers.clone()))
    }
}
fn body_get(value: &JsValue, is_request: bool, used: bool) -> JsResult<JsValue> {
    let (body, _) = body_parts(value, is_request)?;
    if used {
        Ok(body
            .as_ref()
            .map(Body::body_used)
            .transpose()?
            .unwrap_or(false)
            .into())
    } else {
        Ok(body
            .map(|body| body.stream().into())
            .unwrap_or_else(JsValue::null))
    }
}
fn consume(
    value: &JsValue,
    is_request: bool,
    kind: ConsumptionKind,
    c: &mut Context,
) -> JsResult<JsValue> {
    let result = (|| {
        let (body, headers) = body_parts(value, is_request)?;
        let intrinsics = registration(c)?.intrinsics.clone();
        intrinsics.consume_with_clock(body.as_ref(), kind, &headers, c)
    })();
    match result {
        Ok(p) => Ok(p.into()),
        Err(error) => Ok(JsPromise::reject(error, c)?.into()),
    }
}
fn body_methods(class: &mut ClassBuilder, is_request: bool) -> JsResult<()> {
    for (name, kind) in [
        ("arrayBuffer", ConsumptionKind::ArrayBuffer),
        ("blob", ConsumptionKind::Blob),
        ("bytes", ConsumptionKind::Bytes),
        ("formData", ConsumptionKind::FormData),
        ("json", ConsumptionKind::Json),
        ("text", ConsumptionKind::Text),
    ] {
        class.method(
            JsString::from(name),
            0,
            NativeFunction::from_copy_closure(move |this, _, c| consume(this, is_request, kind, c)),
        );
    }
    for (name, used) in [("body", false), ("bodyUsed", true)] {
        let get = FunctionObjectBuilder::new(
            class.context().realm(),
            NativeFunction::from_copy_closure(move |this, _, _| body_get(this, is_request, used)),
        )
        .build();
        class.accessor(
            JsString::from(name),
            Some(get),
            None,
            Attribute::ENUMERABLE.union(Attribute::CONFIGURABLE),
        );
    }
    Ok(())
}
impl Class for Request {
    const NAME: &'static str = "Request";
    const LENGTH: usize = 1;
    const ATTRIBUTES: Attribute = Attribute::WRITABLE.union(Attribute::CONFIGURABLE);
    fn data_constructor(_: &JsValue, args: &[JsValue], c: &mut Context) -> JsResult<Self> {
        Ok(Self {
            data: Gc::new(GcRefCell::new(construct_request(args, c)?)),
        })
    }
    fn init(class: &mut ClassBuilder) -> JsResult<()> {
        for key in [
            "method",
            "url",
            "headers",
            "destination",
            "referrer",
            "referrerPolicy",
            "mode",
            "credentials",
            "cache",
            "redirect",
            "integrity",
            "keepalive",
            "isReloadNavigation",
            "isHistoryNavigation",
            "signal",
            "duplex",
        ] {
            let getter = FunctionObjectBuilder::new(
                class.context().realm(),
                NativeFunction::from_copy_closure(move |this, _, _| {
                    let data = request(this)?;
                    let d = data.borrow();
                    Ok(match key {
                        "method" => js_bytes(&d.method),
                        "url" => JsString::from(d.url.as_str()).into(),
                        "headers" => d.headers.clone().into(),
                        "destination" => JsString::from(d.destination.as_str()).into(),
                        "referrer" => JsString::from(d.referrer.as_str()).into(),
                        "referrerPolicy" => JsString::from(d.referrer_policy.as_str()).into(),
                        "mode" => JsString::from(d.mode.as_str()).into(),
                        "credentials" => JsString::from(d.credentials.as_str()).into(),
                        "cache" => JsString::from(d.cache.as_str()).into(),
                        "redirect" => JsString::from(d.redirect.as_str()).into(),
                        "integrity" => d.integrity.clone().into(),
                        "keepalive" => d.keepalive.into(),
                        "isReloadNavigation" => d.reload.into(),
                        "isHistoryNavigation" => d.history.into(),
                        "signal" => d.signal.clone().into(),
                        "duplex" => JsString::from("half").into(),
                        _ => unreachable!(),
                    })
                }),
            )
            .build();
            class.accessor(
                JsString::from(key),
                Some(getter),
                None,
                Attribute::ENUMERABLE.union(Attribute::CONFIGURABLE),
            );
        }
        body_methods(class, true)?;
        class.method(
            JsString::from("clone"),
            0,
            NativeFunction::from_fn_ptr(|this, _, c| clone_request(this, c)),
        );
        class.property(
            JsSymbol::to_string_tag(),
            JsString::from(Self::NAME),
            Attribute::CONFIGURABLE,
        );
        Ok(())
    }
}
impl Class for Response {
    const NAME: &'static str = "Response";
    const ATTRIBUTES: Attribute = Attribute::WRITABLE.union(Attribute::CONFIGURABLE);
    fn data_constructor(_: &JsValue, args: &[JsValue], c: &mut Context) -> JsResult<Self> {
        let _depth = enter(c)?;
        let intrinsics = registration(c)?.intrinsics.clone();
        let body = intrinsics.convert_nullable(args.get_or_undefined(0), c)?;
        let init = response_init(args.get_or_undefined(1), c)?;
        let body = body
            .map(|body| extract(&intrinsics, body, false, c))
            .transpose()?;
        Ok(Self {
            data: Gc::new(GcRefCell::new(initialize_response(init, body, c)?)),
        })
    }
    fn init(class: &mut ClassBuilder) -> JsResult<()> {
        for key in [
            "type",
            "url",
            "redirected",
            "status",
            "ok",
            "statusText",
            "headers",
        ] {
            let getter = FunctionObjectBuilder::new(
                class.context().realm(),
                NativeFunction::from_copy_closure(move |this, _, _| {
                    let data = response(this)?;
                    let d = data.borrow();
                    Ok(match key {
                        "type" => JsString::from(d.kind.as_str()).into(),
                        "url" => JsString::from(
                            d.url_list
                                .last()
                                .map(|u| u.split('#').next().unwrap_or(""))
                                .unwrap_or(""),
                        )
                        .into(),
                        "redirected" => (d.url_list.len() > 1).into(),
                        "status" => d.status.into(),
                        "ok" => (200..=299).contains(&d.status).into(),
                        "statusText" => js_bytes(&d.status_text),
                        "headers" => d.headers.clone().into(),
                        _ => unreachable!(),
                    })
                }),
            )
            .build();
            class.accessor(
                JsString::from(key),
                Some(getter),
                None,
                Attribute::ENUMERABLE.union(Attribute::CONFIGURABLE),
            );
        }
        body_methods(class, false)?;
        class.method(
            JsString::from("clone"),
            0,
            NativeFunction::from_fn_ptr(|this, _, c| clone_response(this, c)),
        );
        class.static_method(
            JsString::from("error"),
            0,
            NativeFunction::from_fn_ptr(|_, _, c| {
                let _depth = enter(c)?;
                let headers =
                    web_headers::from_header_list_with_guard(vec![], HeadersGuard::Immutable, c)?;
                Ok(Response::from_data(
                    Response {
                        data: Gc::new(GcRefCell::new(ResponseData {
                            status: 0,
                            status_text: vec![],
                            headers,
                            body: None,
                            kind: "error".into(),
                            url_list: vec![],
                        })),
                    },
                    c,
                )?
                .into())
            }),
        );
        class.static_method(
            JsString::from("redirect"),
            1,
            NativeFunction::from_fn_ptr(|_, args, c| {
                let _depth = enter(c)?;
                if args.is_empty() {
                    return Err(typ("Response.redirect requires a URL"));
                }
                let url = usv(&args[0], c)?;
                let status = if args.get_or_undefined(1).is_undefined() {
                    302
                } else {
                    uint16(args.get_or_undefined(1), c)?
                };
                let url = parse_url(&url, c)?;
                if !is_redirect(status) {
                    return Err(range("Invalid redirect status"));
                }
                let headers = web_headers::from_header_list_with_guard(
                    vec![(b"location".to_vec(), url.as_str().as_bytes().to_vec())],
                    HeadersGuard::Immutable,
                    c,
                )?;
                Ok(Response::from_data(
                    Response {
                        data: Gc::new(GcRefCell::new(ResponseData {
                            status,
                            status_text: vec![],
                            headers,
                            body: None,
                            kind: "default".into(),
                            url_list: vec![],
                        })),
                    },
                    c,
                )?
                .into())
            }),
        );
        class.static_method(
            JsString::from("json"),
            1,
            NativeFunction::from_fn_ptr(|_, args, c| {
                let _depth = enter(c)?;
                if args.is_empty() {
                    return Err(typ("Response.json requires data"));
                }
                let init = response_init(args.get_or_undefined(1), c)?;
                let intrinsics = registration(c)?.intrinsics.clone();
                let bytes = intrinsics.json_bytes(&args[0], c)?;
                let value = JsUint8Array::from_iter(bytes, c)?.into();
                let body = intrinsics.convert(&value, c)?;
                let mut body = extract(&intrinsics, body, false, c)?;
                body.content_type = Some("application/json".into());
                let data = initialize_response(init, Some(body), c)?;
                Ok(Response::from_data(
                    Response {
                        data: Gc::new(GcRefCell::new(data)),
                    },
                    c,
                )?
                .into())
            }),
        );
        class.property(
            JsSymbol::to_string_tag(),
            JsString::from(Self::NAME),
            Attribute::CONFIGURABLE,
        );
        Ok(())
    }
}
/// Register the server-profile Request/Response capability once. Options are
/// explicit; missing multipart entropy affects only FormData construction.
/// No `fetch` global or network authority is installed.
pub fn register(context: &mut Context, options: FetchOptions) -> JsResult<()> {
    if context.has_data::<Registration>() {
        return Err(typ("Fetch classes are already registered"));
    }
    for name in ["Request", "Response"] {
        if context
            .global_object()
            .has_property(JsString::from(name), context)?
        {
            return Err(typ("Fetch registration would replace an existing global"));
        }
    }
    let base = options
        .base_url
        .as_deref()
        .map(|value| {
            bound(value.len())?;
            Url::parse(value).map_err(|_| typ("Fetch base_url must be absolute"))
        })
        .transpose()?;
    let origin = if let Some(value) = options.origin {
        bound(value.len())?;
        if value == "null" {
            None
        } else {
            let parsed = Url::parse(&value)
                .map_err(|_| typ("Fetch origin must be an absolute origin or null"))?;
            let origin = parsed.origin().ascii_serialization();
            if origin == "null" { None } else { Some(origin) }
        }
    } else {
        base.as_ref()
            .map(|url| url.origin().ascii_serialization())
            .filter(|v| v != "null")
    };
    let intrinsics = BodyIntrinsics::capture(context)?;
    context.insert_data(Registration {
        intrinsics,
        base,
        origin,
        entropy: options.multipart_entropy,
        boundaries: RefCell::new(BTreeSet::new()),
        depth: Rc::new(Cell::new(0)),
    });
    context.register_global_class::<Request>()?;
    context.register_global_class::<Response>()?;
    for prototype in [
        context.get_global_class::<Request>().unwrap().prototype(),
        context.get_global_class::<Response>().unwrap().prototype(),
    ] {
        for name in [
            "arrayBuffer",
            "blob",
            "bytes",
            "formData",
            "json",
            "text",
            "clone",
        ] {
            prototype.define_property_or_throw(
                JsString::from(name),
                PropertyDescriptor::builder().enumerable(true),
                context,
            )?;
        }
    }
    let ctor = context
        .get_global_class::<Response>()
        .unwrap()
        .constructor();
    for name in ["error", "redirect", "json"] {
        ctor.define_property_or_throw(
            JsString::from(name),
            PropertyDescriptor::builder().enumerable(true),
            context,
        )?;
    }
    Ok(())
}
#[cfg(test)]
#[path = "web_fetch_tests.rs"]
mod tests;
