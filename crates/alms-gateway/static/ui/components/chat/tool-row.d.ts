import type { VNode } from "preact";

/**
 * Ambient declarations for the parts of `tool-row.js` reached from typed
 * tests. Mirrors `message.d.ts`: only the exports a test imports are
 * declared, because nothing else crosses from TypeScript into this module.
 */
export declare function renderParams(
  tool: string,
  params: Record<string, unknown>,
): VNode | null;
