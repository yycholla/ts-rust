//! Native Fetch Body algorithms, without transport, globals, or implicit I/O.
//!
//! References: <https://fetch.spec.whatwg.org/#bodyinit-unions>,
//! <https://fetch.spec.whatwg.org/#body-mixin>, and
//! <https://mimesniff.spec.whatwg.org/#parsing-a-mime-type>.
//! The embedding captures intrinsics before application code, converts BodyInit
//! before init-dictionary getters, then extracts afterwards. Multipart boundaries
//! and newly parsed File timestamps are explicit host inputs. Bodies are bounded
//! to 64 MiB; Blob/FormData retain their own smaller documented capability limits.
//! No Request/Response/fetch globals are installed by this module.
use boa_engine::{
    Context, JsArgs, JsError, JsNativeError, JsObject, JsResult, JsString, JsValue, NativeFunction,
    builtins::{object::OrdinaryObject, promise::ResolvingFunctions},
    object::builtins::{JsArrayBuffer, JsDataView, JsPromise, JsTypedArray, JsUint8Array},
};
use boa_gc::{Finalize, Gc, GcRefCell, Trace};

use crate::{web_blob, web_form_data, web_headers, web_streams, web_url};

const MAX_BYTES: usize = 64 * 1024 * 1024;
const MAX_MIME: usize = 4 * 1024 * 1024;
fn typ(message: &str) -> JsError {
    JsNativeError::typ().with_message(message.to_owned()).into()
}
fn bound(size: usize) -> JsResult<()> {
    if size > MAX_BYTES {
        Err(JsNativeError::range()
            .with_message("Fetch body exceeds 64 MiB")
            .into())
    } else {
        Ok(())
    }
}

/// Traced native intrinsics. Capture once, before running application JavaScript.
#[derive(Clone, Trace, Finalize)]
pub struct BodyIntrinsics {
    resizable: JsObject,
    json_parse: JsObject,
    json_stringify: JsObject,
}
impl BodyIntrinsics {
    pub fn capture(context: &mut Context) -> JsResult<Self> {
        let prototype = context
            .intrinsics()
            .constructors()
            .array_buffer()
            .prototype();
        let descriptor = OrdinaryObject::get_own_property_descriptor(
            &JsValue::undefined(),
            &[prototype.into(), JsString::from("resizable").into()],
            context,
        )?;
        let resizable = descriptor
            .as_object()
            .ok_or_else(|| typ("Missing ArrayBuffer descriptor"))?
            .get(JsString::from("get"), context)?
            .as_callable()
            .ok_or_else(|| typ("Missing ArrayBuffer resizable getter"))?;
        let json = context.intrinsics().objects().json();
        let json_parse = json
            .get(JsString::from("parse"), context)?
            .as_callable()
            .ok_or_else(|| typ("Missing JSON.parse intrinsic"))?;
        let json_stringify = json
            .get(JsString::from("stringify"), context)?
            .as_callable()
            .ok_or_else(|| typ("Missing JSON.stringify intrinsic"))?;
        Ok(Self {
            resizable,
            json_parse,
            json_stringify,
        })
    }

    fn fixed(&self, object: JsObject, context: &mut Context) -> JsResult<JsArrayBuffer> {
        let buffer = JsArrayBuffer::from_object(object.clone())
            .map_err(|_| typ("BodyInit BufferSource cannot use shared memory"))?;
        if self
            .resizable
            .call(&object.into(), &[], context)?
            .to_boolean()
        {
            return Err(typ("BodyInit BufferSource cannot be resizable"));
        }
        Ok(buffer)
    }

    /// Web IDL union conversion. Null/undefined become strings here; nullable
    /// Request/Response constructor arguments should use `convert_nullable`.
    pub fn convert(&self, value: &JsValue, context: &mut Context) -> JsResult<BodyInit> {
        if let Some(object) = value.as_object() {
            if web_blob::snapshot(value).is_some() {
                return Ok(BodyInit(Init::Blob(object)));
            }
            if let Some(buffer) = buffer_of(&object, context)? {
                self.fixed(buffer, context)?;
                return Ok(BodyInit(Init::Buffer(object)));
            }
            if web_form_data::snapshot(value).is_ok() {
                return Ok(BodyInit(Init::Form(object)));
            }
            if web_url::search_params_snapshot(value).is_some() {
                return Ok(BodyInit(Init::Params(object)));
            }
            if web_streams::is_locked(value).is_ok() {
                return Ok(BodyInit(Init::Stream(object)));
            }
        }
        let text = value.to_string(context)?;
        bound(text.len())?;
        let text = text.to_std_string_lossy();
        bound(text.len())?;
        Ok(BodyInit(Init::String(text)))
    }

    pub fn convert_nullable(
        &self,
        value: &JsValue,
        context: &mut Context,
    ) -> JsResult<Option<BodyInit>> {
        if value.is_null_or_undefined() {
            Ok(None)
        } else {
            self.convert(value, context).map(Some)
        }
    }

    pub fn extract(
        &self,
        init: BodyInit,
        options: ExtractionOptions<'_>,
        context: &mut Context,
    ) -> JsResult<ExtractedBody> {
        let (bytes, source, content_type) = match &init.0 {
            Init::Stream(stream) => {
                if options.keepalive {
                    return Err(typ("A streaming body cannot use keepalive"));
                }
                let stream_value = stream.clone().into();
                if web_streams::is_locked(&stream_value)?
                    || web_streams::is_disturbed(&stream_value)?
                {
                    return Err(typ("Body stream is locked or disturbed"));
                }
                return Ok(ExtractedBody {
                    body: Body::new(stream.clone(), BodySource::Null, None),
                    content_type: None,
                });
            }
            Init::Blob(object) => {
                let blob = web_blob::snapshot(&object.clone().into())
                    .ok_or_else(|| typ("Invalid Blob body"))?;
                let content_type = (!blob.media_type.is_empty()).then_some(blob.media_type);
                (blob.bytes, BodySource::Blob(object.clone()), content_type)
            }
            Init::Buffer(object) => (self.copy_buffer(object, context)?, BodySource::Null, None),
            Init::Form(object) => {
                let boundary = options.multipart_boundary.ok_or_else(|| {
                    typ("Multipart extraction requires a host-generated boundary")
                })?;
                let entries = web_form_data::snapshot(&object.clone().into())?;
                let encoded = web_form_data::encode_multipart(&entries, boundary)
                    .map_err(|error| typ(&error))?;
                (
                    encoded.bytes,
                    BodySource::FormData(object.clone()),
                    Some(encoded.content_type),
                )
            }
            Init::Params(object) => {
                let entries = web_url::search_params_snapshot(&object.clone().into())
                    .ok_or_else(|| typ("Invalid URLSearchParams body"))?;
                let bytes = url::form_urlencoded::Serializer::new(String::new())
                    .extend_pairs(entries)
                    .finish()
                    .into_bytes();
                (
                    bytes,
                    BodySource::Null,
                    Some("application/x-www-form-urlencoded;charset=UTF-8".into()),
                )
            }
            Init::String(text) => (
                text.as_bytes().to_vec(),
                BodySource::Null,
                Some("text/plain;charset=UTF-8".into()),
            ),
        };
        bound(bytes.len())?;
        let length = Some(bytes.len() as u64);
        let source = if matches!(source, BodySource::Null) {
            BodySource::Bytes(bytes.clone())
        } else {
            source
        };
        let stream = web_streams::from_bytes(bytes, context)?;
        Ok(ExtractedBody {
            body: Body::new(stream, source, length),
            content_type,
        })
    }

    fn copy_buffer(&self, object: &JsObject, context: &mut Context) -> JsResult<Vec<u8>> {
        let buffer = self.fixed(
            buffer_of(object, context)?.ok_or_else(|| typ("Invalid BufferSource"))?,
            context,
        )?;
        // Web IDL get-a-copy-of-the-bytes yields an empty sequence for detached input.
        if buffer.data().is_none() {
            return Ok(Vec::new());
        }
        let (offset, length) = if JsArrayBuffer::from_object(object.clone()).is_ok() {
            (0, buffer.byte_length())
        } else if let Ok(view) = JsTypedArray::from_object(object.clone()) {
            (view.byte_offset(context)?, view.byte_length(context)?)
        } else {
            let view = JsDataView::from_object(object.clone())?;
            (
                view.byte_offset(context)? as usize,
                view.byte_length(context)? as usize,
            )
        };
        bound(length)?;
        let bytes = buffer.data().ok_or_else(|| typ("Detached BufferSource"))?;
        Ok(bytes
            .get(
                offset
                    ..offset
                        .checked_add(length)
                        .ok_or_else(|| typ("Invalid BufferSource length"))?,
            )
            .ok_or_else(|| typ("Invalid BufferSource range"))?
            .to_vec())
    }

    /// Actual ECMAScript JSON serialization for Response.json and similar hosts.
    /// `undefined` results fail as required by serialize-a-JavaScript-value-to-JSON-bytes.
    pub fn json_bytes(&self, value: &JsValue, context: &mut Context) -> JsResult<Vec<u8>> {
        let result = self.json_stringify.call(
            &JsValue::undefined(),
            std::slice::from_ref(value),
            context,
        )?;
        let text = result
            .as_string()
            .ok_or_else(|| typ("Value cannot be serialized to JSON"))?;
        let bytes = text.to_std_string_lossy().into_bytes();
        bound(bytes.len())?;
        Ok(bytes)
    }

    /// Read a real body stream. The native reader remains locked even on failure;
    /// invalid chunks do not cancel the source. MIME headers are read at completion.
    pub fn consume(
        &self,
        body: Option<&Body>,
        kind: ConsumptionKind,
        headers: &JsObject,
        file_last_modified: i64,
        context: &mut Context,
    ) -> JsResult<JsPromise> {
        self.consume_at(body, kind, headers, Some(file_last_modified), context)
    }

    /// Capture the native clock when fully-read bytes are converted to Files,
    /// rather than before an asynchronous source has finished producing them.
    pub fn consume_with_clock(
        &self,
        body: Option<&Body>,
        kind: ConsumptionKind,
        headers: &JsObject,
        context: &mut Context,
    ) -> JsResult<JsPromise> {
        self.consume_at(body, kind, headers, None, context)
    }

    fn consume_at(
        &self,
        body: Option<&Body>,
        kind: ConsumptionKind,
        headers: &JsObject,
        file_last_modified: Option<i64>,
        context: &mut Context,
    ) -> JsResult<JsPromise> {
        let capture = Consumption {
            intrinsics: self.clone(),
            kind,
            headers: headers.clone(),
            file_last_modified,
        };
        let (promise, settle) = JsPromise::new_pending(context);
        let Some(body) = body else {
            let result = capture.convert(Vec::new(), context);
            finish(result, &settle, context)?;
            return Ok(promise);
        };
        if body.unusable()? {
            finish(Err(typ("Body is locked or disturbed")), &settle, context)?;
            return Ok(promise);
        }
        let read = match web_streams::fully_read_bytes(&body.stream().into(), context) {
            Ok(read) => read,
            Err(error) => {
                finish(Err(error), &settle, context)?;
                return Ok(promise);
            }
        };
        let convert = NativeFunction::from_copy_closure_with_captures(
            |_, args, capture, context| {
                let result = web_streams::copy_uint8_chunk(args.get_or_undefined(0), context)
                    .and_then(|bytes| capture.0.convert(bytes, context));
                finish(result, &capture.1, context)?;
                Ok(JsValue::undefined())
            },
            (capture, settle.clone()),
        )
        .to_js_function(context.realm());
        let failure = NativeFunction::from_copy_closure_with_captures(
            |_, args, settle, context| {
                settle.reject.call(
                    &JsValue::undefined(),
                    &[args.get_or_undefined(0).clone()],
                    context,
                )?;
                Ok(JsValue::undefined())
            },
            settle,
        )
        .to_js_function(context.realm());
        drop(read.then_intrinsic(Some(convert), Some(failure), context)?);
        Ok(promise)
    }
}

fn buffer_of(object: &JsObject, context: &mut Context) -> JsResult<Option<JsObject>> {
    if JsArrayBuffer::from_object(object.clone()).is_ok() {
        return Ok(Some(object.clone()));
    }
    if let Ok(view) = JsTypedArray::from_object(object.clone()) {
        return Ok(view.buffer(context)?.as_object());
    }
    if let Ok(view) = JsDataView::from_object(object.clone()) {
        return Ok(view.buffer(context)?.as_object());
    }
    Ok(None)
}

#[derive(Clone, Trace, Finalize)]
enum Init {
    Blob(JsObject),
    Buffer(JsObject),
    Form(JsObject),
    Params(JsObject),
    Stream(JsObject),
    String(String),
}
/// A converted union retaining references until the specification extracts bytes.
#[derive(Clone, Trace, Finalize)]
pub struct BodyInit(Init);
impl BodyInit {
    pub fn is_form_data(&self) -> bool {
        matches!(&self.0, Init::Form(_))
    }
}
impl BodyInit {
    pub fn is_stream(&self) -> bool {
        matches!(self.0, Init::Stream(_))
    }
}
#[derive(Default)]
pub struct ExtractionOptions<'a> {
    pub keepalive: bool,
    /// Supply a fresh, collision-free host boundary. No entropy source is implicit.
    pub multipart_boundary: Option<&'a str>,
}
#[derive(Clone, Trace, Finalize)]
pub enum BodySource {
    Null,
    Bytes(Vec<u8>),
    Blob(JsObject),
    FormData(JsObject),
}
#[derive(Trace, Finalize)]
struct BodyState {
    stream: JsObject,
    source: BodySource,
    length: Option<u64>,
}
/// Cloning this Rust handle shares identity. Use `clone_body` for Fetch cloning.
#[derive(Clone, Trace, Finalize)]
pub struct Body(Gc<GcRefCell<BodyState>>);
impl Body {
    fn new(stream: JsObject, source: BodySource, length: Option<u64>) -> Self {
        Self(Gc::new(GcRefCell::new(BodyState {
            stream,
            source,
            length,
        })))
    }
    pub fn stream(&self) -> JsObject {
        self.0.borrow().stream.clone()
    }
    pub fn source(&self) -> BodySource {
        self.0.borrow().source.clone()
    }
    pub fn length(&self) -> Option<u64> {
        self.0.borrow().length
    }
    pub fn body_used(&self) -> JsResult<bool> {
        web_streams::is_disturbed(&self.stream().into())
    }
    pub fn unusable(&self) -> JsResult<bool> {
        let stream = self.stream().into();
        Ok(web_streams::is_locked(&stream)? || web_streams::is_disturbed(&stream)?)
    }
    pub fn clone_body(&self, context: &mut Context) -> JsResult<Self> {
        if self.unusable()? {
            return Err(typ("Cannot clone an unusable body"));
        }
        let (first, second) = web_streams::tee_for_fetch(&self.stream().into(), context)?;
        let mut data = self.0.borrow_mut();
        data.stream = first;
        Ok(Self::new(second, data.source.clone(), data.length))
    }
    /// Request transfer through the native identity transform; the old stream
    /// becomes locked. This is distinct from teeing/cloning a response body.
    pub fn proxy(&self, context: &mut Context) -> JsResult<Self> {
        if self.unusable()? {
            return Err(typ("Cannot proxy an unusable body"));
        }
        let stream = web_streams::identity_proxy(&self.stream().into(), context)?;
        Ok(Self::new(stream, self.source(), self.length()))
    }
}
pub struct ExtractedBody {
    pub body: Body,
    pub content_type: Option<String>,
}
#[derive(Clone, Copy)]
pub enum ConsumptionKind {
    Text,
    Json,
    ArrayBuffer,
    Bytes,
    Blob,
    FormData,
}
#[derive(Trace, Finalize)]
struct Consumption {
    intrinsics: BodyIntrinsics,
    #[unsafe_ignore_trace]
    kind: ConsumptionKind,
    headers: JsObject,
    file_last_modified: Option<i64>,
}
enum Converted {
    Value(JsValue),
    Async(JsPromise),
}
fn finish(
    result: JsResult<Converted>,
    settle: &ResolvingFunctions,
    context: &mut Context,
) -> JsResult<()> {
    match result {
        Ok(Converted::Value(value)) => {
            settle
                .resolve
                .call(&JsValue::undefined(), &[value], context)?;
        }
        Err(error) => {
            let value = error.into_opaque(context)?;
            settle
                .reject
                .call(&JsValue::undefined(), &[value], context)?;
        }
        Ok(Converted::Async(promise)) => {
            let success = NativeFunction::from_copy_closure_with_captures(
                |_, args, settle, context| {
                    settle.resolve.call(
                        &JsValue::undefined(),
                        &[args.get_or_undefined(0).clone()],
                        context,
                    )?;
                    Ok(JsValue::undefined())
                },
                settle.clone(),
            )
            .to_js_function(context.realm());
            let failure = NativeFunction::from_copy_closure_with_captures(
                |_, args, settle, context| {
                    settle.reject.call(
                        &JsValue::undefined(),
                        &[args.get_or_undefined(0).clone()],
                        context,
                    )?;
                    Ok(JsValue::undefined())
                },
                settle.clone(),
            )
            .to_js_function(context.realm());
            drop(promise.then_intrinsic(Some(success), Some(failure), context)?);
        }
    }
    Ok(())
}
impl Consumption {
    fn convert(&self, bytes: Vec<u8>, context: &mut Context) -> JsResult<Converted> {
        bound(bytes.len())?;
        match self.kind {
            ConsumptionKind::Bytes => Ok(JsUint8Array::from_iter(bytes, context)?.into()),
            ConsumptionKind::ArrayBuffer => {
                let buffer = JsArrayBuffer::new(bytes.len(), context)?;
                buffer
                    .data_mut()
                    .ok_or_else(|| typ("Detached new ArrayBuffer"))?
                    .copy_from_slice(&bytes);
                Ok(buffer.into())
            }
            ConsumptionKind::Text | ConsumptionKind::Json => {
                let bytes = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(&bytes);
                let text = JsString::from(String::from_utf8_lossy(bytes).as_ref());
                if matches!(self.kind, ConsumptionKind::Text) {
                    Ok(text.into())
                } else {
                    self.intrinsics
                        .json_parse
                        .call(&JsValue::undefined(), &[text.into()], context)
                }
            }
            ConsumptionKind::Blob | ConsumptionKind::FormData => {
                let headers = web_headers::read_header_list(&self.headers.clone().into())?;
                let mime = extract_mime_type(&headers)?;
                if matches!(self.kind, ConsumptionKind::Blob) {
                    return Ok(Converted::Value(
                        web_blob::from_bytes(
                            bytes,
                            &mime.map_or_else(String::new, |mime| mime.serialize()),
                            context,
                        )?
                        .into(),
                    ));
                }
                let mime = mime.ok_or_else(|| typ("Body has no form-data MIME type"))?;
                match mime.essence.as_str() {
                    "application/x-www-form-urlencoded" => {
                        let entries = web_form_data::decode_urlencoded(&bytes)
                            .map_err(|error| typ(&error))?;
                        Ok(web_form_data::from_entries(entries, context)?.into())
                    }
                    "multipart/form-data" => {
                        let content_type = mime.serialize();
                        let modified = self
                            .file_last_modified
                            .unwrap_or_else(|| context.clock().system_time_millis());
                        return Ok(Converted::Async(JsPromise::from_async_fn(
                            async move |context| {
                                let entries =
                                    web_form_data::decode_multipart(bytes, &content_type, modified)
                                        .await
                                        .map_err(|error| typ(&error))?;
                                Ok(
                                    web_form_data::from_entries(
                                        entries,
                                        &mut context.borrow_mut(),
                                    )?
                                    .into(),
                                )
                            },
                            context,
                        )));
                    }
                    _ => Err(typ("Body MIME type is not a supported form encoding")),
                }
            }
        }
        .map(Converted::Value)
    }
}

/// Ordered MIME parameters, preserving first valid duplicate and value case.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MimeType {
    pub essence: String,
    pub parameters: Vec<(String, String)>,
}
impl MimeType {
    pub fn parameter(&self, name: &str) -> Option<&str> {
        self.parameters
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }
    pub fn serialize(&self) -> String {
        let mut output = self.essence.clone();
        for (name, value) in &self.parameters {
            output.push(';');
            output.push_str(name);
            output.push('=');
            if !value.is_empty() && value.chars().all(token) {
                output.push_str(value);
            } else {
                output.push('"');
                for ch in value.chars() {
                    if matches!(ch, '"' | '\\') {
                        output.push('\\');
                    }
                    output.push(ch);
                }
                output.push('"');
            }
        }
        output
    }
}
fn whitespace(ch: char) -> bool {
    matches!(ch, '\t' | '\r' | '\n' | ' ')
}
fn token(ch: char) -> bool {
    ch.is_ascii_alphanumeric()
        || matches!(
            ch,
            '!' | '#'
                | '$'
                | '%'
                | '&'
                | '\''
                | '*'
                | '+'
                | '-'
                | '.'
                | '^'
                | '_'
                | '`'
                | '|'
                | '~'
        )
}
/// WHATWG MIME parser, rather than strict RFC validation. Invalid parameters are
/// ignored; invalid type/subtype returns None. Resource limits are explicit errors.
pub fn parse_mime_type(input: &str) -> JsResult<Option<MimeType>> {
    if input.len() > MAX_MIME {
        return Err(typ("MIME input exceeds 4 MiB"));
    }
    let input = input.trim_matches(whitespace);
    let Some((kind, rest)) = input.split_once('/') else {
        return Ok(None);
    };
    let (subtype, parameters) = rest.split_once(';').unwrap_or((rest, ""));
    let subtype = subtype.trim_end_matches(whitespace);
    if kind.is_empty()
        || subtype.is_empty()
        || !kind.chars().all(token)
        || !subtype.chars().all(token)
    {
        return Ok(None);
    }
    let mut mime = MimeType {
        essence: format!(
            "{}/{}",
            kind.to_ascii_lowercase(),
            subtype.to_ascii_lowercase()
        ),
        parameters: Vec::new(),
    };
    let mut input = parameters;
    while !input.is_empty() {
        input = input.trim_start_matches(whitespace);
        let end = input.find([';', '=']).unwrap_or(input.len());
        let name = input[..end].to_ascii_lowercase();
        input = &input[end..];
        if input.starts_with(';') {
            input = &input[1..];
            continue;
        }
        if input.is_empty() {
            break;
        }
        input = &input[1..];
        if input.is_empty() {
            break;
        }
        let value;
        if input.starts_with('"') {
            let (quoted, remaining) = quoted(&input[1..]);
            value = quoted;
            input = remaining;
            input = &input[input.find(';').unwrap_or(input.len())..];
        } else {
            let end = input.find(';').unwrap_or(input.len());
            value = input[..end].trim_end_matches(whitespace).to_owned();
            input = &input[end..];
            if value.is_empty() {
                if !input.is_empty() {
                    input = &input[1..];
                }
                continue;
            }
        }
        if !name.is_empty()
            && name.chars().all(token)
            && value.chars().all(|ch| {
                ch == '\t' || (' '..='~').contains(&ch) || ('\u{80}'..='\u{ff}').contains(&ch)
            })
            && mime.parameter(&name).is_none()
        {
            if mime.parameters.len() >= 10_000 {
                return Err(typ("MIME parameter limit exceeded"));
            }
            mime.parameters.push((name, value));
        }
        if !input.is_empty() {
            input = &input[1..];
        }
    }
    Ok(Some(mime))
}
fn quoted(mut input: &str) -> (String, &str) {
    let mut output = String::new();
    while let Some(ch) = input.chars().next() {
        input = &input[ch.len_utf8()..];
        if ch == '"' {
            break;
        }
        if ch == '\\' {
            if let Some(next) = input.chars().next() {
                output.push(next);
                input = &input[next.len_utf8()..];
            } else {
                output.push('\\');
            }
        } else {
            output.push(ch);
        }
    }
    (output, input)
}

/// Fetch extract-MIME-type, including duplicate field splitting and charset
/// inheritance only across the same essence. Input is the native header list.
pub fn extract_mime_type(headers: &web_headers::HeaderList) -> JsResult<Option<MimeType>> {
    let mut input = String::new();
    for (name, value) in headers {
        if !name.eq_ignore_ascii_case(b"content-type") {
            continue;
        }
        if input
            .len()
            .saturating_add(value.len().saturating_mul(2))
            .saturating_add(2)
            > MAX_MIME
        {
            return Err(typ("Content-Type exceeds 4 MiB"));
        }
        if !input.is_empty() {
            input.push_str(", ");
        }
        input.extend(value.iter().map(|byte| char::from(*byte)));
    }
    let mut last: Option<MimeType> = None;
    let mut charset: Option<String> = None;
    let mut start = 0;
    let mut quoted = false;
    let mut escaped = false;
    let mut values = Vec::new();
    for (index, ch) in input.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if quoted && ch == '\\' {
            escaped = true;
            continue;
        }
        if ch == '"' {
            quoted = !quoted;
        }
        if ch == ',' && !quoted {
            values.push(&input[start..index]);
            start = index + 1;
        }
    }
    values.push(&input[start..]);
    for value in values {
        let Some(mut mime) = parse_mime_type(value)? else {
            continue;
        };
        if mime.essence == "*/*" {
            continue;
        }
        if last
            .as_ref()
            .is_none_or(|last| last.essence != mime.essence)
        {
            charset = mime.parameter("charset").map(str::to_owned);
        } else if mime.parameter("charset").is_none()
            && let Some(charset) = &charset
        {
            mime.parameters.push(("charset".into(), charset.clone()));
        }
        last = Some(mime);
    }
    Ok(last)
}

/// Constructor content type insertion. The caller must subsequently apply the
/// Request/Response header guard; an existing (even empty) header is preserved.
pub fn append_content_type_if_missing(
    headers: &mut web_headers::HeaderList,
    content_type: Option<&str>,
) -> JsResult<()> {
    if headers
        .iter()
        .any(|(name, _)| name.eq_ignore_ascii_case(b"content-type"))
    {
        return Ok(());
    }
    if let Some(content_type) = content_type {
        if content_type.len() > MAX_MIME
            || !content_type.is_ascii()
            || content_type
                .bytes()
                .any(|byte| matches!(byte, 0 | b'\r' | b'\n'))
        {
            return Err(typ("Invalid extracted Content-Type"));
        }
        headers.push((b"content-type".to_vec(), content_type.as_bytes().to_vec()));
    }
    Ok(())
}

#[cfg(test)]
#[path = "web_fetch_body_tests.rs"]
mod tests;
