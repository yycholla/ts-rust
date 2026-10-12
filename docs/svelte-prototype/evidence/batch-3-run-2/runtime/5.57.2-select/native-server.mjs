import * as $ from 'svelte/internal/server';

export default function DefaultSelect($$renderer, $$props) {
	let { choice = 'second' } = $$props;

	$$renderer.select({ defaultValue: choice, 'aria-label': 'Choice' }, ($$renderer) => {
		$$renderer.option({ value: 'first' }, ($$renderer) => {
			$$renderer.push(`First`);
		});

		$$renderer.option({ value: 'second' }, ($$renderer) => {
			$$renderer.push(`Second`);
		});
	});
}