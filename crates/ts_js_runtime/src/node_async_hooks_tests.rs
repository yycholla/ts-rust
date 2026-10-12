use super::*;
use boa_engine::{
    Source,
    builtins::promise::PromiseState,
    module::{ModuleLoader, ModuleRequest, Referrer},
    object::builtins::JsPromise,
};
use std::cell::RefCell;

#[derive(Default)]
struct Loader(RefCell<Option<Module>>);
impl ModuleLoader for Loader {
    async fn load_imported_module(
        self: Rc<Self>,
        _: Referrer,
        request: ModuleRequest,
        _: &RefCell<&mut Context>,
    ) -> JsResult<Module> {
        let name = request.specifier().to_std_string_escaped();
        if name != "node:async_hooks" && name != "async_hooks" {
            return Err(typ("Unexpected test module"));
        }
        self.0
            .borrow()
            .clone()
            .ok_or_else(|| typ("Uninitialized test builtin"))
    }
}
fn context() -> Context {
    let hooks = Rc::new(AsyncContextHooks);
    let loader = Rc::new(Loader::default());
    let mut context = Context::builder()
        .host_hooks(hooks.clone())
        .module_loader(loader.clone())
        .instructions_remaining(10_000_000)
        .build()
        .unwrap();
    let module = register(&mut context, hooks).unwrap();
    loader.0.replace(Some(module.clone()));
    let promise = module.load_link_evaluate(&mut context);
    context.run_jobs().unwrap();
    assert!(matches!(promise.state(), PromiseState::Fulfilled(_)));
    assert!(
        context
            .global_object()
            .get(JsString::from("AsyncLocalStorage"), &mut context)
            .unwrap()
            .is_undefined()
    );
    let constructor = module
        .namespace(&mut context)
        .get(JsString::from("AsyncLocalStorage"), &mut context)
        .unwrap();
    context
        .register_global_property(JsString::from("ALS"), constructor, Attribute::all())
        .unwrap();
    context
}
fn check(body: &str) {
    let mut context = context();
    let source = format!(
        r#"(async()=>{{
        function eq(a,b) {{ if (JSON.stringify(a)!==JSON.stringify(b)) throw new Error(JSON.stringify({{actual:a,expected:b}})); }}
        function ok(value) {{ if (!value) throw new Error('assertion'); }}
        function throws(fn) {{try{{fn()}}catch(e){{if(e instanceof TypeError)return;throw e}}throw new Error('Expected TypeError')}}
        {body}
        return true;
    }})()"#
    );
    let value = context.eval(Source::from_bytes(&source)).unwrap();
    let promise = JsPromise::from_object(value.as_object().unwrap()).unwrap();
    context.run_jobs().unwrap();
    match promise.state() {
        PromiseState::Fulfilled(value) => assert_eq!(value.as_boolean(), Some(true)),
        PromiseState::Rejected(value) => panic!(
            "Async context rejection: {}",
            value
                .to_string(&mut context)
                .unwrap()
                .to_std_string_escaped()
        ),
        PromiseState::Pending => panic!("Async context test remained pending"),
    }
}
#[test]
fn module_exports_are_native_and_aliases_share_identity() {
    check(
        r#"
        const a = await import('node:async_hooks');
        const b = await import('async_hooks');
        ok(a === b); ok(a.AsyncLocalStorage === ALS); ok(a.default.AsyncLocalStorage === ALS);
        ok(!('AsyncResource' in a)); ok(!('AsyncLocalStorage' in globalThis));
        throws(()=>ALS()); throws(()=>new ALS({defaultValue: 4}));
        throws(()=>ALS.prototype.getStore.call({})); throws(()=>ALS.prototype.run.call({},1,()=>{}));
    "#,
    );
}
#[test]
fn run_restores_identity_arguments_and_exceptions() {
    check(
        r#"
        const a = new ALS(), b = new ALS(), store = {value:1};
        eq(a.getStore(),undefined);
        const sentinel = new Error('sentinel');
        const result = a.run(store, (x,y)=>{
            ok(a.getStore() === store); eq([x,y],[3,4]);
            eq(a.run(null,()=>a.getStore()),null);
            ok(a.getStore() === store);
            try { a.run('throw',()=>{throw sentinel}) } catch(e) {ok(e===sentinel)}
            ok(a.getStore() === store);
            b.enterWith('persists');
            return 7;
        },3,4);
        eq(result,7); eq(a.getStore(),undefined); eq(b.getStore(),'persists');
        throws(()=>a.run(1,{})); eq(a.getStore(),undefined);
    "#,
    );
}
#[test]
fn concurrent_await_and_promise_reactions_keep_registration_context() {
    check(
        r#"
        const a = new ALS(); let resolve;
        const gate = new Promise(r=>resolve=r);
        const left = a.run('left',async()=>{await gate; eq(a.getStore(),'left'); await Promise.resolve(); return a.getStore()});
        const right = a.run('right',async()=>{await gate; eq(a.getStore(),'right'); await 0; return a.getStore()});
        const then = a.run('then',()=>gate.then(()=>a.getStore()));
        a.run('resolver',()=>resolve());
        eq(await Promise.all([left,right,then]),['left','right','then']);
        eq(a.getStore(),undefined);
    "#,
    );
}
#[test]
fn rejected_await_finally_and_thenable_resume_in_context() {
    check(
        r#"
        const a = new ALS();
        const result = await a.run(19,async()=>{
            try { await Promise.reject('rejection') } catch(e) {eq(e,'rejection');eq(a.getStore(),19)}
            finally {eq(a.getStore(),19)}
            const value = await {then(resolve){eq(a.getStore(),19);resolve('thenable')}};
            eq(a.getStore(),19);return value;
        });
        eq(result,'thenable'); eq(a.getStore(),undefined);
    "#,
    );
}
#[test]
fn exit_and_disable_do_not_propagate_old_stores_into_new_jobs() {
    check(
        r#"
        const a = new ALS();
        await a.run('outer',async()=>{
            const outside = a.exit(async()=>{eq(a.getStore(),undefined);await 0;return a.getStore()});
            eq(a.getStore(),'outer');eq(await outside,undefined);eq(a.getStore(),'outer');
        });
        a.enterWith('old'); const old = Promise.resolve().then(()=>a.getStore());
        a.disable(); eq(a.getStore(),undefined); eq(await old,undefined);
        a.enterWith(undefined); eq(a.getStore(),undefined);
        eq(a.run(null,()=>a.getStore()),null);eq(a.getStore(),undefined);
        a.enterWith('new');eq(await Promise.resolve().then(()=>a.getStore()),'new');
    "#,
    );
}
#[test]
fn bind_and_snapshot_capture_all_stores_and_preserve_this() {
    check(
        r#"
        const a = new ALS(), b = new ALS();
        const receiver = {tag:'receiver'};
        const bound = a.run('a',()=>b.run('b',()=>ALS.bind(function(x,y){return [this.tag,a.getStore(),b.getStore(),x+y]})));
        eq(bound.length,2);
        eq(a.run('other',()=>bound.call(receiver,2,3)),['receiver','a','b',5]);
        const snap = a.run('snapshot',()=>ALS.snapshot());
        eq(snap((x)=>[a.getStore(),b.getStore(),x],8),['snapshot',undefined,8]);
        eq(a.getStore(),undefined);throws(()=>ALS.bind(0));throws(()=>snap(null));
        const fn = snap(async()=>{await 0;return a.getStore()}); eq(await fn,'snapshot');
    "#,
    );
}
#[test]
fn async_generator_awaits_preserve_context() {
    check(
        r#"
        const a = new ALS();
        async function* generate(){await 0;yield a.getStore();await 0;return a.getStore()}
        const iterator = generate();
        eq(await a.run('first',()=>iterator.next()),{value:'first',done:false});
        eq(await a.run('second',()=>iterator.next()),{value:'second',done:true});
        eq(a.getStore(),undefined);
    "#,
    );
}
#[test]
fn native_scheduled_callback_boundary_restores_context_on_error() {
    let mut context = context();
    context
        .eval(Source::from_bytes(
            "globalThis.storage=new ALS();storage.enterWith('scheduled');",
        ))
        .unwrap();
    let scheduled = capture(&context).unwrap();
    context
        .eval(Source::from_bytes("storage.enterWith('dispatcher');"))
        .unwrap();
    let result: JsResult<()> = scheduled.run(&mut context, |context| {
        assert_eq!(context.eval(Source::from_bytes("storage.getStore()")).unwrap().as_string().unwrap().to_std_string_escaped(), "scheduled");
        context.eval(Source::from_bytes("globalThis.child=Promise.resolve().then(()=>storage.getStore());throw new Error('callback');"))?;
        Ok(())
    });
    assert!(result.is_err());
    assert_eq!(
        context
            .eval(Source::from_bytes("storage.getStore()"))
            .unwrap()
            .as_string()
            .unwrap()
            .to_std_string_escaped(),
        "dispatcher"
    );
    context.run_jobs().unwrap();
    let child = context.eval(Source::from_bytes("child")).unwrap();
    let child = JsPromise::from_object(child.as_object().unwrap()).unwrap();
    let PromiseState::Fulfilled(value) = child.state() else {
        panic!("Native child did not fulfill")
    };
    assert_eq!(
        value.as_string().unwrap().to_std_string_escaped(),
        "scheduled"
    );
    let mut other = self::context();
    assert!(scheduled.run(&mut other, |_| Ok(())).is_err());
}
#[test]
fn registration_rejects_uninstalled_hooks() {
    let mut context = Context::builder()
        .instructions_remaining(1000)
        .build()
        .unwrap();
    assert!(register(&mut context, Rc::new(AsyncContextHooks)).is_err());
}
