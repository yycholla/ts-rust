import * as $ from 'svelte/internal/server';

export default function DefaultSelect($$renderer, $$props) {
	let { choice = 'second' } = $$props;

	$$renderer.push(`<select aria-label="Choice">`);

	$$renderer.option({ value: 'first' }, ($$renderer) => {
		$$renderer.push(`First`);
	});

	$$renderer.option({ value: 'second' }, ($$renderer) => {
		$$renderer.push(`Second`);
	});

	$$renderer.push(`</select>`);
}