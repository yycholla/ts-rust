import * as $ from 'svelte/internal/server';

export default function RestProps($$renderer, $$props) {
	$$renderer.component(($$renderer) => {
		let { named, $$slots, $$events, ...rest } = $$props;
		let count = 0;

		$$renderer.push(`<button${$.attributes({ ...rest, type: 'button' })}>${$.escape(named)}:${$.escape(rest.title)}:${$.escape('named' in rest)}:${$.escape(Object.keys(rest).sort().join(','))}:${$.escape(count)}</button>`);
	});
}