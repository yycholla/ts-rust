import 'svelte/internal/disclose-version';
import * as $ from 'svelte/internal/client';

var rest_excludes = new Set(['$$slots', '$$events', '$$legacy', 'named']);
var root = $.from_html(`<button> </button>`);

export default function RestProps($$anchor, $$props) {
	$.push($$props, true);

	let rest = $.rest_props($$props, rest_excludes);
	let count = $.state(0);
	var button = root();
	var event_handler = () => $.update(count);

	$.attribute_effect(button, () => ({ ...rest, type: 'button', onclick: event_handler }));

	var text = $.child(button);

	$.reset(button);
	$.template_effect(($0) => $.set_text(text, `${$$props.named ?? ''}:${$$props.title ?? ''}:${'named' in rest}:${$0 ?? ''}:${$.get(count) ?? ''}`), [() => Object.keys(rest).sort().join(',')]);
	$.append($$anchor, button);
	$.pop();
}