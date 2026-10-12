//! Native ordered FormData, with pure bounded body codecs for future Fetch.
//! References: xhr.spec.whatwg.org/#interface-formdata, HTML §4.10.22.8,
//! fetch.spec.whatwg.org/#dom-body-formdata. No DOM, I/O, or JS fallback.
//! Bounds: 10,000 entries, 16 MiB accounted entry data, 32 MiB encoded body.
//! Multipart framing uses multer 3.1.0, not a delimiter-splitting approximation.
//! Register Blob/File first. Native callers supply boundary and file timestamps;
//! codecs never obtain ambient entropy or time. DOM constructors remain unavailable.
use super::web_blob;
use boa_engine::{
    Context, JsArgs, JsData, JsError, JsNativeError, JsObject, JsResult, JsString, JsSymbol,
    JsValue, NativeFunction,
    builtins::{iterable::create_iter_result_object, object::OrdinaryObject},
    class::{Class, ClassBuilder},
    object::{FunctionObjectBuilder, builtins::JsArray},
    property::{Attribute, PropertyDescriptor},
};
use boa_gc::{Finalize, Trace};

const MAX_ENTRIES: usize = 10_000;
const MAX_DATA: usize = 16 * 1024 * 1024;
const MAX_BODY: usize = 32 * 1024 * 1024;
fn typ(message: &str) -> JsError {
    JsNativeError::typ().with_message(message.to_owned()).into()
}
fn limit() -> JsError {
    JsNativeError::range()
        .with_message("Native FormData allocation/iteration limit exceeded")
        .into()
}
fn usv(value: &JsValue, context: &mut Context) -> JsResult<JsString> {
    let string = value.to_string(context)?;
    if string.len() > MAX_DATA {
        return Err(limit());
    }
    let string = string.to_std_string_lossy();
    if string.len() > MAX_DATA {
        return Err(limit());
    }
    Ok(JsString::from(string))
}
fn required(args: &[JsValue], count: usize) -> JsResult<()> {
    if args.len() < count {
        Err(typ("Insufficient FormData arguments"))
    } else {
        Ok(())
    }
}
#[derive(Clone, Trace, Finalize)]
struct Entry {
    name: JsString,
    value: JsValue,
    size: usize,
}
#[derive(Trace, Finalize, JsData)]
struct FormData {
    entries: Vec<Entry>,
    size: usize,
}
fn receiver(this: &JsValue) -> JsResult<JsObject> {
    this.as_object()
        .filter(|object| object.is::<FormData>())
        .ok_or_else(|| typ("FormData receiver required"))
}
#[derive(Clone, Copy)]
enum Iteration {
    Keys,
    Values,
    Entries,
}
#[derive(Trace, Finalize, JsData)]
struct FormIterator {
    // Traced strong reference retains the form and every contained File.
    form: JsObject,
    #[unsafe_ignore_trace]
    kind: Iteration,
    index: usize,
    done: bool,
}
struct Registration {
    iterator_prototype: JsObject,
}

/// Pure entry snapshots retain ordering and duplicates. File bytes and metadata
/// are copied; the live JS FormData object retains original File identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EntrySnapshot {
    pub name: String,
    pub value: EntryValue,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EntryValue {
    Text(String),
    File(FileSnapshot),
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileSnapshot {
    pub name: String,
    pub bytes: Vec<u8>,
    pub media_type: String,
    pub last_modified: i64,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EncodedForm {
    pub bytes: Vec<u8>,
    pub content_type: String,
}
fn entry_size(entry: &EntrySnapshot) -> usize {
    entry.name.len().saturating_add(match &entry.value {
        EntryValue::Text(value) => value.len(),
        EntryValue::File(file) => file
            .bytes
            .len()
            .saturating_add(file.name.len())
            .saturating_add(file.media_type.len()),
    })
}
fn validate_entries(entries: &[EntrySnapshot]) -> Result<(), String> {
    if entries.len() > MAX_ENTRIES {
        return Err("FormData exceeds 10000 entries".into());
    }
    let mut size = 0usize;
    for entry in entries {
        size = size.saturating_add(entry_size(entry));
        if size > MAX_DATA {
            return Err("FormData exceeds 16 MiB entry data".into());
        }
        if let EntryValue::File(file) = &entry.value
            && !file.media_type.bytes().all(|b| (0x20..=0x7e).contains(&b))
        {
            return Err("File content type must contain printable ASCII only".into());
        }
    }
    Ok(())
}
pub fn snapshot(value: &JsValue) -> JsResult<Vec<EntrySnapshot>> {
    let object = receiver(value)?;
    let form = object
        .downcast_ref::<FormData>()
        .ok_or_else(|| typ("FormData receiver required"))?;
    form.entries
        .iter()
        .map(|entry| {
            let value = if let Some(value) = entry.value.as_string() {
                EntryValue::Text(value.to_std_string_lossy())
            } else {
                let blob = web_blob::snapshot(&entry.value)
                    .ok_or_else(|| typ("Invalid native FormData file"))?;
                let file = blob
                    .file
                    .ok_or_else(|| typ("FormData value must be File"))?;
                EntryValue::File(FileSnapshot {
                    name: file.name,
                    bytes: blob.bytes,
                    media_type: blob.media_type,
                    last_modified: file.last_modified,
                })
            };
            Ok(EntrySnapshot {
                name: entry.name.to_std_string_lossy(),
                value,
            })
        })
        .collect()
}
pub fn from_entries(entries: Vec<EntrySnapshot>, context: &mut Context) -> JsResult<JsObject> {
    validate_entries(&entries).map_err(|_| limit())?;
    let mut data = FormData {
        entries: Vec::with_capacity(entries.len()),
        size: 0,
    };
    for entry in entries {
        let size = entry_size(&entry);
        let value = match entry.value {
            EntryValue::Text(value) => JsString::from(value).into(),
            EntryValue::File(file) => web_blob::file_from_bytes(
                file.bytes,
                &file.media_type,
                &file.name,
                file.last_modified,
                context,
            )?
            .into(),
        };
        data.size += size;
        data.entries.push(Entry {
            name: JsString::from(entry.name),
            value,
            size,
        });
    }
    FormData::from_data(data, context)
}
fn create_entry(args: &[JsValue], context: &mut Context) -> JsResult<Entry> {
    required(args, 2)?;
    let name = usv(&args[0], context)?;
    let blob = web_blob::snapshot(&args[1]);
    let (value, value_size) = if let Some(blob) = blob {
        let filename = if args.get_or_undefined(2).is_undefined() {
            None
        } else {
            Some(usv(&args[2], context)?.to_std_string_lossy())
        };
        let existing = blob.file.as_ref();
        let final_name = filename
            .as_deref()
            .or_else(|| existing.map(|f| f.name.as_str()))
            .unwrap_or("blob");
        let size = blob
            .bytes
            .len()
            .saturating_add(final_name.len())
            .saturating_add(blob.media_type.len());
        let value = if existing.is_some() && filename.is_none() {
            args[1].clone()
        } else {
            let modified = existing
                .map(|f| f.last_modified)
                .unwrap_or_else(|| context.clock().now().millis_since_epoch() as i64);
            web_blob::file_from_bytes(blob.bytes, &blob.media_type, final_name, modified, context)?
                .into()
        };
        (value, size)
    } else {
        // With 3 or more arguments only the Blob overload exists, even when
        // filename is undefined. Extra arguments beyond arity 3 are ignored.
        if args.len() >= 3 {
            return Err(typ("FormData filename requires a Blob value"));
        }
        let value = usv(&args[1], context)?;
        let size = value.to_std_string_lossy().len();
        (value.into(), size)
    };
    let size = name.to_std_string_lossy().len().saturating_add(value_size);
    if size > MAX_DATA {
        return Err(limit());
    }
    Ok(Entry { name, value, size })
}
#[derive(Clone, Copy)]
enum Method {
    Append,
    Set,
    Delete,
    Get,
    GetAll,
    Has,
    ForEach,
}
fn method(
    this: &JsValue,
    args: &[JsValue],
    kind: Method,
    context: &mut Context,
) -> JsResult<JsValue> {
    let object = receiver(this)?;
    if matches!(kind, Method::ForEach) {
        required(args, 1)?;
        let callback = args[0]
            .as_callable()
            .ok_or_else(|| typ("FormData callback must be callable"))?;
        let this_arg = args.get_or_undefined(1);
        let mut index = 0;
        loop {
            let entry = object
                .downcast_ref::<FormData>()
                .and_then(|f| f.entries.get(index).cloned());
            let Some(entry) = entry else {
                break;
            };
            if index >= MAX_ENTRIES {
                return Err(limit());
            }
            index += 1;
            callback.call(
                this_arg,
                &[entry.value.clone(), entry.name.clone().into(), this.clone()],
                context,
            )?;
        }
        return Ok(JsValue::undefined());
    }
    if matches!(kind, Method::Append | Method::Set) {
        let entry = create_entry(args, context)?;
        // No native borrow across user conversions: append/set may reenter.
        let mut form = object
            .downcast_mut::<FormData>()
            .ok_or_else(|| typ("FormData receiver required"))?;
        let mut removed_size = 0usize;
        let mut removed_count = 0usize;
        if matches!(kind, Method::Set) {
            for old in &form.entries {
                if old.name == entry.name {
                    removed_size += old.size;
                    removed_count += 1;
                }
            }
        }
        let size = form.size - removed_size + entry.size;
        if size > MAX_DATA || form.entries.len() - removed_count >= MAX_ENTRIES {
            return Err(limit());
        }
        if removed_count == 0 {
            form.entries.push(entry);
        } else {
            let mut first = true;
            form.entries.retain_mut(|old| {
                if old.name != entry.name {
                    return true;
                }
                if first {
                    *old = entry.clone();
                    first = false;
                    true
                } else {
                    false
                }
            });
        }
        form.size = size;
        return Ok(JsValue::undefined());
    }
    required(args, 1)?;
    let name = usv(&args[0], context)?;
    if matches!(kind, Method::Delete) {
        let mut form = object
            .downcast_mut::<FormData>()
            .ok_or_else(|| typ("FormData receiver required"))?;
        let mut removed = 0;
        form.entries.retain(|entry| {
            if entry.name == name {
                removed += entry.size;
                false
            } else {
                true
            }
        });
        form.size -= removed;
        return Ok(JsValue::undefined());
    }
    let values = {
        let form = object
            .downcast_ref::<FormData>()
            .ok_or_else(|| typ("FormData receiver required"))?;
        form.entries
            .iter()
            .filter(|entry| entry.name == name)
            .map(|entry| entry.value.clone())
            .collect::<Vec<_>>()
    };
    Ok(match kind {
        Method::Has => (!values.is_empty()).into(),
        Method::GetAll => JsArray::from_iter(values, context).into(),
        _ => values.into_iter().next().unwrap_or(JsValue::null()),
    })
}
impl Class for FormData {
    const NAME: &'static str = "FormData";
    const ATTRIBUTES: Attribute = Attribute::WRITABLE.union(Attribute::CONFIGURABLE);
    fn data_constructor(_: &JsValue, args: &[JsValue], _: &mut Context) -> JsResult<Self> {
        if !args.get_or_undefined(0).is_undefined()
            || !args.get_or_undefined(1).is_null_or_undefined()
        {
            return Err(typ("Native FormData has no DOM form/submitter capability"));
        }
        Ok(Self {
            entries: vec![],
            size: 0,
        })
    }
    fn init(class: &mut ClassBuilder) -> JsResult<()> {
        for (name, length, kind) in [
            ("append", 2, Method::Append),
            ("set", 2, Method::Set),
            ("delete", 1, Method::Delete),
            ("get", 1, Method::Get),
            ("getAll", 1, Method::GetAll),
            ("has", 1, Method::Has),
            ("forEach", 1, Method::ForEach),
        ] {
            class.method(
                JsString::from(name),
                length,
                NativeFunction::from_copy_closure(move |this, args, ctx| {
                    method(this, args, kind, ctx)
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
                NativeFunction::from_copy_closure(move |this, _, ctx| iterator(this, kind, ctx)),
            );
        }
        class.property(
            JsSymbol::to_string_tag(),
            JsString::from("FormData"),
            Attribute::CONFIGURABLE,
        );
        Ok(())
    }
}
fn iterator(this: &JsValue, kind: Iteration, context: &mut Context) -> JsResult<JsValue> {
    let form = receiver(this)?;
    let prototype = context
        .get_data::<Registration>()
        .ok_or_else(|| typ("FormData capability not registered"))?
        .iterator_prototype
        .clone();
    Ok(JsObject::from_proto_and_data(
        prototype,
        FormIterator {
            form,
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
        .ok_or_else(|| typ("FormData iterator receiver required"))?;
    let (entry, kind) = {
        let mut iterator = object
            .downcast_mut::<FormIterator>()
            .ok_or_else(|| typ("FormData iterator receiver required"))?;
        let entry = if iterator.done {
            None
        } else {
            iterator
                .form
                .downcast_ref::<FormData>()
                .and_then(|f| f.entries.get(iterator.index).cloned())
        };
        if entry.is_some() {
            if iterator.index >= MAX_ENTRIES {
                return Err(limit());
            }
            iterator.index += 1;
        } else {
            iterator.done = true;
        }
        (entry, iterator.kind)
    };
    let Some(entry) = entry else {
        return Ok(create_iter_result_object(
            JsValue::undefined(),
            true,
            context,
        ));
    };
    let value = match kind {
        Iteration::Keys => entry.name.clone().into(),
        Iteration::Values => entry.value.clone(),
        Iteration::Entries => {
            JsArray::from_iter([entry.name.clone().into(), entry.value.clone()], context).into()
        }
    };
    Ok(create_iter_result_object(value, false, context))
}
pub fn register(context: &mut Context) -> JsResult<()> {
    if context.has_data::<Registration>()
        || context
            .global_object()
            .has_property(JsString::from("FormData"), context)?
    {
        return Err(typ("FormData registration would overwrite a global"));
    }
    // Registered native class lookup, not a user-controlled global constructor.
    web_blob::file_from_bytes(vec![], "", "", 0, context)?;
    context.register_global_class::<FormData>()?;
    let prototype = context
        .get_global_class::<FormData>()
        .ok_or_else(|| typ("Missing FormData class"))?
        .prototype();
    for name in [
        "append", "set", "delete", "get", "getAll", "has", "forEach", "keys", "values", "entries",
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
            .configurable(true),
        context,
    )?;
    iterator_prototype.define_property_or_throw(
        JsSymbol::to_string_tag(),
        PropertyDescriptor::builder()
            .value(JsString::from("FormData Iterator"))
            .configurable(true),
        context,
    )?;
    context.insert_data(Registration { iterator_prototype });
    Ok(())
}

fn crlf(value: &str) -> String {
    let mut result = String::with_capacity(value.len());
    let mut chars = value.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\r' || ch == '\n' {
            if ch == '\r' && chars.peek() == Some(&'\n') {
                chars.next();
            }
            result.push_str("\r\n");
        } else {
            result.push(ch);
        }
    }
    result
}
fn escaped(value: &str) -> String {
    value
        .replace('\r', "%0D")
        .replace('\n', "%0A")
        .replace('"', "%22")
}
fn push(output: &mut Vec<u8>, bytes: &[u8]) -> Result<(), String> {
    if output.len().saturating_add(bytes.len()) > MAX_BODY {
        return Err("Form body exceeds 32 MiB".into());
    }
    output.extend_from_slice(bytes);
    Ok(())
}
fn boundary_valid(boundary: &str) -> Result<(), String> {
    if boundary.is_empty()
        || boundary.len() > 70
        || boundary.ends_with(' ')
        || !boundary
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"'()+_,-./:=? ".contains(&b))
    {
        return Err("Invalid RFC7578 multipart boundary".into());
    }
    Ok(())
}
/// Encode a snapshot with an explicit host-chosen boundary. Collision checking
/// rejects ambiguous payloads before returning any artifact. No hidden RNG.
pub fn encode_multipart(entries: &[EntrySnapshot], boundary: &str) -> Result<EncodedForm, String> {
    validate_entries(entries)?;
    boundary_valid(boundary)?;
    let mut bytes = Vec::new();
    let delimiter = format!("--{boundary}");
    for entry in entries {
        let name = escaped(&crlf(&entry.name));
        let mut part = format!("Content-Disposition: form-data; name=\"{name}\"").into_bytes();
        match &entry.value {
            EntryValue::Text(value) => {
                push(&mut part, b"\r\n\r\n")?;
                push(&mut part, crlf(value).as_bytes())?;
            }
            EntryValue::File(file) => {
                // HTML normalizes field names/string values, not file names.
                let mime = if file.media_type.is_empty() {
                    "application/octet-stream"
                } else {
                    &file.media_type
                };
                push(
                    &mut part,
                    format!(
                        "; filename=\"{}\"\r\nContent-Type: {mime}\r\n\r\n",
                        escaped(&file.name)
                    )
                    .as_bytes(),
                )?;
                push(&mut part, &file.bytes)?;
            }
        }
        if part
            .windows(delimiter.len())
            .any(|window| window == delimiter.as_bytes())
        {
            return Err(
                "Multipart boundary collides with field data; choose another boundary".into(),
            );
        }
        push(&mut bytes, delimiter.as_bytes())?;
        push(&mut bytes, b"\r\n")?;
        push(&mut bytes, &part)?;
        push(&mut bytes, b"\r\n")?;
    }
    push(&mut bytes, format!("{delimiter}--\r\n").as_bytes())?;
    let boundary_parameter = if boundary.bytes().all(token) {
        boundary.to_owned()
    } else {
        format!("\"{boundary}\"")
    };
    Ok(EncodedForm {
        bytes,
        content_type: format!("multipart/form-data; boundary={boundary_parameter}"),
    })
}
/// HTML form encoding: files become filenames; names/values normalize newlines.
/// URLSearchParams body serialization is distinct and must use its own list.
pub fn encode_urlencoded(entries: &[EntrySnapshot]) -> Result<EncodedForm, String> {
    validate_entries(entries)?;
    let mut bytes = Vec::new();
    for entry in entries {
        let value = match &entry.value {
            EntryValue::Text(text) => text,
            EntryValue::File(file) => &file.name,
        };
        let pair = url::form_urlencoded::Serializer::new(String::new())
            .append_pair(&crlf(&entry.name), &crlf(value))
            .finish();
        if !bytes.is_empty() {
            push(&mut bytes, b"&")?;
        }
        push(&mut bytes, pair.as_bytes())?;
    }
    Ok(EncodedForm {
        bytes,
        content_type: "application/x-www-form-urlencoded;charset=UTF-8".into(),
    })
}
pub fn decode_urlencoded(bytes: &[u8]) -> Result<Vec<EntrySnapshot>, String> {
    if bytes.len() > MAX_BODY {
        return Err("Form body exceeds 32 MiB".into());
    }
    let mut entries = Vec::new();
    let mut size = 0usize;
    for (name, value) in url::form_urlencoded::parse(bytes) {
        let entry = EntrySnapshot {
            name: name.into_owned(),
            value: EntryValue::Text(value.into_owned()),
        };
        size = size.saturating_add(entry_size(&entry));
        if entries.len() == MAX_ENTRIES || size > MAX_DATA {
            return Err("Decoded FormData exceeds native bounds".into());
        }
        entries.push(entry);
    }
    Ok(entries)
}
fn token(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte)
}
/// Parse MIME disposition parameters independently of multer's case-sensitive
/// name lookup. Quoted semicolons, UTF-8, and HTML percent escapes are supported;
/// RFC7578 forbids filename*, which is rejected explicitly rather than lost.
fn disposition(header: &[u8]) -> Result<(String, Option<String>), String> {
    let mut index = 0;
    let skip = |index: &mut usize| {
        while header
            .get(*index)
            .is_some_and(|b| matches!(b, b' ' | b'\t'))
        {
            *index += 1;
        }
    };
    skip(&mut index);
    let start = index;
    while header.get(index).is_some_and(|b| token(*b)) {
        index += 1;
    }
    if !header[start..index].eq_ignore_ascii_case(b"form-data") {
        return Err("Multipart disposition must be form-data".into());
    }
    let mut name = None;
    let mut filename = None;
    loop {
        skip(&mut index);
        if index == header.len() {
            break;
        }
        if header[index] != b';' {
            return Err("Invalid multipart disposition parameter separator".into());
        }
        index += 1;
        skip(&mut index);
        let start = index;
        while header.get(index).is_some_and(|b| token(*b)) {
            index += 1;
        }
        if index == start {
            return Err("Empty multipart disposition parameter".into());
        }
        let key = &header[start..index];
        skip(&mut index);
        if header.get(index) != Some(&b'=') {
            return Err("Missing multipart disposition parameter equals".into());
        }
        index += 1;
        skip(&mut index);
        let mut value = Vec::new();
        if header.get(index) == Some(&b'"') {
            index += 1;
            loop {
                let byte = *header
                    .get(index)
                    .ok_or("Unterminated multipart disposition quote")?;
                index += 1;
                if byte == b'"' {
                    break;
                }
                if matches!(byte, b'\r' | b'\n' | 0) {
                    return Err("Invalid multipart disposition quoted byte".into());
                }
                // HTML encoders leave backslashes literal and escape quotes as
                // %22. Preserve even a trailing backslash before closing quote.
                value.push(byte);
            }
        } else {
            let start = index;
            while header.get(index).is_some_and(|b| token(*b)) {
                index += 1;
            }
            if start == index {
                return Err("Empty unquoted multipart parameter".into());
            }
            value.extend_from_slice(&header[start..index]);
        }
        if key.eq_ignore_ascii_case(b"filename*") {
            return Err("RFC7578 forbids extended filename* parameters".into());
        }
        let value = String::from_utf8_lossy(&value).into_owned();
        // Decode the three escapes emitted by browser HTML multipart encoding.
        let mut decoded = String::new();
        let raw = value.as_bytes();
        let mut i = 0;
        while i < raw.len() {
            if raw[i] == b'%' && i + 2 < raw.len() {
                let pair = &raw[i + 1..i + 3];
                let byte = if pair.eq_ignore_ascii_case(b"0d") {
                    Some('\r')
                } else if pair.eq_ignore_ascii_case(b"0a") {
                    Some('\n')
                } else if pair == b"22" {
                    Some('"')
                } else {
                    None
                };
                if let Some(ch) = byte {
                    decoded.push(ch);
                    i += 3;
                    continue;
                }
            }
            let ch = value[i..]
                .chars()
                .next()
                .ok_or("Invalid UTF-8 parameter offset")?;
            decoded.push(ch);
            i += ch.len_utf8();
        }
        if key.eq_ignore_ascii_case(b"name") && name.is_none() {
            name = Some(decoded);
        } else if key.eq_ignore_ascii_case(b"filename") && filename.is_none() {
            filename = Some(decoded);
        }
    }
    Ok((name.ok_or("Multipart disposition has no name")?, filename))
}
/// Fully materialized, bounded multipart decoding. The future consumes an
/// in-memory ready stream, never network/filesystem data. Unknown extended
/// filenames, transfer encodings and nested multipart are explicit errors.
/// Repeated part headers use multer's last-value policy; duplicate disposition
/// parameters use the first value. This is HTTP form data, not general MIME.
pub async fn decode_multipart(
    bytes: Vec<u8>,
    content_type: &str,
    last_modified: i64,
) -> Result<Vec<EntrySnapshot>, String> {
    if bytes.len() > MAX_BODY || content_type.len() > 4096 {
        return Err("Form body/header exceeds native bounds".into());
    }
    let boundary = multer::parse_boundary(content_type)
        .map_err(|error| format!("Invalid multipart content type: {error}"))?;
    boundary_valid(&boundary)?;
    let stream = futures_util::stream::iter([Ok::<_, std::io::Error>(bytes)]);
    let constraints = multer::Constraints::new().size_limit(
        multer::SizeLimit::new()
            .whole_stream(MAX_BODY as u64)
            .per_field(MAX_DATA as u64),
    );
    let mut multipart = multer::Multipart::with_constraints(stream, boundary, constraints);
    let mut entries = Vec::new();
    let mut size = 0usize;
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|error| format!("Invalid multipart body: {error}"))?
    {
        if entries.len() == MAX_ENTRIES {
            return Err("Multipart exceeds 10000 entries".into());
        }
        let headers = field.headers();
        if headers.contains_key("content-transfer-encoding") {
            return Err(
                "Content-Transfer-Encoding is unsupported in HTTP multipart form data".into(),
            );
        }
        let header = headers
            .get("content-disposition")
            .ok_or("Missing multipart content disposition")?;
        let (name, filename) = disposition(header.as_bytes())?;
        let mime = match headers.get("content-type") {
            Some(value) => String::from_utf8_lossy(value.as_bytes()).into_owned(),
            None => "text/plain".into(),
        };
        if mime.to_ascii_lowercase().starts_with("multipart/") {
            return Err("Nested multipart form data is unsupported".into());
        }
        let bytes = field
            .bytes()
            .await
            .map_err(|error| format!("Invalid multipart field: {error}"))?
            .to_vec();
        let value = if let Some(filename) = filename {
            let media_type = if mime.bytes().all(|b| (0x20..=0x7e).contains(&b)) {
                mime.to_ascii_lowercase()
            } else {
                String::new()
            };
            EntryValue::File(FileSnapshot {
                name: filename,
                bytes,
                media_type,
                last_modified,
            })
        } else {
            // Fetch requires UTF-8 decode WITHOUT BOM removal and ignores charset.
            EntryValue::Text(String::from_utf8_lossy(&bytes).into_owned())
        };
        let entry = EntrySnapshot { name, value };
        size = size.saturating_add(entry_size(&entry));
        if size > MAX_DATA {
            return Err("Decoded multipart exceeds 16 MiB".into());
        }
        entries.push(entry);
    }
    Ok(entries)
}
#[cfg(test)]
#[path = "web_form_data_tests.rs"]
mod tests;
