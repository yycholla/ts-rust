use super::*;
use boa_engine::Source;
fn new_context() -> Context {
    Context::builder()
        .instructions_remaining(10_000_000)
        .build()
        .unwrap()
}
fn check(body: &str) {
    let mut context = new_context();
    register(&mut context).unwrap();
    let source = format!(
        r#"(()=>{{
        function eq(actual,expected){{if(JSON.stringify(actual)!==JSON.stringify(expected))throw new Error(JSON.stringify({{actual,expected}}));}}
        function throws(fn,kind=TypeError){{try{{fn();}}catch(error){{if(error instanceof kind)return;throw error;}}throw new Error('Expected exception');}}
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
fn normalized_headers_duplicates_and_set_cookie_follow_fetch() {
    check(
        r#"
        const h=new Headers([['X-Z','\r\n value \t\r\n'],['a','1'],['A','2'],['set-cookie','a=1'],['Set-Cookie','b=2']]);
        eq(h.get('X-z'),'value');eq(h.get('missing'),null);eq(h.has('A'),true);
        eq(h.get('a'),'1, 2');eq(h.get('set-cookie'),'a=1, b=2');eq(h.getSetCookie(),['a=1','b=2']);
        eq([...h],[['a','1, 2'],['set-cookie','a=1'],['set-cookie','b=2'],['x-z','value']]);
        h.set('A','3');h.delete('X-Z');h.append('A','4');
        eq([...h.keys()],['a','set-cookie','set-cookie']);eq([...h.values()],['3, 4','a=1','b=2']);
        h.set('set-cookie','c=3');eq(h.getSetCookie(),['c=3']);
        h.delete('set-cookie');eq(h.getSetCookie(),[]);
        eq(Headers.prototype.entries===Headers.prototype[Symbol.iterator],true);
        eq(Object.prototype.toString.call(h),'[object Headers]');
        eq(Object.prototype.toString.call(h.entries()),'[object Headers Iterator]');
        const copy=new Headers(h);h.set('a','changed');eq(copy.get('a'),'3, 4');
    "#,
    );
}
#[test]
fn bytestring_validation_normalization_and_required_arguments() {
    check(
        r#"
        const h=new Headers();h.set('X-Obs','\u00ff\u0080\u000b\u000c');
        eq(h.get('x-obs'),'\u00ff\u0080\u000b\u000c');
        for(const value of ['\u0100','😀','\ud800',Symbol('x')])throws(()=>h.set('x',value));
        for(const name of ['', 'a b', 'ä', 'a:', '\n'])throws(()=>h.get(name));
        for(const value of ['a\nb','a\rb','a\0b'])throws(()=>h.set('x',value));
        h.set('x',' \r\n\t ');eq(h.get('x'),'');
        h.set('x',undefined);eq(h.get('x'),'undefined');h.set('x',null);eq(h.get('x'),'null');
        for(const method of ['get','has','delete'])throws(()=>h[method]());
        for(const method of ['append','set'])throws(()=>h[method]('x'));
        throws(()=>Headers());throws(()=>Headers.prototype.get.call({},'x'));
        for(const init of [null,3,'abc',false])throws(()=>new Headers(init));
        throws(()=>new Headers({[Symbol('x')]:'bad'}));
    "#,
    );
}
#[test]
fn records_iterables_and_conversion_side_effect_order() {
    check(
        r#"
        const record=Object.create({inherited:'no'});record.Z=1;record.a=2;
        Object.defineProperty(record,'hidden',{value:3});
        eq([...new Headers(record)],[['a','2'],['z','1']]);
        const events=[];
        const h=new Headers({[Symbol.iterator]:function*(){
            yield ['A',{toString(){events.push('first');return 'one'}}];
            yield ['a',{toString(){events.push('second');return 'two'}}];
        }});
        eq(h.get('a'),'one, two');eq(events,['first','second']);
        const order=[];
        throws(()=>h.append({toString(){order.push('name');return 'bad name'}},{toString(){order.push('value');return 'ok'}}));
        eq(order,['name','value']);
        const later=[];
        throws(()=>new Headers([['only-one'],['x',{toString(){later.push('converted');return 'v'}}]]));
        eq(later,['converted']);
        const touched=[];
        throws(()=>new Headers({'bad name':'v',get later(){touched.push('later');return 'ok'}}));
        eq(touched,['later']);
    "#,
    );
}
#[test]
fn synchronous_webidl_errors_do_not_invent_iterator_close_calls() {
    check(
        r#"
        const sentinel=new Error('sentinel');const events=[];
        const pair={ [Symbol.iterator](){let i=0;return {
            next(){return i++===0?{value:'x',done:false}:{value:{toString(){throw sentinel}},done:false}},
            return(){events.push('inner return');return {done:true}}
        }}};
        const outer={ [Symbol.iterator](){let i=0;return {
            next(){return i++===0?{value:pair,done:false}:{done:true}},
            return(){events.push('outer return');return {done:true}}
        }}};
        try{new Headers(outer);throw new Error('missing throw')}catch(e){eq(e===sentinel,true)}
        eq(events,[]);
        const steps=[];
        const bad={ [Symbol.iterator](){return {next(){throw sentinel},return(){steps.push('return');return {}}}}};
        try{new Headers(bad)}catch(e){eq(e===sentinel,true)}eq(steps,[]);
    "#,
    );
}
#[test]
fn iterator_and_foreach_observe_mutations_in_sorted_live_order() {
    check(
        r#"
        const h=new Headers({b:'B',c:'C'});const i=h.entries();
        eq(i.next().value,['b','B']);h.append('a','A');
        eq(i.next().value,['b','B']);h.set('c','changed');eq(i.next().value,['c','changed']);
        eq(i.next().done,true);h.set('z','Z');eq(i.next().done,true);
        const live=new Headers({a:'A',b:'B'});const seen=[];const receiver={};
        live.forEach(function(value,key,headers){
            eq(this===receiver,true);eq(headers===live,true);seen.push([key,value]);
            if(key==='a'){headers.delete('b');headers.append('c','C')}
        },receiver);
        eq(seen,[['a','A'],['c','C']]);
        const broken=live.entries();throws(()=>broken.next.call({}));
        throws(()=>live.forEach(1));
    "#,
    );
}
#[test]
fn native_lists_and_immutable_guards_preserve_raw_bytes_and_duplicate_order() {
    let mut context = new_context();
    register(&mut context).unwrap();
    let object = from_header_list(
        vec![
            (b"X".to_vec(), vec![255]),
            (b"x".to_vec(), b" two ".to_vec()),
        ],
        &mut context,
    )
    .unwrap();
    assert_eq!(
        read_header_list(&object.clone().into()).unwrap(),
        vec![(b"x".to_vec(), vec![255]), (b"x".to_vec(), b"two".to_vec())]
    );
    make_immutable(&object.clone().into()).unwrap();
    context
        .register_global_property(JsString::from("h"), object, Attribute::all())
        .unwrap();
    let result=context.eval(Source::from_bytes("let failed=0;for(const fn of [()=>h.append('x','3'),()=>h.set('x','3'),()=>h.delete('missing')]){try{fn()}catch(e){if(e instanceof TypeError)failed++}}failed")).unwrap();
    assert_eq!(result.as_number(), Some(3.0));
}
#[test]
fn mutations_and_constructor_bounds_are_atomic_and_close_only_host_aborts() {
    check(
        r#"
        const h=new Headers({x:'old'});throws(()=>h.set('x','a'.repeat(4*1024*1024)),RangeError);eq(h.get('x'),'old');
        const events=[];
        const inner={ [Symbol.iterator](){return {next(){return {value:'',done:false}},return(){events.push('inner');throw Error('ignored')}}}};
        const outer={ [Symbol.iterator](){return {next(){return {value:inner,done:false}},return(){events.push('outer');throw Error('ignored')}}}};
        throws(()=>new Headers(outer),RangeError);eq(events,['inner','outer']);
    "#,
    );
}
#[test]
fn registration_is_explicit_and_preserves_existing_globals() {
    let mut context = new_context();
    assert_eq!(
        context
            .eval(Source::from_bytes("typeof Headers"))
            .unwrap()
            .as_string()
            .unwrap()
            .to_std_string_lossy(),
        "undefined"
    );
    context
        .eval(Source::from_bytes("globalThis.Headers=42"))
        .unwrap();
    assert!(register(&mut context).is_err());
    assert_eq!(
        context
            .eval(Source::from_bytes("Headers"))
            .unwrap()
            .as_number(),
        Some(42.0)
    );
    let mut context = new_context();
    register(&mut context).unwrap();
    assert!(register(&mut context).is_err());
}

#[test]
fn converted_initializer_defers_http_validation_and_preserves_sequence_arity() {
    let mut c = new_context();
    register(&mut c).unwrap();
    let value=c.eval(Source::from_bytes(r#"
        globalThis.steps=[];
        [[{toString(){steps.push('first');return 'X-First'}},'ok'],
         ['wrong-arity','one','two'],
         [{toString(){steps.push('last-name');return 'X-Last'}},{toString(){steps.push('last-value');return 'value'}}]]
    "#)).unwrap();
    let converted = convert_headers_init(&value, &mut c).unwrap();
    assert_eq!(
        c.eval(Source::from_bytes("JSON.stringify(steps)"))
            .unwrap()
            .to_string(&mut c)
            .unwrap()
            .to_std_string_escaped(),
        r#"["first","last-name","last-value"]"#
    );
    let object = from_header_list(vec![], &mut c).unwrap();
    assert!(fill_headers(&object.clone().into(), &converted).is_err());
    // Fill appends in order. The whole sequence had already converted before
    // arity failed, but only the first valid pair reached the target list.
    assert_eq!(
        read_header_list(&object.into()).unwrap(),
        vec![(b"x-first".to_vec(), b"ok".to_vec())]
    );
    let value = c
        .eval(Source::from_bytes(
            r#"({'bad name': 'ok', get later(){steps.push('record');return 'value'}})"#,
        ))
        .unwrap();
    let converted = convert_headers_init(&value, &mut c).unwrap();
    assert!(converted.to_header_list().is_err());
    assert_eq!(
        c.eval(Source::from_bytes("steps.at(-1)"))
            .unwrap()
            .to_string(&mut c)
            .unwrap()
            .to_std_string_escaped(),
        "record"
    );
    // A ByteString conversion error remains immediate, unlike HTTP validation.
    let value = c
        .eval(Source::from_bytes(
            r#"[['x','\u0100'],[{toString(){throw Error('must not run')}},'x']]"#,
        ))
        .unwrap();
    let error = convert_headers_init(&value, &mut c)
        .unwrap_err()
        .into_opaque(&mut c)
        .unwrap();
    assert_eq!(
        error
            .as_object()
            .unwrap()
            .get(JsString::from("name"), &mut c)
            .unwrap()
            .to_string(&mut c)
            .unwrap()
            .to_std_string_escaped(),
        "TypeError"
    );
    assert!(convert_headers_init(&JsValue::undefined(), &mut c).is_err());
    assert!(convert_headers_init(&JsValue::null(), &mut c).is_err());
}

#[test]
fn native_header_copies_preserve_guard_cookie_order_and_independent_mutation() {
    let mut c = new_context();
    register(&mut c).unwrap();
    let original = from_header_list_with_guard(
        vec![(b"Set-Cookie".to_vec(), b"a=1".to_vec())],
        HeadersGuard::Response,
        &mut c,
    )
    .unwrap();
    append_header_bytes(&original.clone().into(), b"SET-cookie", b" b=2 ").unwrap();
    let copy = copy_headers(&original.clone().into(), None, &mut c).unwrap();
    assert!(!JsObject::equals(&original, &copy));
    assert_eq!(
        header_guard(&copy.clone().into()).unwrap(),
        HeadersGuard::Response
    );
    append_header_bytes(&copy.clone().into(), b"x", b"copied").unwrap();
    assert_eq!(read_header_list(&original.clone().into()).unwrap().len(), 2);
    let frozen = copy_headers(
        &original.clone().into(),
        Some(HeadersGuard::Immutable),
        &mut c,
    )
    .unwrap();
    assert_eq!(
        header_guard(&frozen.clone().into()).unwrap(),
        HeadersGuard::Immutable
    );
    assert!(append_header_bytes(&frozen.clone().into(), b"x", b"blocked").is_err());
    assert_eq!(
        read_header_list(&frozen.clone().into()).unwrap(),
        read_header_list(&original.into()).unwrap()
    );
    let mutable = copy_headers(&frozen.into(), Some(HeadersGuard::RequestNoCors), &mut c).unwrap();
    // The selected server policy does not filter browser-forbidden names.
    append_header_bytes(&mutable.clone().into(), b"Cookie", b"server=value").unwrap();
    append_header_bytes(&mutable.clone().into(), b"Set-Cookie", b"c=3").unwrap();
    let list = read_header_list(&mutable.clone().into()).unwrap();
    assert_eq!(list[0], (b"set-cookie".to_vec(), b"a=1".to_vec()));
    assert_eq!(list[1], (b"set-cookie".to_vec(), b"b=2".to_vec()));
    assert_eq!(list[3], (b"set-cookie".to_vec(), b"c=3".to_vec()));
    assert!(append_header_bytes(&mutable.clone().into(), b"bad name", b"value").is_err());
    assert_eq!(read_header_list(&mutable.into()).unwrap(), list);
}

#[test]
fn converted_headers_survive_gc_native_captures_and_ignore_replaced_globals() {
    let mut c = new_context();
    register(&mut c).unwrap();
    let value = c
        .eval(Source::from_bytes(
            "[['Set-Cookie','one=1'],['set-cookie','two=2'],['x',' value ']]",
        ))
        .unwrap();
    let converted = convert_headers_init(&value, &mut c).unwrap();
    c.register_global_builtin_callable(
        JsString::from("finishHeaders"),
        0,
        NativeFunction::from_copy_closure_with_captures(
            |_, _, converted, c| {
                Ok(from_converted_headers_init(converted, HeadersGuard::Response, c)?.into())
            },
            converted,
        ),
    )
    .unwrap();
    drop(value);
    boa_gc::force_collect();
    let result = c
        .eval(Source::from_bytes(
            r#"
        globalThis.Headers=function(){throw Error('replaced constructor')};
        const finished=finishHeaders();
        JSON.stringify([finished.getSetCookie(),finished.get('x')]);
    "#,
        ))
        .unwrap();
    assert_eq!(
        result.to_string(&mut c).unwrap().to_std_string_escaped(),
        r#"[["one=1","two=2"],"value"]"#
    );
}
