import * as $ from 'svelte/internal/server';

function greeting($$renderer, value) {
	$$renderer.push(`<strong>Hello ${$.escape(value)}</strong>`);
}

export default function Snippets($$renderer, $$props) {
	let { label = 'world' } = $$props;

	$$renderer.push(`<section>`);
	greeting($$renderer, label);
	$$renderer.push(`<!----></section>`);
}