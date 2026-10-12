import * as $ from 'svelte/internal/server';

export default function WholeProps($$renderer, $$props) {
	$$renderer.component(($$renderer) => {
		let { $$slots, $$events, ...props } = $$props;
		let count = 0;

		$$renderer.push(`<button${$.attributes({ ...props, type: 'button' })}>${$.escape(props.named)}:${$.escape(props.title)}:${$.escape('named' in props)}:${$.escape(Object.keys(props).sort().join(','))}:${$.escape(count)}</button>`);
	});
}