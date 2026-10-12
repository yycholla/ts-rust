import 'svelte/internal/disclose-version';
import * as $ from 'svelte/internal/client';

var root = $.from_html(`<select aria-label="Choice"><option>First</option><option>Second</option></select>`);

export default function DefaultSelect($$anchor, $$props) {
	let choice = $.prop($$props, 'choice', 3, 'second');
	var select = root();
	var option = $.child(select);

	option.value = option.__value = 'first';

	var option_1 = $.sibling(option);

	option_1.value = option_1.__value = 'second';
	$.reset(select);
	$.template_effect(() => select.defaultValue = choice());
	$.append($$anchor, select);
}