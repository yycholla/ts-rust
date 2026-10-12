//! Explicit native WebCrypto capability for SSR CSP: random values, UUID v4,
//! and SHA digests. No key operations, filesystem access, ambient entropy,
//! JavaScript crypto implementation, threads, or timers.
//!
//! Algorithms: W3C WebCrypto Level 2 §§10,14.3.5,18.4.4 (2026-08-11 draft),
//! https://w3c.github.io/webcrypto/ and WebIDL §§3.2.26,3.3.1–2.
//! Register after web_text (branded DOMException). Digest input conversion and
//! algorithm normalization happen synchronously, then bytes are copied BEFORE
//! returning the pending promise. Hashing/settlement runs as a bounded generic
//! realm job, drained by the host's existing executor, not an OS crypto thread.
use boa_engine::{
    Context, JsData, JsError, JsNativeError, JsObject, JsResult, JsString, JsSymbol, JsValue,
    NativeFunction,
    builtins::{object::OrdinaryObject, typed_array::TypedArrayKind},
    class::{Class, ClassBuilder},
    job::GenericJob,
    object::{
        FunctionObjectBuilder,
        builtins::{JsArrayBuffer, JsDataView, JsPromise, JsTypedArray},
    },
    property::{Attribute, PropertyDescriptor},
};
use boa_gc::{Finalize, Trace};
use sha2::{Digest, Sha256, Sha384, Sha512};
use std::{cell::Cell, rc::Rc, sync::Arc};

/// Implementations must completely fill `bytes` with cryptographically secure
/// entropy or return an error. Native-only Send+Sync state excludes Boa GC
/// handles; callbacks must not execute JS or reenter this context. Deterministic
/// providers belong only in tests, never production registrations.
pub trait EntropySource: Send + Sync + 'static {
    fn fill(&self, bytes: &mut [u8]) -> Result<(), String>;
}
impl<F> EntropySource for F
where
    F: Fn(&mut [u8]) -> Result<(), String> + Send + Sync + 'static,
{
    fn fill(&self, bytes: &mut [u8]) -> Result<(), String> {
        self(bytes)
    }
}
#[derive(Debug, Clone)]
pub struct CryptoOptions {
    pub secure_context: bool,
    pub max_digest_bytes: usize,
    pub max_pending_digest_bytes: usize,
    pub max_pending_digests: usize,
}
impl Default for CryptoOptions {
    fn default() -> Self {
        Self {
            secure_context: true,
            max_digest_bytes: 16 * 1024 * 1024,
            max_pending_digest_bytes: 64 * 1024 * 1024,
            max_pending_digests: 256,
        }
    }
}
#[derive(Default)]
struct Budget {
    bytes: Cell<usize>,
    jobs: Cell<usize>,
}
struct Registration {
    options: CryptoOptions,
    entropy: Option<Arc<dyn EntropySource>>,
    resizable: JsObject,
    budget: Rc<Budget>,
}
#[derive(Trace, Finalize, JsData)]
struct Crypto {
    subtle: Option<JsObject>,
}
#[derive(Trace, Finalize, JsData)]
struct SubtleCrypto;
const WEB_ATTRS: Attribute = Attribute::ENUMERABLE.union(Attribute::CONFIGURABLE);
fn typ(message: &str) -> JsError {
    JsNativeError::typ().with_message(message.to_owned()).into()
}
fn dom(name: &str, message: &str, context: &mut Context) -> JsError {
    match crate::web_text::dom_exception(message, name, context) {
        Ok(error) => JsError::from_opaque(error.into()),
        Err(error) => error,
    }
}
fn required(args: &[JsValue], n: usize) -> JsResult<()> {
    if args.len() < n {
        Err(typ("Insufficient WebCrypto arguments"))
    } else {
        Ok(())
    }
}
fn crypto(this: &JsValue) -> JsResult<JsObject> {
    this.as_object()
        .filter(|object| object.is::<Crypto>())
        .ok_or_else(|| typ("Crypto receiver required"))
}
impl Class for Crypto {
    const NAME: &'static str = "Crypto";
    const ATTRIBUTES: Attribute = Attribute::WRITABLE.union(Attribute::CONFIGURABLE);
    fn data_constructor(_: &JsValue, _: &[JsValue], _: &mut Context) -> JsResult<Self> {
        Err(typ("Illegal Crypto constructor"))
    }
    fn init(class: &mut ClassBuilder) -> JsResult<()> {
        class.method(
            JsString::from("getRandomValues"),
            1,
            NativeFunction::from_fn_ptr(random_values),
        );
        if class
            .context()
            .get_data::<Registration>()
            .unwrap()
            .options
            .secure_context
        {
            class.method(
                JsString::from("randomUUID"),
                0,
                NativeFunction::from_fn_ptr(random_uuid),
            );
            let getter = FunctionObjectBuilder::new(
                class.context().realm(),
                NativeFunction::from_fn_ptr(|this, _, _| {
                    let object = crypto(this)?;
                    let data = object.downcast_ref::<Crypto>().unwrap();
                    Ok(data
                        .subtle
                        .clone()
                        .map_or(JsValue::undefined(), JsValue::from))
                }),
            )
            .name(JsString::from("get subtle"))
            .build();
            class.accessor(JsString::from("subtle"), Some(getter), None, WEB_ATTRS);
        }
        class.property(
            JsSymbol::to_string_tag(),
            JsString::from("Crypto"),
            Attribute::CONFIGURABLE,
        );
        Ok(())
    }
}
impl Class for SubtleCrypto {
    const NAME: &'static str = "SubtleCrypto";
    const ATTRIBUTES: Attribute = Attribute::WRITABLE.union(Attribute::CONFIGURABLE);
    fn data_constructor(_: &JsValue, _: &[JsValue], _: &mut Context) -> JsResult<Self> {
        Err(typ("Illegal SubtleCrypto constructor"))
    }
    fn init(class: &mut ClassBuilder) -> JsResult<()> {
        class.method(
            JsString::from("digest"),
            2,
            NativeFunction::from_fn_ptr(digest),
        );
        class.property(
            JsSymbol::to_string_tag(),
            JsString::from("SubtleCrypto"),
            Attribute::CONFIGURABLE,
        );
        Ok(())
    }
}
struct BufferSource {
    buffer: JsArrayBuffer,
    offset: usize,
    length: usize,
    integer: bool,
}
/// Conversion uses internal slots, not overridable .buffer/.byteLength getters.
/// WebCrypto lacks AllowShared and AllowResizable, so both are TypeErrors.
fn buffer_source(
    value: &JsValue,
    views_only: bool,
    context: &mut Context,
) -> JsResult<BufferSource> {
    let object = value
        .as_object()
        .ok_or_else(|| typ("WebCrypto requires a BufferSource"))?;
    let array = JsTypedArray::from_object(object.clone()).ok();
    let view = JsDataView::from_object(object.clone()).ok();
    let (buffer_object, integer) = if let Some(array) = &array {
        let integer = matches!(
            array.kind(),
            Some(
                TypedArrayKind::Int8
                    | TypedArrayKind::Uint8
                    | TypedArrayKind::Uint8Clamped
                    | TypedArrayKind::Int16
                    | TypedArrayKind::Uint16
                    | TypedArrayKind::Int32
                    | TypedArrayKind::Uint32
                    | TypedArrayKind::BigInt64
                    | TypedArrayKind::BigUint64
            )
        );
        (
            array
                .buffer(context)?
                .as_object()
                .ok_or_else(|| typ("Invalid typed-array buffer"))?,
            integer,
        )
    } else if let Some(view) = &view {
        (
            view.buffer(context)?
                .as_object()
                .ok_or_else(|| typ("Invalid DataView buffer"))?,
            false,
        )
    } else if !views_only {
        (object.clone(), false)
    } else {
        return Err(typ("getRandomValues requires an ArrayBufferView"));
    };
    let buffer = JsArrayBuffer::from_object(buffer_object.clone())
        .map_err(|_| typ("WebCrypto BufferSource must not be shared"))?;
    let getter = context
        .get_data::<Registration>()
        .ok_or_else(|| typ("WebCrypto is not registered"))?
        .resizable
        .clone();
    if getter
        .call(&buffer_object.into(), &[], context)?
        .to_boolean()
    {
        return Err(typ("WebCrypto BufferSource must not be resizable"));
    }
    // WebIDL copying a detached buffer returns the empty byte sequence. Do not
    // invoke DataView byteLength on detached storage (that getter throws).
    let (offset, length) = if buffer.data().is_none() {
        (0, 0)
    } else if let Some(array) = array {
        (array.byte_offset(context)?, array.byte_length(context)?)
    } else if let Some(view) = view {
        (
            view.byte_offset(context)? as usize,
            view.byte_length(context)? as usize,
        )
    } else {
        (0, buffer.byte_length())
    };
    if offset
        .checked_add(length)
        .is_none_or(|end| end > buffer.byte_length())
    {
        return Err(typ("WebCrypto view is out of bounds"));
    }
    Ok(BufferSource {
        buffer,
        offset,
        length,
        integer,
    })
}
fn entropy(bytes: &mut [u8], context: &mut Context) -> JsResult<()> {
    if bytes.is_empty() {
        return Ok(());
    }
    let provider = context
        .get_data::<Registration>()
        .and_then(|s| s.entropy.clone())
        .ok_or_else(|| {
            dom(
                "OperationError",
                "No native cryptographic entropy provider was installed",
                context,
            )
        })?;
    provider.fill(bytes).map_err(|_| {
        dom(
            "OperationError",
            "Native cryptographic entropy provider failed",
            context,
        )
    })
}
fn random_values(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    crypto(this)?;
    required(args, 1)?;
    let view = buffer_source(&args[0], true, context)?;
    if !view.integer {
        return Err(dom(
            "TypeMismatchError",
            "getRandomValues requires an integer typed array",
            context,
        ));
    }
    if view.length > 65_536 {
        return Err(dom(
            "QuotaExceededError",
            "getRandomValues exceeds the 65536-byte quota",
            context,
        ));
    }
    let mut bytes = vec![0; view.length];
    entropy(&mut bytes, context)?;
    if view.length > 0 {
        let mut data = view
            .buffer
            .data_mut()
            .ok_or_else(|| typ("Random target buffer was detached"))?;
        let destination = data
            .get_mut(view.offset..view.offset + view.length)
            .ok_or_else(|| typ("Random target view is out of bounds"))?;
        destination.copy_from_slice(&bytes);
    }
    Ok(args[0].clone())
}
fn random_uuid(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    crypto(this)?;
    let mut bytes = [0u8; 16];
    entropy(&mut bytes, context)?;
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let mut result = String::with_capacity(36);
    use std::fmt::Write;
    for (i, byte) in bytes.iter().enumerate() {
        if matches!(i, 4 | 6 | 8 | 10) {
            result.push('-');
        }
        write!(&mut result, "{byte:02x}").expect("writing into String");
    }
    Ok(JsString::from(result).into())
}
#[derive(Clone, Copy)]
enum Algorithm {
    Sha1,
    Sha256,
    Sha384,
    Sha512,
}
fn algorithm_name(value: &JsValue, context: &mut Context) -> JsResult<JsString> {
    let value = if let Some(object) = value.as_object() {
        let name = object.get(JsString::from("name"), context)?;
        if name.is_undefined() {
            return Err(typ("Algorithm dictionary requires name"));
        }
        name
    } else {
        value.clone()
    };
    value.to_string(context)
}
fn normalize(value: &JsValue, context: &mut Context) -> JsResult<Algorithm> {
    let name = algorithm_name(value, context)?;
    if name.len() > 7 {
        return Err(dom(
            "NotSupportedError",
            "Digest algorithm is not supported",
            context,
        ));
    }
    let name = name.to_std_string_escaped();
    let algorithm = match name.to_ascii_uppercase().as_str() {
        "SHA-1" => Algorithm::Sha1,
        "SHA-256" => Algorithm::Sha256,
        "SHA-384" => Algorithm::Sha384,
        "SHA-512" => Algorithm::Sha512,
        _ => {
            return Err(dom(
                "NotSupportedError",
                "Digest algorithm is not supported",
                context,
            ));
        }
    };
    // Normalization converts the object to Algorithm, resolves its registered
    // type, then converts it again. The second conversion's getter is observable;
    // its name is overwritten by the canonical name selected above (§18.4.4).
    if value.is_object() {
        algorithm_name(value, context)?;
    }
    Ok(algorithm)
}
struct PendingDigest {
    bytes: Vec<u8>,
    budget: Rc<Budget>,
}
impl Drop for PendingDigest {
    fn drop(&mut self) {
        self.budget
            .bytes
            .set(self.budget.bytes.get() - self.bytes.len());
        self.budget.jobs.set(self.budget.jobs.get() - 1);
    }
}
fn digest(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let operation = (|| {
        if !this
            .as_object()
            .is_some_and(|object| object.is::<SubtleCrypto>())
        {
            return Err(typ("SubtleCrypto receiver required"));
        }
        required(args, 2)?;
        // WebIDL AlgorithmIdentifier union conversion selects object or converts
        // primitive to DOMString before converting the data argument.
        let algorithm = if args[0].is_object() {
            args[0].clone()
        } else {
            args[0].to_string(context)?.into()
        };
        let view = buffer_source(&args[1], false, context)?;
        let algorithm = normalize(&algorithm, context)?;
        let state = context.get_data::<Registration>().unwrap();
        let length = if view.buffer.data().is_none() {
            0
        } else {
            view.length
        };
        if length > state.options.max_digest_bytes
            || state.budget.bytes.get().saturating_add(length)
                > state.options.max_pending_digest_bytes
            || state.budget.jobs.get() >= state.options.max_pending_digests
        {
            return Err(dom(
                "OperationError",
                "Native digest resource limit exceeded",
                context,
            ));
        }
        let bytes = if let Some(data) = view.buffer.data() {
            data.get(view.offset..view.offset + view.length)
                .ok_or_else(|| typ("Digest source became out of bounds"))?
                .to_vec()
        } else {
            vec![]
        };
        let budget = state.budget.clone();
        budget.bytes.set(budget.bytes.get() + bytes.len());
        budget.jobs.set(budget.jobs.get() + 1);
        let pending = PendingDigest { bytes, budget };
        let (promise, settle) = JsPromise::new_pending(context);
        let job = GenericJob::new(
            move |context| {
                let bytes = match algorithm {
                    Algorithm::Sha1 => sha1::Sha1::digest(&pending.bytes).to_vec(),
                    Algorithm::Sha256 => Sha256::digest(&pending.bytes).to_vec(),
                    Algorithm::Sha384 => Sha384::digest(&pending.bytes).to_vec(),
                    Algorithm::Sha512 => Sha512::digest(&pending.bytes).to_vec(),
                };
                drop(pending);
                let result = (|| {
                    let buffer = JsArrayBuffer::new(bytes.len(), context)?;
                    buffer
                        .data_mut()
                        .ok_or_else(|| typ("New digest buffer is detached"))?
                        .copy_from_slice(&bytes);
                    Ok::<JsValue, JsError>(buffer.into())
                })();
                match result {
                    Ok(value) => settle
                        .resolve
                        .call(&JsValue::undefined(), &[value], context)?,
                    Err(error) => settle.reject.call(
                        &JsValue::undefined(),
                        &[error.into_opaque(context)?],
                        context,
                    )?,
                };
                Ok(JsValue::undefined())
            },
            context.realm().clone(),
        );
        context.enqueue_job(job.into());
        Ok(promise)
    })();
    match operation {
        Ok(promise) => Ok(promise.into()),
        Err(error) => Ok(JsPromise::reject(error, context)?.into()),
    }
}
/// Register the selected native subset once. `secure_context` is a host fact;
/// an insecure host exposes only getRandomValues. None entropy still permits
/// digest, but nonempty random requests fail. Key methods are intentionally absent.
pub fn register(
    context: &mut Context,
    options: &CryptoOptions,
    entropy: Option<Arc<dyn EntropySource>>,
) -> JsResult<()> {
    if context.get_data::<Registration>().is_some() {
        return Err(typ("WebCrypto already registered"));
    }
    if options.max_digest_bytes == 0
        || options.max_digest_bytes > 64 * 1024 * 1024
        || options.max_pending_digest_bytes < options.max_digest_bytes
        || options.max_pending_digest_bytes > 64 * 1024 * 1024
        || options.max_pending_digests == 0
        || options.max_pending_digests > 4096
    {
        return Err(typ("Invalid WebCrypto resource limits"));
    }
    for name in ["crypto", "Crypto", "SubtleCrypto"] {
        if context
            .global_object()
            .has_property(JsString::from(name), context)?
        {
            return Err(typ("WebCrypto global already exists"));
        }
    }
    // Confirm the actual native DOMException class, not a user replacement.
    crate::web_text::dom_exception("", "OperationError", context)?;
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
        .ok_or_else(|| typ("Missing intrinsic resizable descriptor"))?
        .get(JsString::from("get"), context)?
        .as_callable()
        .ok_or_else(|| typ("Missing intrinsic resizable getter"))?;
    context.insert_data(Registration {
        options: options.clone(),
        entropy,
        resizable,
        budget: Rc::new(Budget::default()),
    });
    context.register_global_class::<Crypto>()?;
    let subtle = if options.secure_context {
        context.register_global_class::<SubtleCrypto>()?;
        Some(SubtleCrypto::from_data(SubtleCrypto, context)?)
    } else {
        None
    };
    let object = Crypto::from_data(Crypto { subtle }, context)?;
    let getter = FunctionObjectBuilder::new(
        context.realm(),
        NativeFunction::from_copy_closure_with_captures(
            |_, _, object, _| Ok(object.clone().into()),
            object,
        ),
    )
    .name(JsString::from("get crypto"))
    .build();
    context.global_object().define_property_or_throw(
        JsString::from("crypto"),
        PropertyDescriptor::builder()
            .get(getter)
            .enumerable(true)
            .configurable(true),
        context,
    )?;
    let prototype = context.get_global_class::<Crypto>().unwrap().prototype();
    let methods: &[&str] = if options.secure_context {
        &["getRandomValues", "randomUUID"]
    } else {
        &["getRandomValues"]
    };
    for name in methods {
        prototype.define_property_or_throw(
            JsString::from(*name),
            PropertyDescriptor::builder().enumerable(true),
            context,
        )?;
    }
    if let Some(class) = context.get_global_class::<SubtleCrypto>() {
        class.prototype().define_property_or_throw(
            JsString::from("digest"),
            PropertyDescriptor::builder().enumerable(true),
            context,
        )?;
    }
    Ok(())
}
#[cfg(test)]
#[path = "web_crypto_tests.rs"]
mod tests;
