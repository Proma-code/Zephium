import type { GatewayViewV1, commands } from "../ipc/gateway-bindings";
type Commands = typeof commands;

export function gatewayView(profile: string, changes: Partial<GatewayViewV1> = {}): GatewayViewV1 {
  return {
    version: 1,
    profile,
    state: "unconfigured",
    connection_id: null,
    label: null,
    base: null,
    models: 0,
    checking: false,
    checked_at_ms: null,
    error: null,
    ...changes,
  };
}

/** No fixture may fall through to a real gateway or protected credential store. */
export function mockGatewayBindings(overrides: Partial<Commands> = {}): { commands: Commands } {
  return {
    commands: new Proxy(overrides as Commands, {
      get(target, key, receiver) {
        if (typeof key === "symbol" || key === "then") return undefined;
        if (!Object.hasOwn(target, key)) throw new Error(`Unmocked gateway command: ${key}`);
        return Reflect.get(target, key, receiver);
      },
    }),
  };
}
