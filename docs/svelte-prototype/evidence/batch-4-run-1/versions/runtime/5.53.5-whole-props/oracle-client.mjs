import 'svelte/internal/disclose-version';
import * as $ from 'svelte/internal/client';

var root = $.from_html(`<button> </button>`);

export default function WholeProps($$anchor, $$props) {
	$.push($$props, true);

	let props = $.rest_props($$props, ['$$slots', '$$events', '$$legacy']);
	let count = $.state(0);
	var button = root();
	var event_handler = () => $.update(count);

	$.attribute_effect(button, () => ({ ...props, type: 'button', onclick: event_handler }));

	var text = $.child(button);

	$.reset(button);
	$.template_effect(($0) => $.set_text(text, `${$$props.named ?? ''}:${$$props.title ?? ''}:${'named' in props}:${$0 ?? ''}:${$.get(count) ?? ''}`), [() => Object.keys(props).sort().join(',')]);
	$.append($$anchor, button);
	$.pop();
}