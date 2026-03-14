import { html } from '../../deps.js';
import { SessionList } from './session-list.js';
import { RunList } from './run-list.js';

export function Sidebar() {
    return html`
        <div id="sidebar">
            <${SessionList} />
            <${RunList} />
        </div>
    `;
}
