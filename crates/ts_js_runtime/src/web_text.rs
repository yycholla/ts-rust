//! Explicit native Encoding Standard and forgiving-base64 host capability.
//! No filesystem/network access or JavaScript implementation. Each call accepts
//! at most 4 MiB of bytes or 4 Mi UTF-16 code units, with a bounded 12 MiB UTF-8
//! result. Decoder state and incomplete multibyte sequences persist only when
//! the caller requests streaming. Limits throw RangeError, never truncate.
use base64::{
    Engine as _, alphabet,
    engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig},
};
use boa_engine::{
    Context, JsArgs, JsData, JsError, JsNativeError, JsObject, JsResult, JsString, JsSymbol,
    JsValue, NativeFunction,
    builtins::object::OrdinaryObject,
    class::{Class, ClassBuilder},
    object::{
        FunctionObjectBuilder, ObjectInitializer,
        builtins::{JsArrayBuffer, JsDataView, JsSharedArrayBuffer, JsTypedArray, JsUint8Array},
    },
    property::{Attribute, PropertyDescriptor},
};
use boa_gc::{Finalize, Trace};
use encoding_rs::{CoderResult, Decoder, DecoderResult, Encoding};
use std::{cell::RefCell, rc::Rc};

const MAX_INPUT: usize = 4 * 1024 * 1024;
const MAX_OUTPUT: usize = 3 * MAX_INPUT + 16;
fn typ(message: &str) -> JsError {
    JsNativeError::typ().with_message(message.to_owned()).into()
}
fn range(message: &str) -> JsError {
    JsNativeError::range()
        .with_message(message.to_owned())
        .into()
}
fn bounded(length: usize, limit: usize) -> JsResult<()> {
    if length > limit {
        Err(range("Native text host allocation limit exceeded"))
    } else {
        Ok(())
    }
}
fn text(value: &JsValue, context: &mut Context) -> JsResult<JsString> {
    let text = value.to_string(context)?;
    bounded(text.len(), MAX_INPUT)?;
    Ok(text)
}
fn option(options: &JsValue, name: &str, context: &mut Context) -> JsResult<bool> {
    if options.is_null_or_undefined() {
        return Ok(false);
    }
    let object = options
        .as_object()
        .ok_or_else(|| typ("Text options must be an object"))?;
    Ok(object.get(JsString::from(name), context)?.to_boolean())
}
fn required(args: &[JsValue], count: usize) -> JsResult<()> {
    if args.len() < count {
        Err(typ("Insufficient text method arguments"))
    } else {
        Ok(())
    }
}

#[derive(Trace, Finalize, JsData)]
struct TextEncoder;
impl Class for TextEncoder {
    const NAME: &'static str = "TextEncoder";
    const ATTRIBUTES: Attribute = Attribute::WRITABLE.union(Attribute::CONFIGURABLE);
    fn data_constructor(_: &JsValue, _: &[JsValue], _: &mut Context) -> JsResult<Self> {
        Ok(Self)
    }
    fn init(class: &mut ClassBuilder) -> JsResult<()> {
        let getter = FunctionObjectBuilder::new(
            class.context().realm(),
            NativeFunction::from_fn_ptr(|this, _, _| {
                encoder_receiver(this)?;
                Ok(JsString::from("utf-8").into())
            }),
        )
        .name(JsString::from("get encoding"))
        .build();
        class.accessor(
            JsString::from("encoding"),
            Some(getter),
            None,
            Attribute::ENUMERABLE.union(Attribute::CONFIGURABLE),
        );
        class.method(
            JsString::from("encode"),
            0,
            NativeFunction::from_fn_ptr(encode),
        );
        class.method(
            JsString::from("encodeInto"),
            2,
            NativeFunction::from_fn_ptr(encode_into),
        );
        class.property(
            JsSymbol::to_string_tag(),
            JsString::from("TextEncoder"),
            Attribute::CONFIGURABLE,
        );
        Ok(())
    }
}
fn encoder_receiver(this: &JsValue) -> JsResult<()> {
    if this
        .as_object()
        .is_some_and(|object| object.is::<TextEncoder>())
    {
        Ok(())
    } else {
        Err(typ("TextEncoder method requires a TextEncoder receiver"))
    }
}
fn encode(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    encoder_receiver(this)?;
    let input = if args.get_or_undefined(0).is_undefined() {
        String::new()
    } else {
        text(&args[0], context)?.to_std_string_lossy()
    };
    bounded(input.len(), MAX_OUTPUT)?;
    Ok(JsUint8Array::from_iter(input.bytes(), context)?.into())
}
fn encode_into(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    encoder_receiver(this)?;
    required(args, 2)?;
    let input = text(&args[0], context)?.to_std_string_lossy();
    let destination = JsUint8Array::from_object(
        args[1]
            .as_object()
            .ok_or_else(|| typ("encodeInto destination must be Uint8Array"))?,
    )?;
    let buffer = destination
        .buffer(context)?
        .as_object()
        .ok_or_else(|| typ("Invalid destination buffer"))?;
    fixed_buffer(&buffer, context)?;
    // Detached output has zero capacity (WPT encoding/encodeInto.any.js).
    if detached(&buffer) {
        return Ok(ObjectInitializer::new(context)
            .property(JsString::from("read"), 0, Attribute::all())
            .property(JsString::from("written"), 0, Attribute::all())
            .build()
            .into());
    }
    let capacity = destination.byte_length(context)?;
    let mut read = 0usize;
    let mut output = Vec::with_capacity(input.len().min(capacity));
    for character in input.chars() {
        if character.len_utf8() > capacity - output.len() {
            break;
        }
        let mut bytes = [0; 4];
        output.extend_from_slice(character.encode_utf8(&mut bytes).as_bytes());
        read += character.len_utf16();
    }
    let buffer = destination
        .buffer(context)?
        .as_object()
        .ok_or_else(|| typ("Invalid destination buffer"))?;
    let offset = destination.byte_offset(context)?;
    if let Ok(buffer) = JsArrayBuffer::from_object(buffer.clone()) {
        let mut bytes = buffer
            .data_mut()
            .ok_or_else(|| typ("Detached destination buffer"))?;
        let target = bytes
            .get_mut(offset..offset + output.len())
            .ok_or_else(|| typ("Out-of-bounds destination"))?;
        target.copy_from_slice(&output);
    } else {
        // Shared destinations use Boa's integer-indexed writes (atomic byte
        // stores), never mutable non-atomic aliases of shared buffer memory.
        JsSharedArrayBuffer::from_object(buffer)?;
        for (index, byte) in output.iter().enumerate() {
            destination.set(index, u32::from(*byte), true, context)?;
        }
    }
    Ok(ObjectInitializer::new(context)
        .property(JsString::from("read"), read as f64, Attribute::all())
        .property(
            JsString::from("written"),
            output.len() as f64,
            Attribute::all(),
        )
        .build()
        .into())
}

#[derive(Trace, Finalize, JsData)]
struct BufferIntrinsics {
    resizable: JsObject,
    growable: JsObject,
}
fn fixed_buffer(buffer: &JsObject, context: &mut Context) -> JsResult<()> {
    let intrinsics = context
        .get_data::<BufferIntrinsics>()
        .ok_or_else(|| typ("Text capability not registered"))?;
    let getter = if JsArrayBuffer::from_object(buffer.clone()).is_ok() {
        intrinsics.resizable.clone()
    } else {
        JsSharedArrayBuffer::from_object(buffer.clone())?;
        intrinsics.growable.clone()
    };
    if getter
        .call(&buffer.clone().into(), &[], context)?
        .to_boolean()
    {
        return Err(typ("Text BufferSource must not be resizable or growable"));
    }
    Ok(())
}
fn detached(buffer: &JsObject) -> bool {
    JsArrayBuffer::from_object(buffer.clone()).is_ok_and(|buffer| buffer.data().is_none())
}
fn intrinsic_getter(prototype: JsObject, name: &str, context: &mut Context) -> JsResult<JsObject> {
    let descriptor = OrdinaryObject::get_own_property_descriptor(
        &JsValue::undefined(),
        &[prototype.into(), JsString::from(name).into()],
        context,
    )?;
    descriptor
        .as_object()
        .ok_or_else(|| typ("Missing intrinsic buffer descriptor"))?
        .get(JsString::from("get"), context)?
        .as_callable()
        .ok_or_else(|| typ("Invalid intrinsic buffer getter"))
}

#[derive(Clone)]
struct BufferView {
    buffer: JsObject,
    offset: usize,
    length: usize,
}
fn buffer_view(value: &JsValue, context: &mut Context) -> JsResult<Option<BufferView>> {
    if value.is_undefined() {
        return Ok(None);
    }
    let object = value
        .as_object()
        .ok_or_else(|| typ("TextDecoder input must be a BufferSource"))?;
    let (buffer, kind) = if let Ok(array) = JsTypedArray::from_object(object.clone()) {
        (
            array
                .buffer(context)?
                .as_object()
                .ok_or_else(|| typ("Invalid typed array buffer"))?,
            1,
        )
    } else if let Ok(view) = JsDataView::from_object(object.clone()) {
        (
            view.buffer(context)?
                .as_object()
                .ok_or_else(|| typ("Invalid DataView buffer"))?,
            2,
        )
    } else if JsArrayBuffer::from_object(object.clone()).is_ok()
        || JsSharedArrayBuffer::from_object(object.clone()).is_ok()
    {
        (object.clone(), 0)
    } else {
        return Err(typ("TextDecoder input must be a BufferSource"));
    };
    fixed_buffer(&buffer, context)?;
    let (offset, length) = if detached(&buffer) {
        (0, 0)
    } else if kind == 1 {
        let view = JsTypedArray::from_object(object)?;
        (view.byte_offset(context)?, view.byte_length(context)?)
    } else if kind == 2 {
        let view = JsDataView::from_object(object)?;
        (
            view.byte_offset(context)? as usize,
            view.byte_length(context)? as usize,
        )
    } else if let Ok(buffer) = JsArrayBuffer::from_object(buffer.clone()) {
        (0, buffer.byte_length())
    } else {
        (
            0,
            JsSharedArrayBuffer::from_object(buffer.clone())?.byte_length(),
        )
    };
    let view = BufferView {
        buffer,
        offset,
        length,
    };
    bounded(view.length, MAX_INPUT)?;
    Ok(Some(view))
}
fn copy_bytes(view: Option<BufferView>, context: &mut Context) -> JsResult<Vec<u8>> {
    let Some(view) = view else {
        return Ok(vec![]);
    };
    let end = view
        .offset
        .checked_add(view.length)
        .ok_or_else(|| typ("Invalid BufferSource range"))?;
    if let Ok(buffer) = JsArrayBuffer::from_object(view.buffer.clone()) {
        // Web IDL get-buffer-source-copy returns empty data for detached buffers.
        let Some(bytes) = buffer.data() else {
            return Ok(vec![]);
        };
        return bytes
            .get(view.offset..end)
            .map(<[u8]>::to_vec)
            .ok_or_else(|| typ("Out-of-bounds input buffer"));
    }
    let buffer = JsSharedArrayBuffer::from_object(view.buffer)?;
    if end > buffer.byte_length() {
        return Err(typ("Out-of-bounds shared buffer"));
    }
    let bytes = JsUint8Array::from_shared_array_buffer(buffer, context)?;
    (view.offset..end)
        .map(|index| {
            bytes
                .get(index, context)?
                .as_number()
                .map(|value| value as u8)
                .ok_or_else(|| typ("Invalid shared buffer byte"))
        })
        .collect()
}

struct DecoderState {
    encoding: &'static Encoding,
    fatal: bool,
    ignore_bom: bool,
    decoder: Decoder,
    streaming: bool,
    pending: Vec<u8>,
}
impl DecoderState {
    fn decoder(encoding: &'static Encoding, ignore_bom: bool) -> Decoder {
        if ignore_bom {
            encoding.new_decoder_without_bom_handling()
        } else {
            encoding.new_decoder_with_bom_removal()
        }
    }
}
#[derive(Trace, Finalize, JsData)]
struct TextDecoder {
    // Only Rust codec/string/byte state. No hidden Boa GC handles.
    #[unsafe_ignore_trace]
    state: Rc<RefCell<DecoderState>>,
}
fn decoder_state(this: &JsValue) -> JsResult<Rc<RefCell<DecoderState>>> {
    this.as_object()
        .and_then(|object| {
            object
                .downcast_ref::<TextDecoder>()
                .map(|data| data.state.clone())
        })
        .ok_or_else(|| typ("TextDecoder method requires a TextDecoder receiver"))
}
impl Class for TextDecoder {
    const NAME: &'static str = "TextDecoder";
    const ATTRIBUTES: Attribute = Attribute::WRITABLE.union(Attribute::CONFIGURABLE);
    fn data_constructor(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<Self> {
        let label = if args.get_or_undefined(0).is_undefined() {
            "utf-8".into()
        } else {
            text(&args[0], context)?.to_std_string_lossy()
        };
        // Dictionary members are converted before constructor algorithm label
        // validation, in Web IDL's lexicographic member order.
        let fatal = option(args.get_or_undefined(1), "fatal", context)?;
        let ignore_bom = option(args.get_or_undefined(1), "ignoreBOM", context)?;
        let encoding = Encoding::for_label_no_replacement(label.as_bytes())
            .ok_or_else(|| range("Unsupported encoding label"))?;
        Ok(Self {
            state: Rc::new(RefCell::new(DecoderState {
                encoding,
                fatal,
                ignore_bom,
                decoder: DecoderState::decoder(encoding, ignore_bom),
                streaming: false,
                pending: vec![],
            })),
        })
    }
    fn init(class: &mut ClassBuilder) -> JsResult<()> {
        for name in ["encoding", "fatal", "ignoreBOM"] {
            let getter = FunctionObjectBuilder::new(
                class.context().realm(),
                NativeFunction::from_copy_closure(move |this, _, _| {
                    let state = decoder_state(this)?;
                    let state = state.borrow();
                    Ok(match name {
                        "fatal" => state.fatal.into(),
                        "ignoreBOM" => state.ignore_bom.into(),
                        _ => JsString::from(state.encoding.name().to_ascii_lowercase()).into(),
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
            JsString::from("decode"),
            0,
            NativeFunction::from_fn_ptr(decode),
        );
        class.property(
            JsSymbol::to_string_tag(),
            JsString::from("TextDecoder"),
            Attribute::CONFIGURABLE,
        );
        Ok(())
    }
}
fn decode(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let state = decoder_state(this)?;
    // Validate argument types before options getters; copy after conversion so
    // reentrant JS cannot expose a Rust borrow or stale detached backing store.
    buffer_view(args.get_or_undefined(0), context)?;
    let streaming = option(args.get_or_undefined(1), "stream", context)?;
    let view = buffer_view(args.get_or_undefined(0), context)?;
    let input = copy_bytes(view, context)?;
    let mut state = state.borrow_mut();
    if !state.streaming {
        state.decoder = DecoderState::decoder(state.encoding, state.ignore_bom);
        state.pending.clear();
    }
    bounded(state.pending.len() + input.len(), MAX_INPUT)?;
    state.pending.extend(input);
    let capacity = state
        .decoder
        .max_utf8_buffer_length(state.pending.len())
        .ok_or_else(|| range("Decoder output overflow"))?;
    bounded(capacity, MAX_OUTPUT)?;
    let mut output = String::with_capacity(capacity);
    let input = std::mem::take(&mut state.pending);
    state.streaming = streaming;
    if state.fatal {
        let (result, read) =
            state
                .decoder
                .decode_to_string_without_replacement(&input, &mut output, !streaming);
        state.pending.extend_from_slice(&input[read..]);
        match result {
            DecoderResult::Malformed(_, _) => {
                return Err(typ("Encoded input contains invalid data"));
            }
            DecoderResult::OutputFull => {
                return Err(range("Decoder output exceeds the native bound"));
            }
            DecoderResult::InputEmpty => {}
        }
    } else {
        let (result, read, _) = state
            .decoder
            .decode_to_string(&input, &mut output, !streaming);
        state.pending.extend_from_slice(&input[read..]);
        if result == CoderResult::OutputFull {
            return Err(range("Decoder output exceeds the native bound"));
        }
    }
    Ok(JsString::from(output).into())
}

const EXCEPTION_CODES: [(&str, &str, u32); 25] = [
    ("IndexSizeError", "INDEX_SIZE_ERR", 1),
    ("DOMStringSizeError", "DOMSTRING_SIZE_ERR", 2),
    ("HierarchyRequestError", "HIERARCHY_REQUEST_ERR", 3),
    ("WrongDocumentError", "WRONG_DOCUMENT_ERR", 4),
    ("InvalidCharacterError", "INVALID_CHARACTER_ERR", 5),
    ("NoDataAllowedError", "NO_DATA_ALLOWED_ERR", 6),
    (
        "NoModificationAllowedError",
        "NO_MODIFICATION_ALLOWED_ERR",
        7,
    ),
    ("NotFoundError", "NOT_FOUND_ERR", 8),
    ("NotSupportedError", "NOT_SUPPORTED_ERR", 9),
    ("InUseAttributeError", "INUSE_ATTRIBUTE_ERR", 10),
    ("InvalidStateError", "INVALID_STATE_ERR", 11),
    ("SyntaxError", "SYNTAX_ERR", 12),
    ("InvalidModificationError", "INVALID_MODIFICATION_ERR", 13),
    ("NamespaceError", "NAMESPACE_ERR", 14),
    ("InvalidAccessError", "INVALID_ACCESS_ERR", 15),
    ("ValidationError", "VALIDATION_ERR", 16),
    ("TypeMismatchError", "TYPE_MISMATCH_ERR", 17),
    ("SecurityError", "SECURITY_ERR", 18),
    ("NetworkError", "NETWORK_ERR", 19),
    ("AbortError", "ABORT_ERR", 20),
    ("URLMismatchError", "URL_MISMATCH_ERR", 21),
    ("QuotaExceededError", "QUOTA_EXCEEDED_ERR", 22),
    ("TimeoutError", "TIMEOUT_ERR", 23),
    ("InvalidNodeTypeError", "INVALID_NODE_TYPE_ERR", 24),
    ("DataCloneError", "DATA_CLONE_ERR", 25),
];
#[derive(Trace, Finalize, JsData)]
struct DomException {
    name: JsString,
    message: JsString,
}
impl Class for DomException {
    const NAME: &'static str = "DOMException";
    const ATTRIBUTES: Attribute = Attribute::WRITABLE.union(Attribute::CONFIGURABLE);
    fn data_constructor(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<Self> {
        let message = if args.get_or_undefined(0).is_undefined() {
            JsString::from("")
        } else {
            text(&args[0], context)?
        };
        let name = if args.get_or_undefined(1).is_undefined() {
            JsString::from("Error")
        } else {
            text(&args[1], context)?
        };
        Ok(Self { name, message })
    }
    fn init(class: &mut ClassBuilder) -> JsResult<()> {
        for name in ["name", "message", "code"] {
            let getter = FunctionObjectBuilder::new(
                class.context().realm(),
                NativeFunction::from_copy_closure(move |this, _, _| {
                    let object = this
                        .as_object()
                        .ok_or_else(|| typ("Invalid DOMException receiver"))?;
                    let data = object
                        .downcast_ref::<DomException>()
                        .ok_or_else(|| typ("Invalid DOMException receiver"))?;
                    Ok(match name {
                        "name" => data.name.clone().into(),
                        "message" => data.message.clone().into(),
                        _ => {
                            let name = data.name.to_std_string_lossy();
                            EXCEPTION_CODES
                                .iter()
                                .find(|(key, _, _)| key == &name)
                                .map_or(0, |(_, _, code)| *code)
                                .into()
                        }
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
        for (_, name, code) in EXCEPTION_CODES {
            class.property(JsString::from(name), code, Attribute::ENUMERABLE);
            class.static_property(JsString::from(name), code, Attribute::ENUMERABLE);
        }
        class.property(
            JsSymbol::to_string_tag(),
            JsString::from("DOMException"),
            Attribute::CONFIGURABLE,
        );
        Ok(())
    }
}
/// Construct a branded DOMException using registered native class data, never a
/// mutable global constructor. Available after this capability is registered.
pub fn dom_exception(message: &str, name: &str, context: &mut Context) -> JsResult<JsObject> {
    bounded(message.len(), MAX_INPUT)?;
    bounded(name.len(), MAX_INPUT)?;
    DomException::from_data(
        DomException {
            message: JsString::from(message),
            name: JsString::from(name),
        },
        context,
    )
}
fn invalid_character(context: &mut Context) -> JsError {
    match DomException::from_data(
        DomException {
            name: JsString::from("InvalidCharacterError"),
            message: JsString::from("The string is not correctly encoded"),
        },
        context,
    ) {
        Ok(object) => JsError::from_opaque(object.into()),
        Err(error) => error,
    }
}
fn btoa(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    required(args, 1)?;
    let input = text(&args[0], context)?;
    let mut bytes = Vec::with_capacity(input.len());
    for unit in input.iter() {
        if unit > 255 {
            return Err(invalid_character(context));
        }
        bytes.push(unit as u8);
    }
    let result = base64::engine::general_purpose::STANDARD.encode(bytes);
    Ok(JsString::from(result).into())
}
fn atob(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    required(args, 1)?;
    let input = text(&args[0], context)?;
    let mut bytes = Vec::with_capacity(input.len());
    for unit in input.iter() {
        if matches!(unit, 9 | 10 | 12 | 13 | 32) {
            continue;
        }
        if unit > 127 {
            return Err(invalid_character(context));
        }
        bytes.push(unit as u8);
    }
    // Infra forgiving-base64 permits omitted padding and nonzero unused bits,
    // but padding, when present, must be exactly at a multiple-of-four tail.
    if bytes.len() % 4 == 0 {
        if bytes.last() == Some(&b'=') {
            bytes.pop();
        }
        if bytes.last() == Some(&b'=') {
            bytes.pop();
        }
    }
    if bytes.len() % 4 == 1
        || bytes
            .iter()
            .any(|b| !b.is_ascii_alphanumeric() && !matches!(b, b'+' | b'/'))
    {
        return Err(invalid_character(context));
    }
    let engine = GeneralPurpose::new(
        &alphabet::STANDARD,
        GeneralPurposeConfig::new()
            .with_decode_padding_mode(DecodePaddingMode::Indifferent)
            .with_decode_allow_trailing_bits(true),
    );
    let bytes = engine
        .decode(bytes)
        .map_err(|_| invalid_character(context))?;
    let units = bytes.into_iter().map(u16::from).collect::<Vec<_>>();
    Ok(JsString::from(units.as_slice()).into())
}

/// Explicit registration; existing globals are preserved rather than replaced.
/// DOMException is included because atob/btoa throw its InvalidCharacterError.
pub fn register(context: &mut Context) -> JsResult<()> {
    for name in ["TextEncoder", "TextDecoder", "atob", "btoa", "DOMException"] {
        if context
            .global_object()
            .has_property(JsString::from(name), context)?
        {
            return Err(typ("Text host would overwrite an existing global"));
        }
    }
    // Capture trusted intrinsic accessors before application code can replace them.
    let resizable = intrinsic_getter(
        context
            .intrinsics()
            .constructors()
            .array_buffer()
            .prototype(),
        "resizable",
        context,
    )?;
    let growable = intrinsic_getter(
        context
            .intrinsics()
            .constructors()
            .shared_array_buffer()
            .prototype(),
        "growable",
        context,
    )?;
    context.insert_data(BufferIntrinsics {
        resizable,
        growable,
    });
    context.register_global_class::<TextEncoder>()?;
    context.register_global_class::<TextDecoder>()?;
    context.register_global_class::<DomException>()?;
    context
        .get_global_class::<DomException>()
        .expect("registered")
        .prototype()
        .set_prototype(Some(
            context.intrinsics().constructors().error().prototype(),
        ));
    for (prototype, names) in [
        (
            context
                .get_global_class::<TextEncoder>()
                .expect("registered")
                .prototype(),
            &["encode", "encodeInto"][..],
        ),
        (
            context
                .get_global_class::<TextDecoder>()
                .expect("registered")
                .prototype(),
            &["decode"][..],
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
    context.register_global_builtin_callable(
        JsString::from("atob"),
        1,
        NativeFunction::from_fn_ptr(atob),
    )?;
    context.register_global_builtin_callable(
        JsString::from("btoa"),
        1,
        NativeFunction::from_fn_ptr(btoa),
    )?;
    Ok(())
}

#[cfg(test)]
#[path = "web_text_tests.rs"]
mod tests;
