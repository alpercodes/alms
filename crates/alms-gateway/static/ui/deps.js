// Central dependency re-exports — pin versions in one place.
import { h, render } from 'https://esm.sh/preact@10.24.3';
import { signal, computed, effect, batch } from 'https://esm.sh/@preact/signals@1.3.0';
import htm from 'https://esm.sh/htm@3.1.1';

const html = htm.bind(h);

export { h, render, signal, computed, effect, batch, html };
