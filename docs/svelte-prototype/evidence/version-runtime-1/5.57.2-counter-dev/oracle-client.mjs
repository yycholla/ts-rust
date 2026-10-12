import 'svelte/internal/disclose-version';

Counter[$.FILENAME] = 'Counter.svelte';

import * as $ from 'svelte/internal/client';

var root = $.add_locations($.from_html(`<button type="button"> </button>`), Counter[$.FILENAME], [[6, 0]]);

export default function Counter($$anchor, $$props) {
	$.check_target(new.target);
	$.push($$props, true, Counter);

	let step = $.prop($$props, 'step', 3, 1),
		label = $.prop($$props, 'label', 3, 'Count');

	let count = $.tag($.state(0), 'count');
	let doubled = $.tag($.derived(() => $.get(count) * 2), 'doubled');
	var $$exports = { ...$.legacy_api() };
	var button = root();
	var text = $.only_child(button);

	$.template_effect(() => $.set_text(text, `${label() ?? ''}: ${$.get(count) ?? ''} / ${$.get(doubled) ?? ''}`));

	$.delegated('click', button, function click() {
		return $.set(count, $.get(count) + step());
	});

	$.append($$anchor, button);

	return $.pop($$exports);
}

$.delegate(['click']);