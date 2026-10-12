import 'svelte/internal/disclose-version';
import * as $ from 'svelte/internal/client';

var root = $.from_html(`<button type="button"> </button>`);

export default function Counter($$anchor, $$props) {
	let step = $.prop($$props, 'step', 3, 1),
		label = $.prop($$props, 'label', 3, 'Count');

	let count = $.state(0);
	let doubled = $.derived(() => $.get(count) * 2);
	var button = root();
	var text = $.child(button);

	$.reset(button);
	$.template_effect(() => $.set_text(text, `${label() ?? ''}: ${$.get(count) ?? ''} / ${$.get(doubled) ?? ''}`));
	$.delegated('click', button, () => $.set(count, $.get(count) + step()));
	$.append($$anchor, button);
}

$.delegate(['click']);