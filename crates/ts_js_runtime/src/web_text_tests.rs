use super::*;
use boa_engine::Source;

fn check(body: &str) {
    let mut context = Context::builder()
        .instructions_remaining(10_000_000)
        .build()
        .unwrap();
    register(&mut context).unwrap();
    context
        .register_global_builtin_callable(
            JsString::from("detachBuffer"),
            1,
            NativeFunction::from_fn_ptr(|_, args, _| {
                let buffer = JsArrayBuffer::from_object(
                    args.get_or_undefined(0)
                        .as_object()
                        .ok_or_else(|| typ("Buffer required"))?,
                )?;
                buffer.detach(&JsValue::undefined())?;
                Ok(JsValue::undefined())
            }),
        )
        .unwrap();
    let source = format!(
        r#"(()=>{{
        function eq(a,b) {{if(JSON.stringify(a)!==JSON.stringify(b)) throw new Error(JSON.stringify({{actual:a,expected:b}}));}}
        function throws(fn,kind=TypeError) {{try{{fn()}}catch(e){{if(e instanceof kind)return e;throw e}}throw new Error('Expected exception')}}
        {body}
        return true;
    }})()"#
    );
    assert_eq!(
        context
            .eval(Source::from_bytes(&source))
            .unwrap()
            .as_boolean(),
        Some(true)
    );
}

#[test]
fn text_capability_is_explicit() {
    let mut context = Context::builder()
        .instructions_remaining(10_000_000)
        .build()
        .unwrap();
    assert_eq!(
        context
            .eval(Source::from_bytes("typeof TextEncoder + '/' + typeof atob"))
            .unwrap()
            .as_string()
            .unwrap()
            .to_std_string_lossy(),
        "undefined/undefined"
    );
    context
        .eval(Source::from_bytes("globalThis.atob=123"))
        .unwrap();
    assert!(register(&mut context).is_err());
    assert_eq!(
        context
            .eval(Source::from_bytes("atob"))
            .unwrap()
            .as_number(),
        Some(123.0)
    );
    assert_eq!(
        context
            .eval(Source::from_bytes("typeof TextEncoder"))
            .unwrap()
            .as_string()
            .unwrap()
            .to_std_string_lossy(),
        "undefined"
    );
}

#[test]
fn utf8_encoder_usv_strings_and_receiver_checks() {
    check(
        r#"
        const encoder=new TextEncoder('ignored');
        eq(encoder.encoding,'utf-8'); eq([...encoder.encode()],[]); eq([...encoder.encode(undefined)],[]);
        eq([...encoder.encode(null)],[110,117,108,108]);
        eq([...encoder.encode('A😀\uD800')],[65,240,159,152,128,239,191,189]);
        eq(encoder.encode('x') instanceof Uint8Array,true);
        eq(Object.prototype.toString.call(encoder),'[object TextEncoder]');
        throws(()=>TextEncoder()); throws(()=>TextEncoder.prototype.encode.call({})); throws(()=>encoder.encode(Symbol()));
        class Custom extends TextEncoder{}; eq(new Custom().encode('x')[0],120);
    "#,
    );
}

#[test]
fn encode_into_counts_utf16_and_never_splits_characters() {
    check(
        r#"
        const encoder=new TextEncoder(); const whole=new Uint8Array(9).fill(99);const view=whole.subarray(2,7);
        eq(encoder.encodeInto('A😀Z',view),{read:3,written:5});
        eq([...whole],[99,99,65,240,159,152,128,99,99]);
        const small=new Uint8Array(3).fill(7);
        eq(encoder.encodeInto('😀',small),{read:0,written:0});eq([...small],[7,7,7]);
        eq(encoder.encodeInto('\uD800',small),{read:1,written:3});eq([...small],[239,191,189]);
        eq(encoder.encodeInto('abc',new Uint8Array()),{read:0,written:0});
        throws(()=>encoder.encodeInto('a'));throws(()=>encoder.encodeInto('a',new Uint8ClampedArray(4)));
        throws(()=>encoder.encodeInto('a',new DataView(new ArrayBuffer(4))));
        throws(()=>encoder.encodeInto('a',new Proxy(new Uint8Array(4),{})));
    "#,
    );
}

#[test]
fn decoder_accepts_real_buffer_sources_and_view_offsets() {
    check(
        r#"
        const decoder=new TextDecoder(); const bytes=new Uint8Array([0,65,226,130,172,0]);
        eq(decoder.decode(bytes.subarray(1,5)),'A€');
        eq(decoder.decode(new DataView(bytes.buffer,1,4)),'A€');
        eq(decoder.decode(bytes.buffer),'\0A€\0');
        eq(decoder.decode(),'');eq(decoder.decode(undefined),'');
        eq(decoder.decode(new Uint16Array([0x4141])),'AA');
        Object.defineProperty(bytes,'buffer',{get(){throw new Error('shadowed')}});
        Object.defineProperty(bytes,'byteLength',{get(){throw new Error('shadowed')}});
        eq(decoder.decode(bytes),'\0A€\0');
        throws(()=>decoder.decode(null));throws(()=>decoder.decode([65]));throws(()=>decoder.decode('A'));
        throws(()=>decoder.decode(new Proxy(new Uint8Array([65]),{})));
        throws(()=>TextDecoder.prototype.decode.call({},bytes));
    "#,
    );
}

#[test]
fn streaming_decoder_preserves_partial_sequences_and_flushes() {
    check(
        r#"
        const d=new TextDecoder();
        eq(d.decode(new Uint8Array([0xF0,0x9F]),{stream:true}),'');
        eq(d.decode(new Uint8Array([0x98]),{stream:true}),'');
        eq(d.decode(new Uint8Array([0x80])),'😀');
        eq(d.decode(new Uint8Array([0xE2]),{stream:true}),'');
        eq(d.decode(),'�');
        eq(d.decode(new Uint8Array([65])),'A');
        const utf16=new TextDecoder('utf-16le');
        eq(utf16.decode(new Uint8Array([0x3d,0xd8,0]),{stream:true}),'');
        eq(utf16.decode(new Uint8Array([0xde])),'😀');
    "#,
    );
}

#[test]
fn fatal_errors_keep_stream_remainder_and_reset_after_flush() {
    check(
        r#"
        const d=new TextDecoder('utf-8',{fatal:true});
        throws(()=>d.decode(new Uint8Array([255,65]),{stream:true}));
        eq(d.decode(new Uint8Array([66])),'AB');
        eq(d.decode(new Uint8Array([0xe2]),{stream:true}),'');
        throws(()=>d.decode());
        eq(d.decode(new Uint8Array([67])),'C');
        throws(()=>d.decode(new Uint8Array([255])));
        eq(d.decode(new Uint8Array([68])),'D');
        eq(d.fatal,true);eq(d.ignoreBOM,false);
    "#,
    );
}

#[test]
fn bom_removal_is_per_stream_and_never_changes_selected_encoding() {
    check(
        r#"
        const bom=new Uint8Array([239,187,191,65]); const d=new TextDecoder();
        eq(d.decode(bom),'A');eq(d.decode(bom),'A');
        eq(d.decode(bom,{stream:true}),'A');eq(d.decode(bom),'\uFEFFA');
        const split=new TextDecoder();eq(split.decode(bom.subarray(0,1),{stream:true}),'');
        eq(split.decode(bom.subarray(1)),'A');
        eq(new TextDecoder('utf-8',{ignoreBOM:true}).decode(bom),'\uFEFFA');
        eq(new TextDecoder('utf-16be').decode(new Uint8Array([0xff,0xfe,0,65])),'\uFFFEA');
        eq(new TextDecoder('windows-1252').decode(bom),'ï»¿A');
    "#,
    );
}

#[test]
fn labels_legacy_decoders_and_option_conversion_order() {
    check(
        r#"
        eq(new TextDecoder(' ASCII ').encoding,'windows-1252');
        eq(new TextDecoder('latin1').decode(new Uint8Array([128])),'€');
        eq(new TextDecoder('utf-16').encoding,'utf-16le');
        eq(new TextDecoder('shift_jis').decode(new Uint8Array([0x82,0xa0])),'あ');
        eq(new TextDecoder('x-user-defined').decode(new Uint8Array([128])),'\uF780');
        throws(()=>new TextDecoder('replacement'),RangeError);throws(()=>new TextDecoder('bogus'),RangeError);
        throws(()=>new TextDecoder('utf-8',false));
        const seen=[];throws(()=>new TextDecoder('bogus',{get ignoreBOM(){seen.push('bom');return false},get fatal(){seen.push('fatal');return false}}),RangeError);
        eq(seen,['fatal','bom']);
        const d=new TextDecoder();throws(()=>d.decode('invalid',{get stream(){throw new Error('should not run')}}));
        eq(Object.prototype.toString.call(d),'[object TextDecoder]');
    "#,
    );
}

#[test]
fn shared_buffers_use_real_atomic_storage() {
    check(
        r#"
        const buffer=new SharedArrayBuffer(8);const bytes=new Uint8Array(buffer);
        const view=bytes.subarray(2,6);const encoder=new TextEncoder();
        eq(encoder.encodeInto('😀',view),{read:2,written:4});
        eq(new TextDecoder().decode(view),'😀');
        eq([...bytes],[0,0,240,159,152,128,0,0]);
        eq(new TextDecoder().decode(new DataView(buffer,2,4)),'😀');
    "#,
    );
}

#[test]
fn detached_buffers_and_reentrant_option_getters_are_checked() {
    check(
        r#"
        const bytes=new Uint8Array([65]);detachBuffer(bytes.buffer);
        eq(new TextDecoder().decode(bytes),'');
        eq(new TextEncoder().encodeInto('A',bytes),{read:0,written:0});
        const other=new Uint8Array([66]);const d=new TextDecoder();
        eq(d.decode(other,{get stream(){detachBuffer(other.buffer);return false}}),'');
        const valid=new Uint8Array([67]);
        eq(d.decode(valid,{get stream(){eq(d.decode(new Uint8Array([68])),'D');return false}}),'C');
    "#,
    );
}

#[test]
fn base64_uses_browser_binary_strings_and_forgiving_decode() {
    check(
        r#"
        eq(btoa('\0\xFF'),'AP8=');eq(atob('AP8='),'\0\xFF');
        eq(atob(' Y W\tJ\nf\r\f'),'ab_');eq(atob('YQ'),'a');eq(atob('YR=='),'a');
        eq(atob(''),'');eq(btoa(''),'');eq(btoa(null),'bnVsbA==');
        for(const value of ['A','A===','YQ=','YQ===','AA-','AA_','\vYQ==','YQ==\u00A0']) {
            const error=throws(()=>atob(value),DOMException);eq(error.name,'InvalidCharacterError');eq(error.code,5);eq(error instanceof Error,true);
        }
        const error=throws(()=>btoa('€'),DOMException);eq(error.name,'InvalidCharacterError');
        throws(()=>atob());throws(()=>btoa());throws(()=>atob(Symbol()));
        throws(()=>btoa('\uD800'),DOMException);
    "#,
    );
}

#[test]
fn dom_exceptions_have_native_brand_and_error_inheritance() {
    check(
        r#"
        const error=new DOMException('message','TimeoutError');
        eq([error.name,error.message,error.code],['TimeoutError','message',23]);
        eq(error.toString(),'TimeoutError: message');eq(error instanceof Error,true);
        eq(DOMException.INVALID_CHARACTER_ERR,5);eq(error.INVALID_CHARACTER_ERR,5);
        eq(Object.prototype.toString.call(error),'[object DOMException]');
        eq(new DOMException('x','CustomError').code,0);
        eq([new DOMException().name,new DOMException().message],['Error','']);
        throws(()=>Object.getOwnPropertyDescriptor(DOMException.prototype,'name').get.call({}));
    "#,
    );
}

#[test]
fn allocation_limits_do_not_corrupt_destination_or_decoder() {
    check(
        r#"
        const encoder=new TextEncoder();const target=new Uint8Array([1,2]);
        throws(()=>encoder.encodeInto('x'.repeat(4*1024*1024+1),target),RangeError);eq([...target],[1,2]);
        const d=new TextDecoder();eq(d.decode(new Uint8Array([0xe2]),{stream:true}),'');
        throws(()=>d.decode(new Uint8Array(4*1024*1024+1)),RangeError);
        eq(d.decode(new Uint8Array([0x82,0xac])),'€');
    "#,
    );
}

#[test]
fn resizable_and_growable_buffer_sources_are_rejected_with_intrinsic_checks() {
    check(
        r#"
        const decoder=new TextDecoder(),encoder=new TextEncoder();
        const buffers=[new ArrayBuffer(4,{maxByteLength:8}),new SharedArrayBuffer(4,{maxByteLength:8})];
        for(const buffer of buffers) {
            Object.defineProperty(buffer,'resizable',{value:false});
            Object.defineProperty(buffer,'growable',{value:false});
            throws(()=>decoder.decode(buffer));
            throws(()=>decoder.decode(new Uint8Array(buffer)));
            throws(()=>decoder.decode(new DataView(buffer)));
            throws(()=>encoder.encodeInto('x',new Uint8Array(buffer)));
        }
        const bytes=new Uint8Array([65]);
        for(const name of ['buffer','byteOffset','byteLength']) Object.defineProperty(bytes,name,{get(){throw Error('shadow')}});
        eq(decoder.decode(bytes),'A');eq(encoder.encodeInto('B',bytes),{read:1,written:1});
        Object.defineProperty(ArrayBuffer.prototype,'resizable',{get(){throw Error('replaced')}});
        Object.defineProperty(SharedArrayBuffer.prototype,'growable',{get(){throw Error('replaced')}});
        eq(decoder.decode(new Uint8Array([67])),'C');
        throws(()=>decoder.decode(new ArrayBuffer(4,{maxByteLength:8})));
    "#,
    );
}
#[test]
fn native_dom_exception_helper_ignores_mutable_global_constructor() {
    let mut context = Context::builder()
        .instructions_remaining(10_000_000)
        .build()
        .unwrap();
    register(&mut context).unwrap();
    context
        .eval(Source::from_bytes(
            "globalThis.DOMException=function(){throw Error('replaced')}",
        ))
        .unwrap();
    let exception = dom_exception("aborted", "AbortError", &mut context).unwrap();
    assert_eq!(
        exception
            .get(JsString::from("name"), &mut context)
            .unwrap()
            .as_string()
            .unwrap()
            .to_std_string_lossy(),
        "AbortError"
    );
    assert_eq!(
        exception
            .get(JsString::from("code"), &mut context)
            .unwrap()
            .as_number(),
        Some(20.0)
    );
}
