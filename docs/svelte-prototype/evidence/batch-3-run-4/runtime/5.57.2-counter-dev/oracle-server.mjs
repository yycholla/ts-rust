Counter[$.FILENAME] = 'Counter.svelte';

import * as $ from 'svelte/internal/server';

function Counter($$renderer, $$props) {
	$$renderer.component(
		($$renderer) => {
			let { step = 1, label = 'Count' } = $$props;
			let count = 0;
			let doubled = $.derived(() => count * 2);

			$$renderer.push(`<button type="button">`);
			$.push_element($$renderer, 'button', 6, 0);
			$$renderer.push(`${$.escape(label)}: ${$.escape(count)} / ${$.escape(doubled())}</button>`);
			$.pop_element();
		},
		Counter
	);
}

Counter.render = function () {
	throw new Error('Component.render(...) is no longer valid in Svelte 5. See https://svelte.dev/docs/svelte/v5-migration-guide#Components-are-no-longer-classes for more information');
};

export default Counter;