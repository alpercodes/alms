import { h } from 'https://esm.sh/preact@10.24.3';
import htm from 'https://esm.sh/htm@3.1.1';
import { SessionList } from './session-list.js';
import { RunList } from './run-list.js';

const html = htm.bind(h);

export function Sidebar() {
    return html`
        <div id="sidebar">
            <${SessionList} />
            <${RunList} />
        </div>
    `;
}
