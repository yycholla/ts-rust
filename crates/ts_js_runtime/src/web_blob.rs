//! Explicit, in-memory File API capability. No filesystem or JavaScript fallback.
//! Implements https://w3c.github.io/FileAPI/ and Web IDL sequence/BufferSource
//! conversions. Call register before application code mutates intrinsic prototypes.
//! Blob storage is immutable and bounded to 16 MiB, with at most 100,000 parts.
//! Native endings use CRLF on Windows and LF elsewhere (including wasm32).
//! stream() creates a fresh native readable byte stream over immutable data.
use boa_engine::{
    Context, JsArgs, JsData, JsError, JsNativeError, JsObject, JsResult, JsString, JsSymbol,
    JsValue, NativeFunction,
    builtins::object::OrdinaryObject,
    class::{Class, ClassBuilder},
    object::{
        FunctionObjectBuilder,
        builtins::{JsArrayBuffer, JsDataView, JsPromise, JsTypedArray, JsUint8Array},
    },
    property::{Attribute, PropertyDescriptor},
};
use boa_gc::{Finalize, Trace};
use std::rc::Rc;

const MAX_BYTES: usize = 16 * 1024 * 1024;
const MAX_PARTS: usize = 100_000;
fn typ(message: &str) -> JsError {
    JsNativeError::typ().with_message(message.to_owned()).into()
}
fn bound(length: usize) -> JsResult<()> {
    if length > MAX_BYTES {
        Err(JsNativeError::range()
            .with_message("Blob exceeds the native 16 MiB limit")
            .into())
    } else {
        Ok(())
    }
}
fn string(value: &JsValue, context: &mut Context) -> JsResult<JsString> {
    let value = value.to_string(context)?;
    bound(value.len())?;
    Ok(value)
}
fn media_type(value: &JsString) -> String {
    if value.iter().any(|unit| !(0x20..=0x7e).contains(&unit)) {
        String::new()
    } else {
        value.to_std_string_lossy().to_ascii_lowercase()
    }
}
#[derive(Clone)]
struct Data {
    bytes: Rc<[u8]>,
    start: usize,
    end: usize,
    media_type: String,
}
impl Data {
    fn new(bytes: Vec<u8>, media_type: String) -> JsResult<Self> {
        bound(bytes.len())?;
        let end = bytes.len();
        Ok(Self {
            bytes: bytes.into(),
            start: 0,
            end,
            media_type,
        })
    }
    fn bytes(&self) -> &[u8] {
        &self.bytes[self.start..self.end]
    }
}
#[derive(Trace, Finalize, JsData)]
struct Blob {
    #[unsafe_ignore_trace]
    data: Data,
}
#[derive(Trace, Finalize, JsData)]
struct File {
    #[unsafe_ignore_trace]
    data: Data,
    name: JsString,
    #[unsafe_ignore_trace]
    last_modified: i64,
}
/// Copied immutable payload and optional constructed-file metadata, without JS getters.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlobSnapshot {
    pub bytes: Vec<u8>,
    pub media_type: String,
    pub file: Option<FileMetadata>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileMetadata {
    pub name: String,
    pub last_modified: i64,
    pub webkit_relative_path: String,
}
pub fn snapshot(value: &JsValue) -> Option<BlobSnapshot> {
    let object = value.as_object()?;
    if let Some(file) = object.downcast_ref::<File>() {
        return Some(BlobSnapshot {
            bytes: file.data.bytes().to_vec(),
            media_type: file.data.media_type.clone(),
            file: Some(FileMetadata {
                name: file.name.to_std_string_lossy(),
                last_modified: file.last_modified,
                webkit_relative_path: String::new(),
            }),
        });
    }
    let blob = object.downcast_ref::<Blob>()?;
    Some(BlobSnapshot {
        bytes: blob.data.bytes().to_vec(),
        media_type: blob.data.media_type.clone(),
        file: None,
    })
}
fn data(value: &JsValue) -> JsResult<Data> {
    let object = value
        .as_object()
        .ok_or_else(|| typ("Blob receiver required"))?;
    if let Some(blob) = object.downcast_ref::<Blob>() {
        return Ok(blob.data.clone());
    }
    if let Some(file) = object.downcast_ref::<File>() {
        return Ok(file.data.clone());
    }
    Err(typ("Blob receiver required"))
}
pub fn from_bytes(bytes: Vec<u8>, mime: &str, context: &mut Context) -> JsResult<JsObject> {
    bound(mime.len())?;
    Blob::from_data(
        Blob {
            data: Data::new(bytes, media_type(&JsString::from(mime)))?,
        },
        context,
    )
}
pub fn file_from_bytes(
    bytes: Vec<u8>,
    mime: &str,
    name: &str,
    last_modified: i64,
    context: &mut Context,
) -> JsResult<JsObject> {
    bound(mime.len())?;
    bound(name.len())?;
    File::from_data(
        File {
            data: Data::new(bytes, media_type(&JsString::from(mime)))?,
            name: JsString::from(name),
            last_modified,
        },
        context,
    )
}

#[derive(Trace, Finalize, JsData)]
struct Intrinsics {
    resizable: JsObject,
}
fn fixed_buffer(object: JsObject, context: &mut Context) -> JsResult<JsArrayBuffer> {
    let buffer = JsArrayBuffer::from_object(object.clone())
        .map_err(|_| typ("Blob BufferSource must not be shared"))?;
    let getter = context
        .get_data::<Intrinsics>()
        .ok_or_else(|| typ("Blob capability not registered"))?
        .resizable
        .clone();
    if getter.call(&object.into(), &[], context)?.to_boolean() {
        return Err(typ("Blob BufferSource must not be resizable"));
    }
    Ok(buffer)
}
enum Part {
    Text(String),
    Blob(Data),
    Buffer(JsObject),
}
fn convert_part(value: JsValue, context: &mut Context) -> JsResult<Part> {
    if let Ok(data) = data(&value) {
        return Ok(Part::Blob(data));
    }
    if let Some(object) = value.as_object() {
        let buffer = if JsArrayBuffer::from_object(object.clone()).is_ok() {
            Some(object.clone())
        } else if let Ok(view) = JsTypedArray::from_object(object.clone()) {
            view.buffer(context)?.as_object()
        } else if let Ok(view) = JsDataView::from_object(object.clone()) {
            view.buffer(context)?.as_object()
        } else {
            None
        };
        if let Some(buffer) = buffer {
            fixed_buffer(buffer, context)?;
            return Ok(Part::Buffer(object));
        }
    }
    Ok(Part::Text(string(&value, context)?.to_std_string_lossy()))
}
fn parts(value: &JsValue, context: &mut Context) -> JsResult<Vec<Part>> {
    let object = value
        .as_object()
        .ok_or_else(|| typ("Blob parts must be an iterable object"))?;
    let method = object
        .get(JsSymbol::iterator(), context)?
        .as_callable()
        .ok_or_else(|| typ("Blob parts must be iterable"))?;
    let iterator = method
        .call(value, &[], context)?
        .as_object()
        .ok_or_else(|| typ("Invalid Blob parts iterator"))?;
    let next = iterator
        .get(JsString::from("next"), context)?
        .as_callable()
        .ok_or_else(|| typ("Invalid iterator next"))?;
    let mut result = Vec::new();
    let mut converted_bytes = 0usize;
    loop {
        let item = next
            .call(&iterator.clone().into(), &[], context)?
            .as_object()
            .ok_or_else(|| typ("Iterator result must be an object"))?;
        if item.get(JsString::from("done"), context)?.to_boolean() {
            return Ok(result);
        }
        if result.len() == MAX_PARTS {
            return Err(JsNativeError::range()
                .with_message("Blob part count exceeds 100000")
                .into());
        }
        // Web IDL creating-a-sequence propagates abrupt completion without IteratorClose.
        let part = convert_part(item.get(JsString::from("value"), context)?, context)?;
        if let Part::Text(text) = &part {
            converted_bytes += text.len();
            bound(converted_bytes)?;
        }
        result.push(part);
    }
}
fn long_long(value: &JsValue, context: &mut Context) -> JsResult<i64> {
    let number = value.to_number(context)?;
    if !number.is_finite() || number == 0.0 {
        return Ok(0);
    }
    // Keep the remainder signed. Euclidean remainder adds 2^64 to small
    // negative values, rounding -1/-3 to exactly 2^64 in f64 and losing them.
    let remainder = number.trunc() % 18446744073709551616.0;
    let signed = if remainder >= 9223372036854775808.0 {
        remainder - 18446744073709551616.0
    } else if remainder < -9223372036854775808.0 {
        remainder + 18446744073709551616.0
    } else {
        remainder
    };
    Ok(signed as i64)
}
struct Options {
    native: bool,
    mime: String,
    modified: Option<i64>,
}
fn options(value: &JsValue, file: bool, context: &mut Context) -> JsResult<Options> {
    let object = if value.is_null_or_undefined() {
        None
    } else {
        Some(
            value
                .as_object()
                .ok_or_else(|| typ("Blob options must be a dictionary"))?,
        )
    };
    let get = |name: &str, context: &mut Context| -> JsResult<JsValue> {
        match &object {
            Some(object) => object.get(JsString::from(name), context),
            None => Ok(JsValue::undefined()),
        }
    };
    let ending = get("endings", context)?;
    let native = if ending.is_undefined() {
        false
    } else {
        match string(&ending, context)?.to_std_string_lossy().as_str() {
            "transparent" => false,
            "native" => true,
            _ => return Err(typ("Invalid Blob endings value")),
        }
    };
    let mime = get("type", context)?;
    let mime = if mime.is_undefined() {
        String::new()
    } else {
        media_type(&string(&mime, context)?)
    };
    let modified = if file {
        let value = get("lastModified", context)?;
        if value.is_undefined() {
            None
        } else {
            Some(long_long(&value, context)?)
        }
    } else {
        None
    };
    Ok(Options {
        native,
        mime,
        modified,
    })
}
fn copy_buffer(object: JsObject, context: &mut Context) -> JsResult<Vec<u8>> {
    let (buffer, view) = if JsArrayBuffer::from_object(object.clone()).is_ok() {
        (object.clone(), None)
    } else if let Ok(array) = JsTypedArray::from_object(object.clone()) {
        (
            array
                .buffer(context)?
                .as_object()
                .ok_or_else(|| typ("Invalid buffer"))?,
            Some(false),
        )
    } else {
        (
            JsDataView::from_object(object.clone())?
                .buffer(context)?
                .as_object()
                .ok_or_else(|| typ("Invalid buffer"))?,
            Some(true),
        )
    };
    let buffer = fixed_buffer(buffer, context)?;
    if buffer.data().is_none() {
        return Ok(vec![]);
    }
    let (offset, length) = match view {
        None => (0, buffer.byte_length()),
        Some(false) => {
            let view = JsTypedArray::from_object(object)?;
            (view.byte_offset(context)?, view.byte_length(context)?)
        }
        Some(true) => {
            let view = JsDataView::from_object(object)?;
            (
                view.byte_offset(context)? as usize,
                view.byte_length(context)? as usize,
            )
        }
    };
    bound(length)?;
    let bytes = buffer.data().ok_or_else(|| typ("Detached Blob buffer"))?;
    Ok(bytes
        .get(offset..offset + length)
        .ok_or_else(|| typ("Invalid Blob buffer range"))?
        .to_vec())
}
fn append(bytes: &mut Vec<u8>, more: &[u8]) -> JsResult<()> {
    bound(bytes.len().saturating_add(more.len()))?;
    bytes.extend_from_slice(more);
    Ok(())
}
fn process(parts: Vec<Part>, options: &Options, context: &mut Context) -> JsResult<Data> {
    let mut bytes = Vec::new();
    for part in parts {
        match part {
            Part::Blob(blob) => append(&mut bytes, blob.bytes())?,
            Part::Buffer(object) => append(&mut bytes, &copy_buffer(object, context)?)?,
            Part::Text(text) if options.native => {
                let mut chars = text.chars().peekable();
                while let Some(ch) = chars.next() {
                    if ch == '\r' || ch == '\n' {
                        if ch == '\r' && chars.peek() == Some(&'\n') {
                            chars.next();
                        }
                        append(&mut bytes, if cfg!(windows) { b"\r\n" } else { b"\n" })?;
                    } else {
                        let mut utf8 = [0; 4];
                        append(&mut bytes, ch.encode_utf8(&mut utf8).as_bytes())?;
                    }
                }
            }
            Part::Text(text) => append(&mut bytes, text.as_bytes())?,
        }
    }
    Data::new(bytes, options.mime.clone())
}
impl Class for Blob {
    const NAME: &'static str = "Blob";
    const ATTRIBUTES: Attribute = Attribute::WRITABLE.union(Attribute::CONFIGURABLE);
    fn data_constructor(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<Self> {
        let parts = if args.get_or_undefined(0).is_undefined() {
            vec![]
        } else {
            parts(&args[0], context)?
        };
        let options = options(args.get_or_undefined(1), false, context)?;
        Ok(Self {
            data: process(parts, &options, context)?,
        })
    }
    fn init(class: &mut ClassBuilder) -> JsResult<()> {
        for name in ["size", "type"] {
            let getter = FunctionObjectBuilder::new(
                class.context().realm(),
                NativeFunction::from_copy_closure(move |this, _, _| {
                    let data = data(this)?;
                    Ok(if name == "size" {
                        (data.bytes().len() as f64).into()
                    } else {
                        JsString::from(data.media_type).into()
                    })
                }),
            )
            .name(JsString::from(format!("get {name}")))
            .build();
            class.accessor(
                JsString::from(name),
                Some(getter),
                None,
                Attribute::ENUMERABLE.union(Attribute::CONFIGURABLE),
            );
        }
        class.method(
            JsString::from("slice"),
            0,
            NativeFunction::from_fn_ptr(slice),
        );
        class.method(
            JsString::from("stream"),
            0,
            NativeFunction::from_copy_closure(|this, _, context| {
                let data = data(this)?;
                crate::web_streams::from_bytes(data.bytes().to_vec(), context).map(Into::into)
            }),
        );
        for name in ["text", "arrayBuffer", "bytes"] {
            class.method(
                JsString::from(name),
                0,
                NativeFunction::from_copy_closure(move |this, _, context| {
                    read(this, name, context)
                }),
            );
        }
        class.property(
            JsSymbol::to_string_tag(),
            JsString::from("Blob"),
            Attribute::CONFIGURABLE,
        );
        Ok(())
    }
}
impl Class for File {
    const NAME: &'static str = "File";
    const LENGTH: usize = 2;
    const ATTRIBUTES: Attribute = Attribute::WRITABLE.union(Attribute::CONFIGURABLE);
    fn data_constructor(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<Self> {
        if args.len() < 2 {
            return Err(typ("File requires parts and a name"));
        }
        let parts = parts(&args[0], context)?;
        let name = JsString::from(string(&args[1], context)?.to_std_string_lossy());
        let options = options(args.get_or_undefined(2), true, context)?;
        let last_modified = options
            .modified
            .unwrap_or_else(|| context.clock().system_time_millis());
        Ok(Self {
            data: process(parts, &options, context)?,
            name,
            last_modified,
        })
    }
    fn init(class: &mut ClassBuilder) -> JsResult<()> {
        for name in ["name", "lastModified", "webkitRelativePath"] {
            let getter = FunctionObjectBuilder::new(
                class.context().realm(),
                NativeFunction::from_copy_closure(move |this, _, _| {
                    let object = this
                        .as_object()
                        .ok_or_else(|| typ("File receiver required"))?;
                    let file = object
                        .downcast_ref::<File>()
                        .ok_or_else(|| typ("File receiver required"))?;
                    Ok(match name {
                        "name" => file.name.clone().into(),
                        "lastModified" => (file.last_modified as f64).into(),
                        _ => JsString::from("").into(),
                    })
                }),
            )
            .name(JsString::from(format!("get {name}")))
            .build();
            class.accessor(
                JsString::from(name),
                Some(getter),
                None,
                Attribute::ENUMERABLE.union(Attribute::CONFIGURABLE),
            );
        }
        class.property(
            JsSymbol::to_string_tag(),
            JsString::from("File"),
            Attribute::CONFIGURABLE,
        );
        Ok(())
    }
}
fn slice(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let original = data(this)?;
    let size = original.bytes().len() as i64;
    let start = long_long(args.get_or_undefined(0), context)?;
    let end = if args.get_or_undefined(1).is_undefined() {
        size
    } else {
        long_long(&args[1], context)?
    };
    let mime = if args.get_or_undefined(2).is_undefined() {
        String::new()
    } else {
        media_type(&string(&args[2], context)?)
    };
    let relative = |value: i64| if value < 0 { size.saturating_add(value).max(0) } else { value.min(size) } as usize;
    let start = relative(start);
    let end = relative(end).max(start);
    Ok(Blob::from_data(
        Blob {
            data: Data {
                bytes: original.bytes,
                start: original.start + start,
                end: original.start + end,
                media_type: mime,
            },
        },
        context,
    )?
    .into())
}
fn read(this: &JsValue, method: &str, context: &mut Context) -> JsResult<JsValue> {
    let result: JsResult<JsValue> = (|| {
        let data = data(this)?;
        match method {
            "text" => {
                let bytes = data
                    .bytes()
                    .strip_prefix(&[0xef, 0xbb, 0xbf])
                    .unwrap_or(data.bytes());
                Ok(JsString::from(String::from_utf8_lossy(bytes).as_ref()).into())
            }
            "bytes" => Ok(JsUint8Array::from_iter(data.bytes().iter().copied(), context)?.into()),
            _ => {
                let buffer = JsArrayBuffer::new(data.bytes().len(), context)?;
                buffer
                    .data_mut()
                    .ok_or_else(|| typ("Detached output buffer"))?
                    .copy_from_slice(data.bytes());
                Ok(buffer.into())
            }
        }
    })();
    Ok(match result {
        Ok(value) => JsPromise::resolve(value, context)?,
        Err(error) => JsPromise::reject(error, context)?,
    }
    .into())
}
/// Register before executing application code; does not enable other capabilities.
pub fn register(context: &mut Context) -> JsResult<()> {
    for name in ["Blob", "File"] {
        if context
            .global_object()
            .has_property(JsString::from(name), context)?
        {
            return Err(typ("Blob/File global already exists"));
        }
    }
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
    let getter = descriptor
        .as_object()
        .ok_or_else(|| typ("Missing intrinsic ArrayBuffer resizable getter"))?
        .get(JsString::from("get"), context)?
        .as_callable()
        .ok_or_else(|| typ("Invalid intrinsic ArrayBuffer resizable getter"))?;
    context.insert_data(Intrinsics { resizable: getter });
    context.register_global_class::<Blob>()?;
    context.register_global_class::<File>()?;
    let blob = context
        .get_global_class::<Blob>()
        .ok_or_else(|| typ("Missing Blob class"))?;
    let blob_proto = blob.prototype();
    let blob_ctor = blob.constructor();
    let file = context
        .get_global_class::<File>()
        .ok_or_else(|| typ("Missing File class"))?;
    file.prototype().set_prototype(Some(blob_proto.clone()));
    file.constructor().set_prototype(Some(blob_ctor));
    for name in ["slice", "text", "arrayBuffer", "bytes"] {
        blob_proto.define_property_or_throw(
            JsString::from(name),
            PropertyDescriptor::builder().enumerable(true),
            context,
        )?;
    }
    Ok(())
}
#[cfg(test)]
#[path = "web_blob_tests.rs"]
mod tests;
