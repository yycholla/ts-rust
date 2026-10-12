use super::*;
use boa_engine::Source;
fn context() -> Context {
    let mut context = Context::builder()
        .instructions_remaining(10_000_000)
        .build()
        .unwrap();
    crate::web_text::register(&mut context).unwrap();
    register(&mut context).unwrap();
    context
}
fn run(context: &mut Context, body: &str) {
    let source = format!(
        r#"(()=>{{
        function eq(a,b){{if(JSON.stringify(a)!==JSON.stringify(b))throw Error(JSON.stringify({{actual:a,expected:b}}));}}
        function assert(value){{if(!value)throw Error('Assertion failed');}}
        function throws(fn,name='TypeError'){{try{{fn()}}catch(e){{eq(e.name,name);return e}}throw Error('Expected throw');}}
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
fn check(body: &str) {
    let mut context = context();
    run(&mut context, body);
    assert!(take_reported_errors(&mut context).is_empty());
}
#[test]
fn event_dispatch_phases_identity_cancellation_and_cleanup() {
    check(
        r#"
        const target=new EventTarget(),e=new Event('x',{bubbles:true,cancelable:true,composed:true});
        const calls=[];
        target.addEventListener('x',function(event){
            assert(this===target&&event===e&&event.target===target&&event.currentTarget===target);
            assert(event.composedPath()[0]===target);eq(event.eventPhase,Event.AT_TARGET);
            calls.push('bubble');event.preventDefault();
        });
        target.addEventListener('x',()=>calls.push('capture'),true);
        eq(target.dispatchEvent(e),false);eq(calls,['capture','bubble']);
        eq([e.currentTarget,e.eventPhase,e.defaultPrevented,e.isTrusted],[null,0,true,false]);
        assert(e.target===target);eq(e.composedPath(),[]);
        assert(e.timeStamp>=0);eq(Object.getOwnPropertyDescriptor(e,'isTrusted').configurable,false);
        e.initEvent('y');eq([e.type,e.target,e.bubbles,e.cancelable,e.defaultPrevented],['y',null,false,false,false]);
        throws(()=>new Event());throws(()=>target.dispatchEvent({}));
        throws(()=>Event.prototype.preventDefault.call({}));
    "#,
    );
}
#[test]
fn listeners_use_identity_capture_live_removal_and_phase_snapshots() {
    check(
        r#"
        const t=new EventTarget(),calls=[];
        function late(){calls.push('late')}
        function removed(){calls.push('removed')}
        function first(){calls.push('first');t.removeEventListener('x',removed);t.addEventListener('x',late)}
        t.addEventListener('x',first);t.addEventListener('x',first,{once:true});
        t.addEventListener('x',removed);t.dispatchEvent(new Event('x'));eq(calls,['first']);
        t.dispatchEvent(new Event('x'));eq(calls,['first','first','late']);
        const u=new EventTarget(),seen=[];
        const cb=()=>seen.push('same');u.addEventListener('x',cb,true);u.addEventListener('x',cb,false);
        u.removeEventListener('x',cb,true);u.dispatchEvent(new Event('x'));eq(seen,['same']);
        const v=new EventTarget(),order=[];
        v.addEventListener('x',()=>{order.push('capture');v.addEventListener('x',()=>order.push('added'))},true);
        v.dispatchEvent(new Event('x'));eq(order,['capture','added']);
        const receiver={handleEvent(e){assert(this===receiver);eq(e.type,'object');}};
        v.addEventListener('object',receiver);v.dispatchEvent(new Event('object'));
    "#,
    );
}
#[test]
fn once_passive_stops_and_reentrant_dispatch_follow_dom() {
    check(
        r#"
        const t=new EventTarget();let count=0;
        t.addEventListener('once',()=>{count++;t.dispatchEvent(new Event('once'))},{once:true});
        t.dispatchEvent(new Event('once'));eq(count,1);
        const e=new Event('passive',{cancelable:true});
        t.addEventListener('passive',event=>{event.preventDefault();event.returnValue=false},{passive:true});
        assert(t.dispatchEvent(e));assert(!e.defaultPrevented);
        const order=[];t.addEventListener('stop',event=>{order.push(1);event.stopImmediatePropagation()});
        t.addEventListener('stop',()=>order.push(2));t.dispatchEvent(new Event('stop'));eq(order,[1]);
        const u=new EventTarget(),a=[];
        u.addEventListener('x',event=>{a.push(1);event.stopPropagation()},true);
        u.addEventListener('x',()=>a.push(2),true);u.addEventListener('x',()=>a.push(3));
        u.dispatchEvent(new Event('x'));eq(a,[1,2]);
        t.addEventListener('reenter',event=>throws(()=>t.dispatchEvent(event),'InvalidStateError'));
        t.dispatchEvent(new Event('reenter'));
    "#,
    );
}
#[test]
fn abort_reason_identity_default_exception_and_signal_inheritance() {
    check(
        r#"
        const c=new AbortController(),s=c.signal,reason={value:42};
        assert(s instanceof AbortSignal&&s instanceof EventTarget);
        assert(Object.getPrototypeOf(AbortSignal)===EventTarget);
        eq([s.aborted,s.reason,s.onabort],[false,undefined,null]);s.throwIfAborted();
        const calls=[];s.onabort=function(e){assert(this===s);assert(e.isTrusted);calls.push('first')};
        s.addEventListener('abort',()=>calls.push('second'));
        s.onabort=()=>calls.push('replacement');
        c.abort(reason);c.abort('ignored');eq(calls,['replacement','second']);assert(s.reason===reason);
        try{s.throwIfAborted();throw Error('missing throw')}catch(e){assert(e===reason)}
        const d=new AbortController();d.abort();assert(d.signal.reason instanceof DOMException);
        eq([d.signal.reason.name,d.signal.reason.code],['AbortError',20]);
        assert(AbortSignal.abort(null).reason===null);assert(AbortSignal.abort().reason instanceof DOMException);
        throws(()=>new AbortSignal());throws(()=>AbortSignal.prototype.throwIfAborted.call({}));
        throws(()=>AbortSignal.timeout(1),'NotSupportedError');throws(()=>AbortSignal.timeout(-1));
    "#,
    );
}
#[test]
fn listener_abort_options_remove_before_handlers_and_ignore_aborted_sources() {
    check(
        r#"
        const c=new AbortController(),t=new EventTarget();let calls=0;
        t.addEventListener('x',()=>calls++,{signal:c.signal});
        c.signal.addEventListener('abort',()=>t.dispatchEvent(new Event('x')));
        t.dispatchEvent(new Event('x'));c.abort();t.dispatchEvent(new Event('x'));eq(calls,1);
        t.addEventListener('x',()=>calls++,{signal:c.signal});t.dispatchEvent(new Event('x'));eq(calls,1);
        throws(()=>t.addEventListener('x',null,{signal:null}));
        const a=new AbortController(),b=new AbortController();function f(){calls++}
        t.addEventListener('y',f,{signal:a.signal});t.addEventListener('y',f,{signal:b.signal});
        b.abort();t.dispatchEvent(new Event('y'));eq(calls,2);a.abort();t.dispatchEvent(new Event('y'));eq(calls,2);
    "#,
    );
}
#[test]
fn any_flattens_dependencies_marks_before_callbacks_and_converts_full_sequence() {
    check(
        r#"
        const a=new AbortController(),b=new AbortController(),reason={};
        const first=AbortSignal.any([a.signal,b.signal,a.signal]);
        const second=AbortSignal.any([first,b.signal]);const calls=[];
        a.signal.onabort=()=>{assert(first.aborted&&second.aborted);assert(second.reason===reason);calls.push('source')};
        first.onabort=()=>calls.push('first');second.onabort=()=>calls.push('second');
        a.abort(reason);b.abort('later');eq(calls,['source','first','second']);
        assert(first.reason===reason&&second.reason===reason);
        assert(AbortSignal.any([AbortSignal.abort('first'),AbortSignal.abort('second')]).reason==='first');
        assert(!AbortSignal.any([]).aborted);
        let visited=0;throws(()=>AbortSignal.any({*[Symbol.iterator](){visited++;yield AbortSignal.abort();visited++;yield {}}}));eq(visited,2);
        let closed=0;
        throws(()=>AbortSignal.any({[Symbol.iterator](){return {next(){return {value:{},done:false}},return(){closed++;return {done:true}}}}}));
        eq(closed,0); // Web IDL synchronous sequence conversion propagates element errors directly.
    "#,
    );
}
#[test]
fn listener_exceptions_are_reported_and_dispatch_continues() {
    let mut context = context();
    run(
        &mut context,
        r#"
        globalThis.thrown={marker:1};const t=new EventTarget();let count=0;
        t.addEventListener('x',()=>{throw thrown});t.addEventListener('x',()=>count++);
        assert(t.dispatchEvent(new Event('x')));eq(count,1);
    "#,
    );
    let errors = take_reported_errors(&mut context);
    assert_eq!(errors.len(), 1);
    let thrown = context
        .global_object()
        .get(JsString::from("thrown"), &mut context)
        .unwrap();
    assert!(JsObject::equals(
        &errors[0].as_object().unwrap(),
        &thrown.as_object().unwrap()
    ));
    assert!(take_reported_errors(&mut context).is_empty());
}
#[test]
fn native_signal_helpers_recursion_limits_and_gc_references() {
    let mut context = context();
    let signal = new_signal(&mut context).unwrap();
    assert!(abort_reason(&signal.clone().into()).unwrap().is_none());
    abort_signal(&signal, JsValue::from(7), &mut context).unwrap();
    assert_eq!(
        abort_reason(&signal.into()).unwrap().unwrap().as_number(),
        Some(7.0)
    );
    run(
        &mut context,
        r#"
        globalThis.controller=new AbortController();globalThis.s=AbortSignal.any([controller.signal]);
        globalThis.reason={retained:true};globalThis.observed=false;
        s.onabort=()=>{observed=s.reason===reason};
    "#,
    );
    boa_gc::force_collect();
    run(&mut context, "controller.abort(reason);assert(observed);");
    run(
        &mut context,
        r#"
        const t=new EventTarget();t.addEventListener('loop',()=>t.dispatchEvent(new Event('loop')));
        assert(t.dispatchEvent(new Event('loop')));
        let calls=0;t.addEventListener('ok',()=>calls++);t.dispatchEvent(new Event('ok'));eq(calls,1);
    "#,
    );
    let errors = take_reported_errors(&mut context);
    assert_eq!(errors.len(), 1);
    assert_eq!(
        errors[0]
            .as_object()
            .unwrap()
            .get(JsString::from("name"), &mut context)
            .unwrap()
            .to_string(&mut context)
            .unwrap()
            .to_std_string_lossy(),
        "RangeError"
    );
    register(&mut context).unwrap();
}

#[test]
fn native_dependents_ignore_mutable_hooks_and_mark_all_reasons_before_dispatch() {
    let mut c = context();
    run(
        &mut c,
        r#"
        globalThis.controller=new AbortController();globalThis.source=controller.signal;
        globalThis.calls=[];globalThis.reason={native:true};
        Object.defineProperty(source,'aborted',{get(){throw Error('public getter must not run')}});
        Object.defineProperty(source,'reason',{get(){throw Error('public getter must not run')}});
        source.addEventListener=()=>{throw Error('public method must not run')};
        globalThis.originalAny=AbortSignal.any;AbortSignal.any=()=>{throw Error('global helper must not run')};
    "#,
    );
    let source = c
        .global_object()
        .get(JsString::from("source"), &mut c)
        .unwrap();
    let source = validate_signal(&source).unwrap();
    let first = dependent_signal(&[source.clone(), source.clone()], &mut c).unwrap();
    let second = dependent_signal(std::slice::from_ref(&first), &mut c).unwrap();
    assert!(!JsObject::equals(&first, &source));
    assert!(!JsObject::equals(&first, &second));
    c.register_global_property(JsString::from("first"), first, Attribute::all())
        .unwrap();
    c.register_global_property(JsString::from("second"), second, Attribute::all())
        .unwrap();
    run(
        &mut c,
        r#"
        EventTarget.prototype.addEventListener.call(source,'abort',()=>{
            assert(first.aborted&&second.aborted);assert(first.reason===reason&&second.reason===reason);calls.push('source');
        });
        first.onabort=()=>calls.push('first');second.onabort=()=>calls.push('second');
        globalThis.combined=originalAny([second,first]);combined.onabort=()=>calls.push('combined');
    "#,
    );
    boa_gc::force_collect();
    run(
        &mut c,
        "controller.abort(reason);eq(calls,['source','first','second','combined']);assert(combined.reason===reason);",
    );
    assert!(take_reported_errors(&mut c).is_empty());
}

#[test]
fn native_dependents_preserve_already_aborted_order_and_validate_all_inputs() {
    let mut c = context();
    let first = new_signal(&mut c).unwrap();
    let second = new_signal(&mut c).unwrap();
    let a = c.eval(Source::from_bytes("({reason:'first'})")).unwrap();
    let b = c.eval(Source::from_bytes("({reason:'second'})")).unwrap();
    abort_signal(&first, a.clone(), &mut c).unwrap();
    abort_signal(&second, b.clone(), &mut c).unwrap();
    let result = dependent_signal(&[second.clone(), first.clone()], &mut c).unwrap();
    let reason = abort_reason(&result.clone().into()).unwrap().unwrap();
    assert!(JsObject::equals(
        &reason.as_object().unwrap(),
        &b.as_object().unwrap()
    ));
    assert!(!JsObject::equals(&result, &second));
    let invalid = c
        .eval(Source::from_bytes("({aborted:true,reason:'fake'})"))
        .unwrap();
    assert!(validate_signal(&invalid).is_err());
    assert!(dependent_signal(&[first, invalid.as_object().unwrap()], &mut c).is_err());
    let empty = dependent_signal(&[], &mut c).unwrap();
    assert!(abort_reason(&empty.into()).unwrap().is_none());
    let chained = dependent_signal(&[result], &mut c).unwrap();
    boa_gc::force_collect();
    assert!(JsObject::equals(
        &abort_reason(&chained.into())
            .unwrap()
            .unwrap()
            .as_object()
            .unwrap(),
        &b.as_object().unwrap()
    ));
    assert!(take_reported_errors(&mut c).is_empty());
}

fn algorithm_context() -> Context {
    let mut c = context();
    c.register_global_builtin_callable(
        JsString::from("addNative"),
        2,
        NativeFunction::from_fn_ptr(|_, args, c| {
            let signal = object(args.get_or_undefined(0))?;
            let callback = object(args.get_or_undefined(1))?;
            Ok(JsValue::from(
                add_abort_algorithm(&signal, callback, c)? as f64
            ))
        }),
    )
    .unwrap();
    c.register_global_builtin_callable(
        JsString::from("removeNative"),
        2,
        NativeFunction::from_fn_ptr(|_, args, c| {
            let signal = object(args.get_or_undefined(0))?;
            let token = args.get_or_undefined(1).to_number(c)? as u64;
            remove_abort_algorithm(&signal, token)?;
            Ok(JsValue::undefined())
        }),
    )
    .unwrap();
    c
}

#[test]
fn native_algorithms_interleave_listener_removal_before_uninterruptible_events() {
    let mut c = algorithm_context();
    run(
        &mut c,
        r#"
        const controller=new AbortController(), signal=controller.signal;
        const target=new EventTarget(), calls=[], reason={same:true};
        addNative(signal,function(value){
            'use strict';assert(this===undefined);assert(value===reason);
            calls.push('before');target.dispatchEvent(new Event('x'));
        });
        target.addEventListener('x',()=>calls.push('listener'),{signal});
        addNative(signal,value=>{
            assert(value===reason);calls.push('after');target.dispatchEvent(new Event('x'));
        });
        signal.addEventListener('abort',event=>{calls.push('event');event.stopImmediatePropagation()});
        signal.addEventListener('abort',()=>calls.push('blocked'));
        controller.abort(reason);
        eq(calls,['before','listener','after','event']);
        eq(addNative(signal,()=>{throw Error('already aborted')}),0);
        controller.abort({other:true});eq(calls,['before','listener','after','event']);
    "#,
    );
    assert!(take_reported_errors(&mut c).is_empty());
}

#[test]
fn native_algorithms_remove_future_steps_and_continue_after_exceptions() {
    let mut c = algorithm_context();
    run(
        &mut c,
        r#"
        globalThis.controller=new AbortController();globalThis.calls=[];
        globalThis.failure={algorithm:true};globalThis.eventFailure={event:true};
        let second;
        addNative(controller.signal,()=>{
            calls.push('first');removeNative(controller.signal,second);throw failure;
        });
        second=addNative(controller.signal,()=>calls.push('cancelled'));
        addNative(controller.signal,()=>calls.push('third'));
        controller.signal.onabort=()=>{calls.push('event');throw eventFailure};
        controller.abort();eq(calls,['first','third','event']);
        removeNative(controller.signal,second);removeNative(controller.signal,0);
    "#,
    );
    let errors = take_reported_errors(&mut c);
    assert_eq!(errors.len(), 2);
    for (error, name) in errors.iter().zip(["failure", "eventFailure"]) {
        let expected = c.global_object().get(JsString::from(name), &mut c).unwrap();
        assert!(JsObject::equals(
            &error.as_object().unwrap(),
            &expected.as_object().unwrap()
        ));
    }
    let controller = c
        .global_object()
        .get(JsString::from("controller"), &mut c)
        .unwrap()
        .as_object()
        .unwrap();
    let signal = controller
        .downcast_ref::<AbortController>()
        .unwrap()
        .signal
        .clone();
    assert!(
        super::signal(&signal.into())
            .unwrap()
            .borrow()
            .algorithms
            .is_empty()
    );
}

#[test]
fn native_algorithms_mark_dependents_before_callbacks_and_allow_cross_signal_removal() {
    let mut c = algorithm_context();
    run(
        &mut c,
        r#"
        globalThis.controller=new AbortController();globalThis.calls=[];
        globalThis.dependent=AbortSignal.any([controller.signal]);
        const reason={shared:true};
        const token=addNative(dependent,()=>calls.push('removed'));
        addNative(controller.signal,value=>{
            assert(dependent.aborted&&dependent.reason===value&&value===reason);
            removeNative(dependent,token);
            eq(addNative(dependent,()=>calls.push('too late')),0);
            controller.abort('reentrant');calls.push('source');
        });
        dependent.onabort=()=>calls.push('dependent event');
        controller.abort(reason);eq(calls,['source','dependent event']);
    "#,
    );
    assert!(take_reported_errors(&mut c).is_empty());
}

#[test]
fn native_algorithm_cleanup_gc_and_shared_resource_limits() {
    let mut c = algorithm_context();
    run(
        &mut c,
        r#"
        globalThis.controller=new AbortController();globalThis.calls=[];
        globalThis.target=new EventTarget();globalThis.listener=()=>{};
        globalThis.token=addNative(controller.signal,(()=>{
            const captured={retained:'yes'};return ()=>calls.push(captured.retained);
        })());
        target.addEventListener('x',listener,{signal:controller.signal});
        target.removeEventListener('x',listener);
    "#,
    );
    let controller = c
        .global_object()
        .get(JsString::from("controller"), &mut c)
        .unwrap()
        .as_object()
        .unwrap();
    let object = controller
        .downcast_ref::<AbortController>()
        .unwrap()
        .signal
        .clone();
    let state = signal(&object.clone().into()).unwrap();
    assert_eq!(state.borrow().algorithms.len(), 1);
    boa_gc::force_collect();
    run(&mut c, "controller.abort();eq(calls,['yes']);");
    assert!(state.borrow().algorithms.is_empty());

    let signal = new_signal(&mut c).unwrap();
    let callback = c
        .eval(Source::from_bytes("(()=>{})"))
        .unwrap()
        .as_object()
        .unwrap();
    let state = super::signal(&signal.clone().into()).unwrap();
    state.borrow_mut().next_algorithm_token = u64::MAX;
    assert!(add_abort_algorithm(&signal, callback.clone(), &mut c).is_err());
    assert!(state.borrow().algorithms.is_empty());
    state.borrow_mut().next_algorithm_token = 1;
    for expected in 1..=MAX_ITEMS {
        assert_eq!(
            add_abort_algorithm(&signal, callback.clone(), &mut c).unwrap(),
            expected as u64
        );
    }
    assert!(add_abort_algorithm(&signal, callback.clone(), &mut c).is_err());
    c.register_global_property(JsString::from("limited"), signal.clone(), Attribute::all())
        .unwrap();
    run(
        &mut c,
        "throws(()=>target.addEventListener('full',listener,{signal:limited}),'RangeError');",
    );
    remove_abort_algorithm(&signal, 1).unwrap();
    run(
        &mut c,
        "target.addEventListener('full',listener,{signal:limited});",
    );
    assert_eq!(state.borrow().algorithms.len(), MAX_ITEMS);
    assert!(add_abort_algorithm(&signal, callback, &mut c).is_err());
    run(&mut c, "target.removeEventListener('full',listener);");
    assert_eq!(state.borrow().algorithms.len(), MAX_ITEMS - 1);
    assert!(add_abort_algorithm(&signal, JsObject::with_null_proto(), &mut c).is_err());
    assert!(remove_abort_algorithm(&JsObject::with_null_proto(), 0).is_err());
    assert!(take_reported_errors(&mut c).is_empty());
}

#[test]
fn native_algorithm_error_reporting_exhaustion_still_finishes_abort_cleanup() {
    let mut c = algorithm_context();
    host(&c)
        .unwrap()
        .borrow_mut()
        .errors
        .resize(MAX_ITEMS, JsValue::null());
    run(
        &mut c,
        r#"
        const controller=new AbortController(),calls=[];
        addNative(controller.signal,()=>{throw Error('over capacity')});
        addNative(controller.signal,()=>calls.push('next'));
        controller.signal.onabort=()=>calls.push('event');
        throws(()=>controller.abort(),'RangeError');eq(calls,['next','event']);
    "#,
    );
    assert_eq!(take_reported_errors(&mut c).len(), MAX_ITEMS);
}
