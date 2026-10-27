import type { ChatGptViewV1, commands } from "../ipc/chatgpt-bindings";

type Commands = typeof commands;

export function chatgptView(profile: string, changes: Partial<ChatGptViewV1> = {}): ChatGptViewV1 {
  return {
    version: 1,
    profile,
    state: "signed_out",
    email: null,
    error: null,
    revocation_unconfirmed: false,
    presence: "missing",
    ...changes,
  };
}

/** Account tests never fall through to real OAuth or native credential storage. */
export function mockChatGptBindings(overrides: Partial<Commands> = {}): { commands: Commands } {
  return {
    commands: new Proxy(overrides as Commands, {
      get(target, key, receiver) {
        if (typeof key === "symbol" || key === "then") return undefined;
        if (!Object.hasOwn(target, key)) throw new Error(`Unmocked ChatGPT command: ${key}`);
        return Reflect.get(target, key, receiver);
      },
    }),
  };
}
