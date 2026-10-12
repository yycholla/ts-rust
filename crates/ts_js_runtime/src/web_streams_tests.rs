use super::*;
use boa_engine::{Source, builtins::promise::PromiseState};
fn context() -> Context {
    let mut c = Context::builder()
        .instructions_remaining(10_000_000)
        .build()
        .unwrap();
    register(&mut c).unwrap();
    c
}
fn run(c: &mut Context, body: &str) {
    let source = format!(
        r#"(async()=>{{
      function eq(a,b){{if(JSON.stringify(a)!==JSON.stringify(b))throw Error(JSON.stringify({{actual:a,expected:b}}));}}
      function assert(v){{if(!v)throw Error('Assertion failed');}}
      function throws(fn,name='TypeError'){{try{{fn()}}catch(e){{eq(e.name,name);return e}}throw Error('Expected throw');}}
      async function rejects(p,expected){{try{{await p}}catch(e){{if(typeof expected==='string')eq(e.name,expected);else assert(e===expected);return}}throw Error('Expected rejection');}}
      {body}
      return true;
    }})()"#
    );
    let value = c.eval(Source::from_bytes(&source)).unwrap();
    let promise = JsPromise::from_object(value.as_object().unwrap()).unwrap();
    c.run_jobs().unwrap();
    match promise.state() {
        PromiseState::Fulfilled(value) => assert_eq!(value.as_boolean(), Some(true)),
        PromiseState::Rejected(reason) => {
            let message = reason
                .to_string(c)
                .map(|value| value.to_std_string_escaped())
                .unwrap_or_else(|error| format!("<reason string conversion failed: {error:?}>"));
            panic!("Async stream test rejected: {message}; value: {reason:?}");
        }
        PromiseState::Pending => panic!("Async stream test remained pending after draining jobs"),
    }
}
fn check(body: &str) {
    run(&mut context(), body)
}
#[test]
fn queue_sizes_backpressure_and_controller_states() {
    check(
        r#"
    let controller,sizeCalls=0,pulls=0;
    const source={start(c){assert(this===source);controller=c},pull(){pulls++}};
    const stream=new ReadableStream(source,{highWaterMark:3,size(v){sizeCalls++;return v.length}});
    eq(controller.desiredSize,3);controller.enqueue('ab');eq(controller.desiredSize,1);
    controller.enqueue('c');eq(controller.desiredSize,0);await Promise.resolve();eq(pulls,0);
    const r=stream.getReader();assert(stream.locked);eq(await r.read(),{value:'ab',done:false});
    eq(controller.desiredSize,2);assert(pulls>=1);eq(sizeCalls,2);
    controller.close();eq(await r.read(),{value:'c',done:false});await r.closed;
    eq(controller.desiredSize,0);eq(await r.read(),{done:true});
    throws(()=>controller.close());throws(()=>controller.enqueue('x'));r.releaseLock();assert(!stream.locked);
    throws(()=>new ReadableStreamDefaultController());assert(new ReadableStream({type:'bytes'}) instanceof ReadableStream);
    throws(()=>new ReadableStream({}, {highWaterMark:-1}),'RangeError');
"#,
    )
}
#[test]
fn start_promise_gates_pull_and_pending_reads_bypass_size() {
    check(
        r#"
    let start,controller,pulls=0,sizeCalls=0;
    const s=new ReadableStream({start(c){controller=c;return new Promise(resolve=>start=resolve)},pull(c){pulls++;c.enqueue(42)}},{highWaterMark:0,size(){sizeCalls++;return 1}});
    const r=s.getReader(),pending=r.read();let settled=false;pending.then(()=>settled=true);
    await Promise.resolve();eq([pulls,settled],[0,false]);start();
    eq(await pending,{value:42,done:false});eq([pulls,sizeCalls],[1,0]);
    controller.close();await r.closed;r.releaseLock();
    const marker={};throws(()=>new ReadableStream({start(){throw new TypeError('start')}}));
    const bad=new ReadableStream({start(){return Promise.reject(marker)}}).getReader();
    await rejects(bad.read(),marker);await rejects(bad.closed,marker);
"#,
    )
}
#[test]
fn errors_release_pending_reads_and_reacquire_locks() {
    check(
        r#"
    let c;const s=new ReadableStream({start(v){c=v}},{highWaterMark:0});const r=s.getReader();
    const p=r.read(),closed=r.closed;r.releaseLock();assert(!s.locked);
    await rejects(p,'TypeError');await rejects(closed,'TypeError');await rejects(r.read(),'TypeError');
    const second=new ReadableStreamDefaultReader(s);c.enqueue(9);eq(await second.read(),{value:9,done:false});
    const error={failure:true};const a=second.read(),b=second.read();c.error(error);
    await rejects(a,error);await rejects(b,error);await rejects(second.closed,error);eq(c.desiredSize,null);
    c.error('ignored');throws(()=>c.enqueue(1));second.releaseLock();
    const third=s.getReader();await rejects(third.read(),error);third.releaseLock();
"#,
    )
}
#[test]
fn cancel_closes_immediately_and_awaits_source_promise() {
    check(
        r#"
    let controller,finish,cancelReason,cancelCalls=0;
    const source={start(c){controller=c},cancel(reason){assert(this===source);cancelReason=reason;cancelCalls++;return new Promise(r=>finish=r)}};
    const s=new ReadableStream(source,{highWaterMark:0}),r=s.getReader(),reason={};
    await rejects(s.cancel(),'TypeError');const pending=r.read(),canceled=r.cancel(reason);
    await r.closed;eq(await pending,{done:true});assert(cancelReason===reason);assert(s.locked);
    let finished=false;canceled.then(()=>finished=true);await Promise.resolve();eq(finished,false);
    finish(99);eq(await canceled,undefined);eq(cancelCalls,1);r.releaseLock();
    await s.cancel('again');eq(cancelCalls,1);throws(()=>controller.enqueue('late'));
    const marker={};const bad=new ReadableStream({cancel(){throw marker}});await rejects(bad.cancel(),marker);
"#,
    )
}
#[test]
fn async_iteration_serializes_requests_and_releases_or_cancels() {
    check(
        r#"
    let next=0,reason;
    const s=new ReadableStream({pull(c){c.enqueue(++next);if(next===3)c.close()},cancel(v){reason=v}},{highWaterMark:0});
    const it=s.values();assert(s.locked);const a=it.next(),b=it.next(),c=it.next(),d=it.next();
    eq(await Promise.all([a,b,c,d]),[{value:1,done:false},{value:2,done:false},{value:3,done:false},{done:true}]);
    assert(!s.locked);eq(await it.next(),{done:true});eq(await it.return(7),{value:7,done:true});
    const s2=new ReadableStream({start(c){c.enqueue('a');c.enqueue('b')},cancel(v){reason=v}});
    const keep=s2.values({preventCancel:true});eq((await keep.next()).value,'a');await keep.return();assert(!s2.locked);eq(reason,undefined);
    const stop=s2[Symbol.asyncIterator]();eq(await stop.return('stop'),{value:'stop',done:true});eq(reason,'stop');assert(!s2.locked);
    const all=[];for await(const value of new ReadableStream({start(c){c.enqueue(4);c.close()}})){all.push(value)}eq(all,[4]);
"#,
    )
}
#[test]
fn tee_shares_default_chunks_and_combines_cancel_reasons() {
    check(
        r#"
    const value={shared:true};const s=new ReadableStream({start(c){c.enqueue(value);c.close()}});
    const [a,b]=s.tee();assert(s.locked);const ra=a.getReader(),rb=b.getReader();
    assert((await ra.read()).value===value);assert((await rb.read()).value===value);
    eq(await ra.read(),{done:true});eq(await rb.read(),{done:true});
    let reasons,calls=0;const pending=new ReadableStream({cancel(r){reasons=r;calls++}});
    const [left,right]=pending.tee(),r1={},r2={};let firstDone=false;
    const p1=left.cancel(r1);p1.then(()=>firstDone=true);await Promise.resolve();eq(firstDone,false);
    const p2=right.cancel(r2);await Promise.all([p1,p2]);eq(calls,1);assert(reasons[0]===r1&&reasons[1]===r2);
    let c;const source=new ReadableStream({start(controller){c=controller}}),[x,y]=source.tee();
    const canceled=x.cancel();c.close();const reader=y.getReader();eq(await reader.read(),{done:true});await canceled;
"#,
    )
}
#[test]
fn tee_forwards_errors_and_size_failures_preserve_identity() {
    check(
        r#"
    let c;const source=new ReadableStream({start(v){c=v}}),[a,b]=source.tee(),error={};
    const ra=a.getReader(),rb=b.getReader();c.error(error);
    await rejects(ra.read(),error);await rejects(rb.read(),error);await rejects(ra.closed,error);
    let controller;const marker={};const s=new ReadableStream({start(c){controller=c}},{size(){throw marker}});
    try{controller.enqueue(1);throw Error('missing')}catch(e){assert(e===marker)}
    await rejects(s.getReader().read(),marker);
    let invalid;const bad=new ReadableStream({start(c){invalid=c}},{size(){return NaN}});
    throws(()=>invalid.enqueue(1),'RangeError');await rejects(bad.getReader().read(),'RangeError');
"#,
    )
}
#[test]
fn native_byte_adapter_and_incremental_consumption() {
    let mut c = context();
    let bytes: Vec<u8> = (0..150_000).map(|i| (i % 251) as u8).collect();
    let stream = from_bytes(bytes.clone(), &mut c).unwrap();
    assert!(!is_disturbed(&stream.clone().into()).unwrap());
    let promise = consume_bytes(&stream.clone().into(), &mut c).unwrap();
    assert!(is_locked(&stream.clone().into()).unwrap());
    c.run_jobs().unwrap();
    let PromiseState::Fulfilled(value) = promise.state() else {
        panic!("byte consumption did not complete")
    };
    assert_eq!(copy_uint8_chunk(&value, &mut c).unwrap(), bytes);
    assert!(is_disturbed(&stream.clone().into()).unwrap());
    assert!(!is_locked(&stream.into()).unwrap());
    let (stream, controller) = new_readable(0.0, &mut c).unwrap();
    let consumer = consume_bytes(&stream.clone().into(), &mut c).unwrap();
    c.run_jobs().unwrap();
    assert!(matches!(consumer.state(), PromiseState::Pending));
    controller
        .enqueue(
            JsUint8Array::from_iter([1, 2], &mut c).unwrap().into(),
            &mut c,
        )
        .unwrap();
    c.run_jobs().unwrap();
    assert!(matches!(consumer.state(), PromiseState::Pending));
    controller
        .enqueue(JsUint8Array::from_iter([3], &mut c).unwrap().into(), &mut c)
        .unwrap();
    controller.close(&mut c).unwrap();
    c.run_jobs().unwrap();
    let PromiseState::Fulfilled(value) = consumer.state() else {
        panic!("host byte stream did not complete")
    };
    assert_eq!(copy_uint8_chunk(&value, &mut c).unwrap(), vec![1, 2, 3]);
}
#[test]
fn native_reactions_ignore_promise_species_and_constructor_hooks() {
    check(
        r#"
    const descriptor=Object.getOwnPropertyDescriptor(Promise,Symbol.species);let called=0;
    Object.defineProperty(Promise,Symbol.species,{configurable:true,get(){called++;throw Error('species should not run')}});
    try {
        let c;const s=new ReadableStream({start(controller){c=controller;return Promise.resolve()},pull(controller){controller.enqueue(1);controller.close()}});
        const r=s.getReader();eq(await r.read(),{value:1,done:false});await r.closed;r.releaseLock();
        const error={};const s2=new ReadableStream({start(controller){c=controller}},{highWaterMark:0}),r2=s2.getReader();
        Object.defineProperty(r2.closed,'constructor',{get(){throw Error('constructor should not run')}});
        c.error(error);await rejects(r2.read(),error);eq(called,0);
    } finally {Object.defineProperty(Promise,Symbol.species,descriptor);}
"#,
    )
}
#[test]
fn state_tracing_and_queue_bounds_do_not_fake_success() {
    let mut c = context();
    run(
        &mut c,
        r#"globalThis.reason={alive:1};globalThis.s=new ReadableStream({start(c){c.enqueue(reason);c.close()}});"#,
    );
    boa_gc::force_collect();
    run(
        &mut c,
        "assert((await s.getReader().read()).value===reason);",
    );
    run(
        &mut c,
        r#"
        let controller;const s=new ReadableStream({start(c){controller=c}},{highWaterMark:0,size(){return 0}});
        for(let i=0;i<10000;i++)controller.enqueue(i);
        throws(()=>controller.enqueue('overflow'),'RangeError');await rejects(s.getReader().read(),'RangeError');
        const source=new ReadableStream();const r=source.getReader();throws(()=>source.getReader());r.releaseLock();
        throws(()=>source.getReader({mode:'byob'}));await rejects(source.pipeTo({}),'TypeError');
    "#,
    );
}

#[path = "web_streams_byte_tests.rs"]
mod byte_tests;
