import * as $ from 'svelte/internal/server';

export default function Counter($$renderer, $$props) {
	let { step = 1, label = 'Count' } = $$props;
	let count = 0;
	let doubled = count * 2;

	$$renderer.push(`<button type="button">${$.escape(label)}: ${$.escape(count)} / ${$.escape(doubled)}</button>`);
}