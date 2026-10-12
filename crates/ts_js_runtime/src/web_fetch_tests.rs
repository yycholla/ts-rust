use super::*;
use boa_engine::{Source, builtins::promise::PromiseState};

fn context_with_entropy(entropy: Option<MultipartEntropy>) -> Context {
    let mut c = Context::builder()
        .instructions_remaining(10_000_000)
        .build()
        .unwrap();
    crate::web_text::register(&mut c).unwrap();
    crate::web_url::register(&mut c).unwrap();
    crate::web_blob::register(&mut c).unwrap();
    crate::web_form_data::register(&mut c).unwrap();
    crate::web_headers::register(&mut c).unwrap();
    crate::web_abort::register(&mut c).unwrap();
    crate::web_streams::register(&mut c).unwrap();
    register(
        &mut c,
        FetchOptions {
            base_url: Some("https://example.test/app/".into()),
            origin: None,
            multipart_entropy: entropy,
        },
    )
    .unwrap();
    c
}
fn context() -> Context {
    // Deterministic fixture only; the production host must supply real entropy.
    let counter = Rc::new(Cell::new(0u8));
    context_with_entropy(Some(Rc::new(move |bytes| {
        counter.set(counter.get().wrapping_add(1));
        bytes.fill(counter.get());
        Ok(())
    })))
}
fn run(c: &mut Context, body: &str) {
    let source = format!(
        r#"(async()=>{{
        const stringify=JSON.stringify;
        function eq(a,b){{if(stringify(a)!==stringify(b))throw Error(stringify({{actual:a,expected:b}}));}}
        function assert(v){{if(!v)throw Error('Assertion failed');}}
        function throws(fn,name='TypeError'){{try{{fn()}}catch(e){{eq(e.name,name);return e}}throw Error('Expected throw');}}
        async function rejects(p,name='TypeError'){{try{{await p}}catch(e){{eq(e.name,name);return e}}throw Error('Expected rejection');}}
        {body}
        return true;
    }})()"#
    );
    let value = c.eval(Source::from_bytes(&source)).unwrap();
    let promise = JsPromise::from_object(value.as_object().unwrap()).unwrap();
    c.run_jobs().unwrap();
    match promise.state() {
        PromiseState::Fulfilled(value) => assert_eq!(value.as_boolean(), Some(true)),
        PromiseState::Rejected(reason) => panic!(
            "Fetch test rejected: {}",
            reason.to_string(c).unwrap().to_std_string_escaped()
        ),
        PromiseState::Pending => panic!("Fetch test remained pending after draining jobs"),
    }
}
fn check(body: &str) {
    run(&mut context(), body)
}

#[test]
fn response_snapshot_preserves_bytes_duplicates_and_shared_body_under_poisoned_accessors() {
    let mut c = context();
    let value = c.eval(Source::from_bytes(r#"
        (() => {
            const bytes = new Uint8Array([7, 0, 255, 128, 7]);
            const response = new Response(bytes.subarray(1, 4), {
                status: 201, statusText: 'caf\u00e9',
                headers: [['Set-Cookie','a=1'],['Set-Cookie','b=2'],
                          ['x-test','one'],['x-test','two'],['x-byte','\u00ff']]
            });
            bytes.fill(42);
            globalThis.snapshotHeaders = response.headers;
            globalThis.snapshotCalls = 0;
            for (const key of ['status','statusText','headers','body','bodyUsed','arrayBuffer','bytes']) {
                Object.defineProperty(response, key, {get() { snapshotCalls++; throw Error('overridden accessor'); }});
            }
            Object.setPrototypeOf(response, null);
            globalThis.Response = undefined;
            globalThis.Uint8Array = undefined;
            return response;
        })()
    "#)).unwrap();
    let snapshot = response_snapshot(&value).unwrap();
    assert_eq!(snapshot.status, 201);
    assert_eq!(snapshot.status_text, b"caf\xe9");
    assert_eq!(
        snapshot.headers,
        vec![
            (b"set-cookie".to_vec(), b"a=1".to_vec()),
            (b"set-cookie".to_vec(), b"b=2".to_vec()),
            (b"x-test".to_vec(), b"one".to_vec()),
            (b"x-test".to_vec(), b"two".to_vec()),
            (b"x-byte".to_vec(), vec![255]),
        ]
    );
    let body = snapshot.body.as_ref().unwrap();
    let again = response_snapshot(&value).unwrap();
    assert!(JsValue::same_value(
        &body.stream().into(),
        &again.body.unwrap().stream().into()
    ));
    assert!(!body.body_used().unwrap());
    assert!(!body.unusable().unwrap());
    let pending = crate::web_streams::fully_read_bytes(&body.stream().into(), &mut c).unwrap();
    c.run_jobs().unwrap();
    let PromiseState::Fulfilled(bytes) = pending.state() else {
        panic!("Native byte consumption must fulfill")
    };
    assert_eq!(
        crate::web_streams::copy_uint8_chunk(&bytes, &mut c).unwrap(),
        vec![0, 255, 128]
    );
    assert!(body.body_used().unwrap());
    assert!(
        response_snapshot(&value)
            .unwrap()
            .body
            .unwrap()
            .unusable()
            .unwrap()
    );
    let headers = c
        .global_object()
        .get(JsString::from("snapshotHeaders"), &mut c)
        .unwrap();
    web_headers::append_header_bytes(&headers, b"x-later", b"updated").unwrap();
    assert_eq!(
        snapshot.headers.len(),
        5,
        "Header snapshots are immutable copies"
    );
    assert_eq!(response_snapshot(&value).unwrap().headers.len(), 6);
    assert_eq!(
        c.global_object()
            .get(JsString::from("snapshotCalls"), &mut c)
            .unwrap()
            .as_number(),
        Some(0.0)
    );
}

#[test]
fn response_snapshot_requires_native_brand_and_keeps_null_bodies_null() {
    let mut c = context();
    for source in [
        "({status:200,headers:[]})",
        "Object.create(Response.prototype)",
        "new Request('https://example.test/')",
        "new Proxy(new Response('real'), {})",
    ] {
        let value = c.eval(Source::from_bytes(source)).unwrap();
        assert!(
            response_snapshot(&value).is_err(),
            "Reject unbranded {source}"
        );
    }
    for (source, status) in [
        ("new Response(null,{status:204})", 204),
        ("Response.error()", 0),
    ] {
        let value = c.eval(Source::from_bytes(source)).unwrap();
        for _ in 0..2 {
            let snapshot = response_snapshot(&value).unwrap();
            assert_eq!(snapshot.status, status);
            assert!(snapshot.status_text.is_empty());
            assert!(snapshot.headers.is_empty());
            assert!(snapshot.body.is_none());
        }
    }
}

#[test]
fn request_defaults_url_referrer_and_dictionary_members() {
    check(
        r#"
    const r=new Request('../items?q=1#frag');
    eq([r.url,r.method,r.destination,r.referrer,r.referrerPolicy,r.mode,r.credentials,r.cache,r.redirect,r.integrity,r.keepalive,r.isReloadNavigation,r.isHistoryNavigation,r.duplex],
       ['https://example.test/items?q=1#frag','GET','','about:client','','cors','same-origin','default','follow','',false,false,false,'half']);
    assert(r.headers===r.headers&&r.signal===r.signal);assert(r.body===null&&!r.bodyUsed);
    eq(Object.prototype.toString.call(r),'[object Request]');
    const q=new Request('/x',{method:'patch',mode:'same-origin',cache:'only-if-cached',credentials:'include',redirect:'manual',integrity:'x\ud800',keepalive:true,referrer:'/from',referrerPolicy:'origin',priority:'high',window:null});
    eq([q.method,q.mode,q.cache,q.credentials,q.redirect,q.integrity,q.keepalive,q.referrer,q.referrerPolicy],['patch','same-origin','only-if-cached','include','manual','x\ud800',true,'https://example.test/from','origin']);
    eq(new Request('/x',{referrer:'https://other.test/x'}).referrer,'about:client');
    eq(new Request('/x',{referrer:''}).referrer,'');
    throws(()=>new Request());throws(()=>new Request('https://u:p@example.test/'));
    throws(()=>new Request('/x',{method:'TRACE'}));throws(()=>new Request('/x',{method:'bad method'}));
    throws(()=>new Request('/x',{mode:'no-cors',method:'PUT'}));throws(()=>new Request('/x',{cache:'only-if-cached'}));
    throws(()=>new Request('/x',{mode:'navigate'}));throws(()=>new Request('/x',{window:{}}));
    assert(typeof fetch==='undefined');
    "#,
    )
}

#[test]
fn conversion_is_lexical_inherited_and_precedes_http_validation() {
    check(
        r#"
    const log=[];
    const values={body:{toString(){log.push('body:string');return 'x'}},method:{toString(){log.push('method:string');return 'post'}},headers:[['bad name','x']]};
    const proto=new Proxy(values,{get(t,k){log.push(k);return t[k]}});
    throws(()=>new Request({toString(){log.push('input');return '/x'}},Object.create(proto)));
    eq(log,['input','body','body:string','cache','credentials','duplex','headers','integrity','keepalive','method','method:string','mode','priority','redirect','referrer','referrerPolicy','signal','window']);
    const observed=[];
    throws(()=>new Response({toString(){observed.push('body');return 'x'}},{get headers(){observed.push('headers');return [['only-one']]},get status(){observed.push('status');return 0},get statusText(){observed.push('statusText');return ''}}),'RangeError');
    eq(observed,['body','headers','status','statusText']);
    let gotStatus=false;
    throws(()=>new Response(null,{headers:[['x','\u0100']],get status(){gotStatus=true;return 0}}));assert(!gotStatus);
    eq(throws(()=>new Request('http://[',{mode:'not-a-mode'})).message,'Invalid Fetch enum value');
    "#,
    )
}

#[test]
fn response_status_cookies_and_existing_response_as_init() {
    check(
        r#"
    const a=new Response('hello',{status:65536+201,statusText:'Created',headers:[['Set-Cookie','a=1'],['set-cookie','b=2']]});
    eq([a.status,a.statusText,a.ok,a.type,a.url,a.redirected],[201,'Created',true,'default','',false]);
    eq(a.headers.getSetCookie(),['a=1','b=2']);eq(a.headers.get('content-type'),'text/plain;charset=UTF-8');
    const b=new Response('copy',a);eq(b.status,201);eq(b.headers.getSetCookie(),['a=1','b=2']);
    b.headers.set('x','b');assert(!a.headers.has('x'));eq(await a.text(),'hello');eq(await b.text(),'copy');
    for(const status of [0,199,600,NaN,Infinity])throws(()=>new Response(null,{status}),'RangeError');
    for(const status of [204,205,304]){throws(()=>new Response('',{status}));eq(new Response(null,{status}).body,null)}
    throws(()=>new Response(null,{statusText:'bad\rline'}));throws(()=>new Response(null,{statusText:'\u0100'}));
    eq(new Response(null,{statusText:'\t\u00ff'}).statusText,'\t\u00ff');
    "#,
    )
}

#[test]
fn body_reading_locking_disturbance_and_null_body() {
    check(
        r#"
    const r=new Response('hello');assert(!r.bodyUsed);const reader=r.body.getReader();assert(!r.bodyUsed);
    await rejects(r.text());throws(()=>r.clone());reader.releaseLock();
    eq(await r.text(),'hello');assert(r.bodyUsed&&r.body.locked);await rejects(r.bytes());throws(()=>r.clone());
    const empty=new Response();eq(await empty.text(),'');eq(await empty.text(),'');assert(!empty.bodyUsed);await rejects(empty.json(),'SyntaxError');
    const utf=new Response('x\ud800');eq(Array.from(await utf.bytes()),[120,239,191,189]);
    const buf=await new Response(new Uint8Array([1,2])).arrayBuffer();eq(Array.from(new Uint8Array(buf)),[1,2]);
    eq(await new Response('{"ok":true}').json(),{ok:true});
    const blob=await new Response('abc',{headers:{'content-type':'TEXT/PLAIN'}}).blob();eq([blob.type,blob.size],['text/plain',3]);
    await rejects(Request.prototype.text.call(new Response()));await rejects(Response.prototype.text.call({}));
    "#,
    )
}

#[test]
fn native_clone_has_independent_headers_and_branch_bytes() {
    check(
        r#"
    const r=new Response(new Uint8Array([7,8,9])),q=r.clone();assert(r.body!==q.body);
    q.headers.set('x','q');assert(!r.headers.has('x'));
    eq(await Promise.all([r.bytes().then(b=>Array.from(b)),q.bytes().then(b=>Array.from(b))]),[[7,8,9],[7,8,9]]);
    let controller;const source=new ReadableStream({start(c){controller=c}});
    const a=new Response(source),b=a.clone();const pa=a.bytes(),pb=b.bytes();
    controller.enqueue(new Uint8Array([3,4]));controller.close();
    eq(Array.from(await pa),[3,4]);eq(Array.from(await pb),[3,4]);
    "#,
    )
}

#[test]
fn request_copy_proxies_and_null_init_does_not_erase_body() {
    check(
        r#"
    const a=new Request('/x',{method:'post',body:'original',headers:{x:'a'}});
    const b=new Request(a,{body:null});assert(a.body!==b.body&&a.body.locked);assert(a.signal!==b.signal);
    eq(await b.text(),'original');await rejects(a.text());
    const x=new Request('/x',{method:'POST',body:'keep'}),y=new Request(x,{body:'replacement'});
    assert(!x.body.locked);eq(await x.text(),'keep');eq(await y.text(),'replacement');
    const q=new Request('/x',{method:'POST',body:'copy'}),p=new Request('/other',q);
    eq([p.method,p.url],['POST','https://example.test/other']);eq(await p.text(),'copy');
    const consumed=new Request('/x',{method:'POST',body:'x'});await consumed.text();throws(()=>new Request(consumed));
    const reentrant=new Request('/x',{method:'POST',body:'x'});let reading;
    throws(()=>new Request(reentrant,{get headers(){reading=reentrant.text();return {}}}));eq(await reading,'x');
    "#,
    )
}

#[test]
fn request_stream_constraints_and_body_extraction_timing() {
    check(
        r#"
    throws(()=>new Request('/x',{body:'x'}));throws(()=>new Request('/x',{method:'HEAD',body:'x'}));
    const stream=()=>new ReadableStream({start(c){c.enqueue(new Uint8Array([65]));c.close()}});
    throws(()=>new Request('/x',{method:'POST',body:stream()}));
    throws(()=>new Request('/x',{method:'POST',body:stream(),duplex:'half',keepalive:true}));
    throws(()=>new Request('/x',{method:'POST',body:stream(),duplex:'half',mode:'no-cors'}));
    const a=new Request('/x',{method:'POST',body:stream(),duplex:'half'});eq(await a.text(),'A');assert(!a.headers.has('content-type'));
    const bytes=new Uint8Array([65]);const b=new Response(bytes,{get status(){bytes[0]=66;return 200}});bytes[0]=67;eq(await b.text(),'B');
    const noCors=new Request('/x',{mode:'no-cors',method:'POST',headers:{'Set-Cookie':'a=1','X-Server':'yes'},body:'x'});
    eq(noCors.headers.getSetCookie(),['a=1']);eq(noCors.headers.get('x-server'),'yes');
    "#,
    )
}

#[test]
fn abort_followers_clone_and_explicit_null_signal() {
    check(
        r#"
    const controller=new AbortController(),r=new Request('/x',{signal:controller.signal}),q=r.clone();
    assert(r.signal!==controller.signal&&q.signal!==r.signal);
    const order=[];r.signal.addEventListener('abort',()=>{assert(q.signal.aborted);order.push('r')});q.signal.addEventListener('abort',()=>order.push('q'));
    const reason={why:1};controller.abort(reason);eq(order,['r','q']);assert(r.signal.reason===reason&&q.signal.reason===reason);
    const after=new Request(r);assert(after.signal.aborted&&after.signal.reason===reason);
    const detached=new Request(r,{signal:null});assert(!detached.signal.aborted);
    throws(()=>new Request('/x',{signal:{aborted:false}}));
    "#,
    )
}

#[test]
fn response_static_factories_order_and_immutable_guards() {
    check(
        r#"
    const e=Response.error();eq([e.status,e.statusText,e.type,e.ok,e.body],[0,'','error',false,null]);throws(()=>e.headers.set('x','y'));throws(()=>e.clone().headers.append('x','y'));
    const r=Response.redirect('../next#x',307);eq([r.status,r.headers.get('location'),r.url,r.redirected],[307,'https://example.test/next#x','',false]);throws(()=>r.headers.delete('location'));
    throws(()=>Response.redirect('/x',200),'RangeError');throws(()=>Response.redirect('http://[',200));
    const log=[];const j=Response.json({toJSON(){log.push('toJSON');return {x:1}}},{get headers(){log.push('headers');return {}},get status(){log.push('status');return 201},get statusText(){log.push('statusText');return 'Created'}});
    eq(log,['headers','status','statusText','toJSON']);eq([j.status,j.headers.get('content-type')],[201,'application/json']);eq(await j.json(),{x:1});
    throws(()=>Response.json(undefined));throws(()=>Response.json({x:1},{status:204}));
    const typed=Response.json(1,{headers:{'Content-Type':'application/custom'}});eq(typed.headers.get('content-type'),'application/custom');eq(await typed.text(),'1');
    "#,
    )
}

#[test]
fn native_operations_ignore_poisoned_public_globals() {
    check(
        r#"
    const R=Request,S=Response;const controller=new AbortController();const r=new R('/x',{method:'POST',body:'x',signal:controller.signal});
    const poison=()=>{throw Error('poison')};
    globalThis.Request=globalThis.Response=globalThis.Headers=globalThis.URL=globalThis.AbortSignal=globalThis.ReadableStream=poison;
    JSON.stringify=poison;JSON.parse=poison;
    const clone=r.clone();const response=S.json({ok:true});eq(await response.json(),{ok:true});
    eq(await clone.text(),'x');eq(await r.text(),'x');
    const copy=new R('/y',{signal:controller.signal});controller.abort('native');eq(copy.signal.reason,'native');
    eq(S.redirect('/z').headers.get('location'),'https://example.test/z');
    "#,
    )
}

#[test]
fn multipart_native_roundtrip_and_fresh_host_boundaries() {
    check(
        r#"
    const form=new FormData();form.append('text','hello');form.append('file',new Blob(['bytes'],{type:'text/plain'}),'a.txt');
    const a=new Response(form),b=new Request('/x',{method:'POST',body:form});
    const ta=a.headers.get('content-type'),tb=b.headers.get('content-type');assert(ta.startsWith('multipart/form-data; boundary=')&&ta!==tb);
    const parsed=await a.formData();eq(parsed.get('text'),'hello');eq([parsed.get('file').name,parsed.get('file').type,await parsed.get('file').text()],['a.txt','text/plain','bytes']);
    const data=await b.formData();eq(data.get('text'),'hello');
    const url=new Response(new URLSearchParams([['x','a b'],['x','c']]));eq(url.headers.get('content-type'),'application/x-www-form-urlencoded;charset=UTF-8');eq((await url.formData()).getAll('x'),['a b','c']);
    "#,
    )
}

#[test]
fn missing_entropy_and_repeated_entropy_are_explicit() {
    let mut c = context_with_entropy(None);
    run(
        &mut c,
        r#"eq(await new Response('ok').text(),'ok');const f=new FormData();f.append('x','y');assert(throws(()=>new Response(f)).message.includes('entropy'));"#,
    );
    let mut c = context_with_entropy(Some(Rc::new(|bytes| {
        bytes.fill(0);
        Ok(())
    })));
    run(
        &mut c,
        r#"const f=new FormData();new Response(f);assert(throws(()=>new Response(f)).message.includes('boundary'));"#,
    );
}

#[test]
fn brands_subclasses_bounds_and_native_handles() {
    let mut c = context();
    run(
        &mut c,
        r#"
    class R extends Request{}class S extends Response{}
    assert(new R('/x') instanceof R);assert(new S('x') instanceof S);
    throws(()=>Request('/x'));throws(()=>Response('x'));throws(()=>Request.prototype.clone.call({}));
    throws(()=>Object.getOwnPropertyDescriptor(Response.prototype,'status').get.call({}));
    throws(()=>new Request('x'.repeat(1048577)),'RangeError');
    assert(Object.getOwnPropertyDescriptor(Response.prototype,'text').enumerable);
    assert(Object.getOwnPropertyDescriptor(Response,'json').enumerable);
    globalThis.nativeRequest=new Request('/x',{method:'POST',body:'x'});globalThis.nativeResponse=new Response('y');
    "#,
    );
    let request = c
        .global_object()
        .get(JsString::from("nativeRequest"), &mut c)
        .unwrap();
    let response = c
        .global_object()
        .get(JsString::from("nativeResponse"), &mut c)
        .unwrap();
    assert!(is_request(&request) && !is_response(&request));
    assert!(is_response(&response) && !is_request(&response));
    assert!(request_body(&request).unwrap().is_some());
    assert!(response_body(&response).unwrap().is_some());
    boa_gc::force_collect();
    run(
        &mut c,
        "eq(await nativeRequest.text(),'x');eq(await nativeResponse.text(),'y');",
    );
}

#[test]
fn multipart_collision_retries_with_a_fresh_boundary() {
    check(
        r#"
    const form=new FormData();
    const first='----ts-rust-'+'01'.repeat(16);
    form.append('collision','prefix\r\n--'+first+'\r\nsuffix');
    const response=new Response(form);
    eq(response.headers.get('content-type'),'multipart/form-data; boundary=----ts-rust-'+'02'.repeat(16));
    eq((await response.formData()).get('collision'),'prefix\r\n--'+first+'\r\nsuffix');
    "#,
    )
}
