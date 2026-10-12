import * as $ from 'svelte/internal/server';

export default function Typed_props($$renderer, $$props) {
	$$renderer.component(($$renderer) => {
		let { count, name = 'visitor' } = $$props;

		$.head('1l8o2ip', $$renderer, ($$renderer) => {
			$$renderer.title(($$renderer) => {
				$$renderer.push(`<title>Hello ${$.escape(name)}</title>`);
			});
		});

		$$renderer.push(`<p>${$.escape(name)}: ${$.escape(count.toFixed(0))}</p>`);
	});
}