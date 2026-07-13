const bridge = globalThis.__almsState;

if (!bridge || bridge.version !== 1) {
    throw new Error('Normalized frontend state bridge is not installed');
}

export const entityState = bridge;
