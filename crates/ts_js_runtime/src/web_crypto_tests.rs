use super::*;
use boa_engine::{Source, builtins::promise::PromiseState};
use std::sync::atomic::{AtomicUsize, Ordering};
fn context(provider: Option<Arc<dyn EntropySource>>, options: CryptoOptions) -> Context {
    let mut context = Context::builder()
        .instructions_remaining(20_000_000)
        .build()
        .unwrap();
    crate::web_text::register(&mut context).unwrap();
    register(&mut context, &options, provider).unwrap();
    context
        .register_global_builtin_callable(
            JsString::from("detachBuffer"),
            1,
            NativeFunction::from_fn_ptr(|_, args, _| {
                let buffer = JsArrayBuffer::from_object(
                    args.first()
                        .and_then(JsValue::as_object)
                        .ok_or_else(|| typ("Buffer required"))?,
                )?;
                buffer.detach(&JsValue::undefined())?;
                Ok(JsValue::undefined())
            }),
        )
        .unwrap();
    context
}
fn fixture() -> Context {
    // Deliberately deterministic provider for byte-boundary/UUID tests ONLY.
    context(
        Some(Arc::new(|bytes: &mut [u8]| {
            for (i, byte) in bytes.iter_mut().enumerate() {
                *byte = i as u8;
            }
            Ok(())
        })),
        CryptoOptions::default(),
    )
}
fn run(context: &mut Context, body: &str) {
    let source = format!(
        r#"(async()=>{{
    const stringify=JSON.stringify;
    function eq(a,b){{if(stringify(a)!==stringify(b))throw Error(stringify({{actual:a,expected:b}}));}}
    function assert(v){{if(!v)throw Error('Assertion failed');}}
    function throws(f,name='TypeError'){{try{{f()}}catch(e){{eq(e.name,name);return e}}throw Error('Expected exception');}}
    async function rejects(p,name='TypeError'){{assert(p instanceof Promise);try{{await p}}catch(e){{eq(e.name,name);return e}}throw Error('Expected rejection');}}
    const hex=b=>Array.from(new Uint8Array(b),x=>x.toString(16).padStart(2,'0')).join('');
    {body}
    return true;
    }})()"#
    );
    let value = context.eval(Source::from_bytes(&source)).unwrap();
    let promise = JsPromise::from_object(value.as_object().unwrap()).unwrap();
    context.run_jobs().unwrap();
    match promise.state() {
        PromiseState::Fulfilled(v) => assert_eq!(v.as_boolean(), Some(true)),
        PromiseState::Rejected(v) => panic!(
            "Crypto fixture rejected: {}",
            v.to_string(context).unwrap().to_std_string_escaped()
        ),
        PromiseState::Pending => panic!("Crypto fixture remained pending"),
    }
}
#[test]
fn integer_array_kinds_exact_view_bytes_quota_and_identity() {
    run(
        &mut fixture(),
        r#"
    for(const C of [Int8Array,Uint8Array,Uint8ClampedArray,Int16Array,Uint16Array,Int32Array,Uint32Array,BigInt64Array,BigUint64Array]){
        const buffer=new ArrayBuffer(32), all=new Uint8Array(buffer);all.fill(255);
        const view=new C(buffer,8,8/C.BYTES_PER_ELEMENT);
        Object.defineProperty(view,'byteLength',{get(){throw Error('public byteLength used')}});
        Object.defineProperty(view,'byteOffset',{get(){throw Error('public byteOffset used')}});
        Object.defineProperty(view,'buffer',{get(){throw Error('public buffer used')}});
        assert(crypto.getRandomValues(view)===view);
        eq(Array.from(all.slice(8,16)),[0,1,2,3,4,5,6,7]);
        assert(all.slice(0,8).every(x=>x===255)&&all.slice(16).every(x=>x===255));
    }
    const maximum=new Uint32Array(16384);assert(crypto.getRandomValues(maximum)===maximum);
    const error=throws(()=>crypto.getRandomValues(new Uint16Array(32769)),'QuotaExceededError');
    assert(error instanceof DOMException);eq(error.code,22);
    assert(crypto.getRandomValues(new Uint8Array(0)).length===0);
    "#,
    );
}
#[test]
fn conversion_type_mismatch_shared_resizable_detached_and_branding() {
    run(
        &mut fixture(),
        r#"
    for(const x of [new Float32Array(1),new Float64Array(1),new DataView(new ArrayBuffer(1))]){
        const e=throws(()=>crypto.getRandomValues(x),'TypeMismatchError');assert(e instanceof DOMException);eq(e.code,17);
    }
    for(const x of [null,undefined,{},[],new ArrayBuffer(4),new Proxy(new Uint8Array(4),{})])throws(()=>crypto.getRandomValues(x));
    throws(()=>crypto.getRandomValues());throws(()=>Crypto());throws(()=>new Crypto());throws(()=>new SubtleCrypto());
    throws(()=>Crypto.prototype.getRandomValues.call({},new Uint8Array(1)));
    throws(()=>Crypto.prototype.randomUUID.call({}));
    throws(()=>crypto.getRandomValues(new Uint8Array(new SharedArrayBuffer(8))));
    throws(()=>crypto.getRandomValues(new Uint8Array(new ArrayBuffer(8,{maxByteLength:16}))));
    const detached=new Uint8Array([1,2]);detachBuffer(detached.buffer);assert(crypto.getRandomValues(detached)===detached);
    assert(crypto===crypto&&crypto.subtle===crypto.subtle);
    eq(Object.prototype.toString.call(crypto),'[object Crypto]');
    eq(Object.prototype.toString.call(crypto.subtle),'[object SubtleCrypto]');
    eq([crypto.getRandomValues.length,crypto.randomUUID.length,crypto.subtle.digest.length],[1,0,2]);
    assert(Object.getOwnPropertyDescriptor(Crypto.prototype,'getRandomValues').enumerable);
    "#,
    );
}
#[test]
fn uuid_uses_entropy_bytes_with_version_and_variant_bits() {
    run(
        &mut fixture(),
        r#"eq(crypto.randomUUID(),'00010203-0405-4607-8809-0a0b0c0d0e0f');"#,
    );
}
#[test]
fn provider_failure_is_atomic_and_missing_entropy_never_falls_back() {
    let provider: Arc<dyn EntropySource> = Arc::new(|bytes: &mut [u8]| {
        bytes.fill(123);
        Err("fixture unavailable".into())
    });
    let mut c = context(Some(provider), CryptoOptions::default());
    run(
        &mut c,
        r#"const a=new Uint8Array([7,8]);const e=throws(()=>crypto.getRandomValues(a),'OperationError');assert(e instanceof DOMException);eq(Array.from(a),[7,8]);throws(()=>crypto.randomUUID(),'OperationError');"#,
    );
    let mut c = context(None, CryptoOptions::default());
    run(
        &mut c,
        r#"throws(()=>crypto.getRandomValues(new Uint8Array(1)),'OperationError');throws(()=>crypto.randomUUID(),'OperationError');assert((await crypto.subtle.digest('SHA-256',new Uint8Array(0))).byteLength===32);"#,
    );
}
#[test]
fn invalid_random_requests_do_not_consume_entropy() {
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    let mut c = context(
        Some(Arc::new(move |bytes: &mut [u8]| {
            observed.fetch_add(1, Ordering::SeqCst);
            bytes.fill(1);
            Ok(())
        })),
        CryptoOptions::default(),
    );
    run(
        &mut c,
        r#"throws(()=>crypto.getRandomValues(new Uint8Array(65537)),'QuotaExceededError');throws(()=>crypto.getRandomValues(new Float32Array(1)),'TypeMismatchError');throws(()=>crypto.getRandomValues({}));"#,
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}
#[test]
fn native_sha_known_answers_and_buffer_views() {
    run(
        &mut fixture(),
        r#"
    const data=new Uint8Array([0,97,98,99,0]),input=new DataView(data.buffer,1,3);
    const answers={
        'SHA-1':'a9993e364706816aba3e25717850c26c9cd0d89d',
        'SHA-256':'ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad',
        'SHA-384':'cb00753f45a35e8bb5a03d699ac65007272c32ab0eded1631a8b605a43ff5bed8086072ba1e7cc2358baeca134c825a7',
        'SHA-512':'ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f'
    };
    for(const [name,expected] of Object.entries(answers)){
        const result=await crypto.subtle.digest({name:name.toLowerCase()},input);
        assert(result instanceof ArrayBuffer);eq(hex(result),expected);
    }
    eq(hex(await crypto.subtle.digest('SHA-256',data.subarray(1,4))),answers['SHA-256']);
    eq(hex(await crypto.subtle.digest('SHA-256',data.buffer.slice(1,4))),answers['SHA-256']);
    "#,
    );
}
#[test]
fn normalization_getters_precede_copy_and_later_mutation_cannot_change_digest() {
    run(
        &mut fixture(),
        r#"
    let gets=0;const data=new Uint8Array([97]);
    const algorithm={get name(){gets++;data[0]=98;return gets===1?'sHa-256':'ignored-after-registration'}};
    const promise=crypto.subtle.digest(algorithm,data);eq(gets,2);data[0]=99;
    eq(hex(await promise),'3e23e8160039594a33894f6564e1b1348bbd7a0088d42c4acb73eeaed59c009d');
    const detached=new Uint8Array([97]);let once=false;
    const p=crypto.subtle.digest({get name(){if(!once){once=true;detachBuffer(detached.buffer)}return 'SHA-256'}},detached);
    eq(hex(await p),'e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855');
    const previous=new ArrayBuffer(2);detachBuffer(previous);
    eq(hex(await crypto.subtle.digest('SHA-256',previous)),'e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855');
    "#,
    );
}
#[test]
fn digest_rejections_follow_webidl_and_preserve_thrown_values() {
    run(
        &mut fixture(),
        r#"
    await rejects(crypto.subtle.digest('SHA256',new Uint8Array(1)),'NotSupportedError');
    await rejects(crypto.subtle.digest(' SHA-256',new Uint8Array(1)),'NotSupportedError');
    await rejects(crypto.subtle.digest({},new Uint8Array(1)));
    await rejects(crypto.subtle.digest(Symbol(),new Uint8Array(1)));
    await rejects(crypto.subtle.digest('SHA-256','abc'));
    await rejects(crypto.subtle.digest('SHA-256',new SharedArrayBuffer(4)));
    await rejects(crypto.subtle.digest('SHA-256',new Uint8Array(new SharedArrayBuffer(4))));
    await rejects(crypto.subtle.digest('SHA-256',new ArrayBuffer(4,{maxByteLength:8})));
    await rejects(crypto.subtle.digest());
    await rejects(SubtleCrypto.prototype.digest.call({},'SHA-256',new Uint8Array()));
    let gets=0;await rejects(crypto.subtle.digest({get name(){gets++;return 'SHA-256'}},{}));eq(gets,0);
    const reason={sentinel:1};let caught=false;try{await crypto.subtle.digest({get name(){throw reason}},new Uint8Array())}catch(e){assert(e===reason);caught=true}assert(caught);
    let n=0;await rejects(crypto.subtle.digest({get name(){return ++n===1?'SHA-256':undefined}},new Uint8Array()));eq(n,2);
    "#,
    );
}
#[test]
fn digest_remains_pending_until_native_job_and_limits_release_after_completion() {
    let options = CryptoOptions {
        max_digest_bytes: 4,
        max_pending_digest_bytes: 4,
        max_pending_digests: 1,
        ..CryptoOptions::default()
    };
    let mut c = context(None, options);
    let value = c
        .eval(Source::from_bytes(
            "crypto.subtle.digest('SHA-256',new Uint8Array([97]))",
        ))
        .unwrap();
    let promise = JsPromise::from_object(value.as_object().unwrap()).unwrap();
    assert!(matches!(promise.state(), PromiseState::Pending));
    c.run_jobs().unwrap();
    assert!(matches!(promise.state(), PromiseState::Fulfilled(_)));
    run(
        &mut c,
        r#"const a=crypto.subtle.digest('SHA-256',new Uint8Array(4));const b=crypto.subtle.digest('SHA-256',new Uint8Array(1));await rejects(b,'OperationError');await a;assert((await crypto.subtle.digest('SHA-256',new Uint8Array(4))).byteLength===32);await rejects(crypto.subtle.digest('SHA-256',new Uint8Array(5)),'OperationError');"#,
    );
}
#[test]
fn insecure_host_has_no_secure_crypto_surface_and_registration_is_explicit() {
    let options = CryptoOptions {
        secure_context: false,
        ..CryptoOptions::default()
    };
    let mut c = context(None, options);
    run(
        &mut c,
        r#"eq(typeof crypto.subtle,'undefined');eq(typeof crypto.randomUUID,'undefined');eq(typeof SubtleCrypto,'undefined');assert(typeof crypto.getRandomValues==='function');"#,
    );
    assert!(register(&mut c, &CryptoOptions::default(), None).is_err());
}
