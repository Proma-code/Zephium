import type { GoViewV1, commands } from "../ipc/go-bindings";
type Commands = typeof commands;
export function goView(profile: string, changes: Partial<GoViewV1> = {}): GoViewV1 {
  return {
    version: 1,
    profile,
    state: "unconfigured",
    connection_id: null,
    models: 0,
    checking: false,
    catalog_checked_at_ms: null,
    last_billing: null,
    error: null,
    ...changes,
  };
}
/** Strict synthetic IPC: never invokes the provider or a real credential store. */
export function mockGoBindings(overrides: Partial<Commands> = {}): { commands: Commands } {
  return {
    commands: new Proxy(overrides as Commands, {
      get(target, key, receiver) {
        if (typeof key === "symbol" || key === "then") return undefined;
        if (!Object.hasOwn(target, key)) throw new Error(`Unmocked OpenCode Go command: ${key}`);
        return Reflect.get(target, key, receiver);
      },
    }),
  };
}
