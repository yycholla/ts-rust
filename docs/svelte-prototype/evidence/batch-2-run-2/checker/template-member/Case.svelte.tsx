                               
;
;type $$ComponentProps =  { count: number };function $$render() {

  let { count }:/*Ωignore_startΩ*/$$ComponentProps/*Ωignore_endΩ*/ = $props();
;
async () => {
 { svelteHTML.createElement("p", {});count.notARealMember(); }
};
return { props: {} as any as $$ComponentProps, exports: {}, bindings: __sveltets_$$bindings(''), slots: {}, events: {} }}
const Case__SvelteComponent_ = __sveltets_2_fn_component($$render());
/*Ωignore_startΩ*/type Case__SvelteComponent_ = ReturnType<typeof Case__SvelteComponent_>;
/*Ωignore_endΩ*/export default Case__SvelteComponent_;