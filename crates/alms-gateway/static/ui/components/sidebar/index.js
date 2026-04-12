import { html } from '../../deps.js';
import { SessionList } from './session-list.js';
import { sidebarOpen, closeSidebar } from '../header.js';

export function Sidebar() {
    const openClass = sidebarOpen.value ? ' sidebar-open' : '';
    return html`
        ${sidebarOpen.value && html`<div class="sidebar-backdrop" onClick=${closeSidebar}></div>`}
        <div id="sidebar" class=${openClass}>
            <${SessionList} />
        </div>
    `;
}
