import "$styles/global.css";
import { expect, test, vi } from "vitest";
import { render } from "vitest-browser-svelte";
import { page, userEvent } from "vitest/browser";
import type { WorkLocalServerV1, WorkMoreModelsV1_Serialize } from "$shared/ipc/bindings";
import { PROFILE, entry, models } from "$shared/testing/work-models";
import { emitNativeEvent } from "$shared/testing/native-events";
import AiPage from "../components/sections/AiPage.svelte";

vi.mock("$domain/tabs", () => ({
  tabs: { profile: () => ({ id: "00000000000000000000000001", kind: "regular" }) },
}));
const native = vi.hoisted(() => ({
  read: vi.fn(),
  set: vi.fn(),
  test: vi.fn(),
  choose: vi.fn(),
  account: vi.fn(),
  local: vi.fn(async () => [] as WorkLocalServerV1[]),
  endpoint: vi.fn(),
  more: vi.fn(async (): Promise<WorkMoreModelsV1_Serialize> => ({
    provider: "compatible",
    entries: [],
    fault: null,
  })),
}));
const go = vi.hoisted(() => ({ read: vi.fn() }));
vi.mock("$shared/ipc/go-bindings", async () => {
  const { goView, mockGoBindings } = await import("$shared/testing/opencode-go");
  return mockGoBindings({
    goStatus: async (profile) => {
      go.read(profile);
      return goView(profile);
    },
  });
});
const gateway = vi.hoisted(() => ({ read: vi.fn() }));
vi.mock("$shared/ipc/gateway-bindings", async () => {
  const { gatewayView, mockGatewayBindings } = await import("$shared/testing/gateway");
  return mockGatewayBindings({
    gatewayStatus: async (profile) => {
      gateway.read(profile);
      return gatewayView(profile);
    },
  });
});
vi.mock("$shared/ipc/chatgpt-bindings", async () => {
  const { chatgptView, mockChatGptBindings } = await import("$shared/testing/chatgpt");
  return mockChatGptBindings({ chatgptAccount: async (profile) => chatgptView(profile) });
});
vi.mock("$shared/ipc/bindings", async () => {
  const { mockBindings } = await import("$shared/testing/bindings");
  return mockBindings({
    workModels: native.read,
    workSetProviderKey: native.set,
    workTestProviderKey: native.test,
    workChooseModel: native.choose,
    accountGet: native.account,
    workLocalServers: native.local,
    workSetModelEndpoint: native.endpoint,
    workMoreModels: native.more,
    voiceStatus: async () => ({
      version: 1,
      engines: ["on_device", "open_ai"],
      chosen: null,
      effective: "on_device",
      language: null,
      languages: ["en-US"],
      on_device: "en_US",
      microphone: "granted",
    }),
    workDecisionPreference: async () => ({
      version: 1,
      profile: PROFILE,
      choice: "recommended",
      effective: "standard",
      typesafe_key_present: false,
      error: null,
    }),
  });
});

test("a refused key is explained and acceptance makes no billing promise", async () => {
  await page.viewport(1100, 1200);
  native.read.mockResolvedValue(models({ keys: { open_ai: "set" }, lead: "openai/gpt-6-sol" }));
  native.set
    .mockResolvedValueOnce({ ...models({ keys: { open_ai: "set" } }), fault: "key_refused" })
    .mockResolvedValueOnce(models({ keys: { open_ai: "set", anthropic: "valid" } }));
  const screen = await render(AiPage);

  await screen.getByRole("button", { name: "Add key" }).first().click();
  await userEvent.keyboard("sk-ant-wrong");
  await screen.getByRole("button", { name: "Save" }).click();
  expect(native.set).toHaveBeenCalledWith(PROFILE, "anthropic", "sk-ant-wrong");
  await expect.element(screen.getByText("Anthropic didn’t accept this key.")).toBeVisible();

  await screen.getByLabelText("Anthropic API key").fill("sk-ant-right");
  await screen.getByRole("button", { name: "Save" }).click();
  await expect.element(screen.getByText("Key accepted")).toBeVisible();
  await expect.element(screen.getByRole("button", { name: "Save" })).not.toBeInTheDocument();
});

test("testing a stored key reports what the provider said", async () => {
  await page.viewport(1100, 1200);
  native.read.mockResolvedValue(models({ keys: { open_ai: "set" } }));
  native.test.mockResolvedValue({ ...models({ keys: { open_ai: "set" } }), fault: "unreachable" });
  const screen = await render(AiPage);
  await screen.getByRole("button", { name: "Test" }).click();
  expect(native.test).toHaveBeenCalledWith(PROFILE, "open_ai");
  await expect
    .element(screen.getByText("Couldn’t reach OpenAI. Check your connection and try again."))
    .toBeVisible();
});

test("a role follows Automatic until the person chooses a model", async () => {
  await page.viewport(1100, 1200);
  native.read.mockResolvedValue(
    models({ keys: { anthropic: "valid" }, lead: "anthropic/claude-opus-5-5" }),
  );
  native.choose.mockResolvedValue(
    models({
      keys: { anthropic: "valid" },
      lead: "anthropic/claude-sonnet-5-5",
      chosen: "anthropic/claude-sonnet-5-5",
    }),
  );
  const screen = await render(AiPage);
  await expect.element(screen.getByText("Automatic · Claude Opus 5.5")).toBeVisible();
  await screen.getByText("Automatic · Claude Opus 5.5").click();
  await page.getByRole("option", { name: /Claude Sonnet 5.5/ }).click();
  expect(native.choose).toHaveBeenCalledWith(PROFILE, "lead", "anthropic/claude-sonnet-5-5");
});

test("billing trouble preserves acceptance and the old Cloud group stays gone", async () => {
  await page.viewport(1100, 1200);
  const view = models({ keys: { open_ai: "valid" }, cloud: true });
  view.providers = view.providers.map((provider) =>
    provider.provider === "open_ai" ? { ...provider, fault: "billing" } : provider,
  );
  native.read.mockResolvedValue(view);
  const screen = await render(AiPage);
  await expect.element(screen.getByText("Key accepted")).toBeVisible();
  await expect
    .element(
      screen.getByText("Check your provider’s billing or available credits, then try again."),
    )
    .toBeVisible();
  await expect.element(screen.getByText("Zephium Cloud", { exact: true })).not.toBeInTheDocument();
});

test("Settings AI exposes Jev setup beside the regular model controls with honest fallback", async () => {
  await page.viewport(1100, 1200);
  native.read.mockResolvedValue(models({ keys: { open_ai: "set" }, lead: "openai/gpt-6-sol" }));
  const screen = await render(AiPage);
  await expect.element(screen.getByRole("button", { name: "Add Jev key" })).toBeVisible();
  await expect.element(screen.getByRole("radio", { name: "Recommended" })).toBeChecked();
  await expect.element(screen.getByText("Active: Standard")).toBeVisible();
  await expect.element(screen.getByRole("heading", { name: "Work", exact: true })).toBeVisible();
});

test("Zephium AI is offered where the server hosts models, and shows the account once signed in", async () => {
  await page.viewport(1100, 1200);
  native.read.mockResolvedValue(models({}));
  const account = {
    version: 1,
    available: true,
    server: "https://api.zephium.app",
    custom_server: false,
    hosted_models: true,
    billing: true,
    signup_open: true,
    signed_in: false,
    pending_email: null,
    resend_in: null,
    email: null,
    name: null,
    plan: null,
    usage: null,
    subscription: null,
    offers: [],
    checkout_open: false,
    notice: null,
  };
  native.account.mockResolvedValue({ account, fault: null, retry_after: null });
  const screen = await render(AiPage);
  await expect.element(screen.getByRole("button", { name: "Sign in…" })).toBeVisible();
  await screen.unmount();

  native.read.mockResolvedValue(models({ cloud: true }));
  native.account.mockResolvedValue({
    account: {
      ...account,
      signed_in: true,
      email: "ada@example.com",
      plan: "free",
      usage: { used_percent: 30, resets_at: null, extra: false },
    },
    fault: null,
    retry_after: null,
  });
  const signed = await render(AiPage);
  await expect.element(signed.getByText("Signed in · Free plan · 30% used")).toBeVisible();
  await expect
    .element(signed.getByText("Add a provider key or sign in", { exact: false }))
    .not.toBeInTheDocument();
});

test("a model server already running here is one click away", async () => {
  await page.viewport(1100, 1400);
  native.read.mockResolvedValue(models({}));
  native.local.mockResolvedValue([
    { kind: "ollama", base: "http://localhost:11434/v1", models: 3 },
  ]);
  native.endpoint.mockResolvedValue(models({ base: "http://localhost:11434/v1" }));
  const screen = await render(AiPage);
  await screen.getByRole("button", { name: "Use Ollama on this computer" }).click();
  expect(native.endpoint).toHaveBeenCalledWith(PROFILE, "http://localhost:11434/v1");
  await expect
    .element(screen.getByText("Set OLLAMA_CONTEXT_LENGTH", { exact: false }))
    .toBeVisible();
});

test("a custom server is asked for its models once per address, even when it has none", async () => {
  await page.viewport(1100, 1200);
  native.more.mockReset();
  native.read.mockResolvedValue(models({ base: "http://localhost:11434/v1" }));
  for (const answer of [
    { provider: "compatible", entries: [], fault: null },
    { provider: "compatible", entries: [], fault: "unreachable" },
  ] satisfies WorkMoreModelsV1_Serialize[]) {
    native.more.mockReset();
    native.more.mockResolvedValue(answer);
    const screen = await render(AiPage);
    await expect.element(screen.getByText("http://localhost:11434/v1")).toBeVisible();
    await new Promise((settle) => setTimeout(settle, 400));
    expect(native.more).toHaveBeenCalledTimes(1);
    emitNativeEvent("workModelsChanged", { version: 1 });
    await new Promise((settle) => setTimeout(settle, 200));
    expect(native.more).toHaveBeenCalledTimes(1);
    screen.unmount();
  }
  native.more.mockReset();
  native.more.mockResolvedValue({ provider: "compatible", entries: [], fault: null });
  const screen = await render(AiPage);
  await expect.poll(() => native.more.mock.calls.length).toBe(1);
  native.read.mockResolvedValue(models({ base: "http://localhost:1234/v1" }));
  emitNativeEvent("workModelsChanged", { version: 1 });
  await expect.poll(() => native.more.mock.calls.length).toBe(2);
  screen.unmount();
});

test("legacy direct account models are not offered as a new Settings choice", async () => {
  const view = models({ keys: { open_ai: "valid" } });
  view.entries.push(
    entry("open_ai", "gpt-account", "GPT Account", false, ["lead"], {
      id: "chatgpt/gpt-account",
      price: null,
    }),
  );
  native.read.mockResolvedValue(view);
  const screen = await render(AiPage);
  await screen.getByRole("button", { name: "Work", exact: true }).click();
  expect(page.getByRole("option", { name: /GPT Account/u }).query()).toBeNull();
});

test("a disconnected ChatGPT choice stays visible instead of silently resetting", async () => {
  native.read.mockResolvedValue(
    models({
      keys: { open_ai: "valid" },
      chosen: "chatgpt/gpt-account",
      lead: "openai/gpt-6-sol",
    }),
  );
  const screen = await render(AiPage);
  await expect
    .element(
      screen.getByText(
        "This saved direct connection is retired. Select Codex for sidebar chat or choose another configured source. Your saved account and history are unchanged.",
      ),
    )
    .toBeVisible();
});

test("secondary account guidance loads on demand and remains keyboard-dismissible", async () => {
  native.read.mockResolvedValue(models({}));
  const screen = await render(AiPage);
  const summary = screen.getByText("Claude, Antigravity, and OpenCode options", { exact: true });
  await expect
    .element(screen.getByRole("heading", { name: "Other subscription tools" }))
    .not.toBeInTheDocument();
  summary.element().focus();
  await userEvent.keyboard("{Enter}");
  await expect
    .element(screen.getByRole("heading", { name: "Other subscription tools" }))
    .toBeVisible();
  await userEvent.keyboard("{Enter}");
  await expect
    .element(screen.getByRole("heading", { name: "Other subscription tools" }))
    .not.toBeInTheDocument();
  await userEvent.keyboard("{Enter}");
  await expect.element(screen.getByRole("heading", { name: "Google Antigravity" })).toBeVisible();
});

test("gateway Settings loads on explicit disclosure and can close and reopen by keyboard", async () => {
  native.read.mockResolvedValue(models({}));
  gateway.read.mockClear();
  const screen = await render(AiPage);
  const summary = screen.getByText("Local gateway", { exact: true });
  expect(gateway.read).not.toHaveBeenCalled();
  summary.element().focus();
  await userEvent.keyboard("{Enter}");
  await expect.element(screen.getByRole("button", { name: "Set up local gateway" })).toBeVisible();
  expect(gateway.read).toHaveBeenCalledExactlyOnceWith(PROFILE);
  await userEvent.keyboard("{Enter}");
  await expect
    .element(screen.getByRole("button", { name: "Set up local gateway" }))
    .not.toBeInTheDocument();
  await userEvent.keyboard("{Enter}");
  await expect.element(screen.getByRole("button", { name: "Set up local gateway" })).toBeVisible();
  expect(gateway.read).toHaveBeenCalledTimes(2);
});

test("native gateway models are selectable without a custom endpoint and source stays outside the model name", async () => {
  const view = models({});
  view.entries.push(
    entry("compatible", "example", "Example model", false, ["lead"], {
      id: `gateway/${"a".repeat(32)}/example`,
      price: null,
      supports: {
        tools: true,
        vision: false,
        native_search: false,
        reasoning: false,
        prompt_cache: false,
      },
    }),
  );
  native.read.mockResolvedValue(view);
  const screen = await render(AiPage);
  await screen.getByRole("button", { name: "Work", exact: true }).click();
  await expect
    .element(page.getByRole("option", { name: "Example model", exact: true }))
    .toHaveAccessibleDescription("Local gateway");
});

test("OpenCode Go is a lazy independent section, not another generic API-key row", async () => {
  native.read.mockResolvedValue(models({}));
  go.read.mockClear();
  const screen = await render(AiPage);
  expect(go.read).not.toHaveBeenCalled();
  const summary = screen.getByText("OpenCode Go", { exact: true });
  summary.element().focus();
  await userEvent.keyboard("{Enter}");
  await expect.element(screen.getByRole("button", { name: "Set up OpenCode Go" })).toBeVisible();
  expect(go.read).toHaveBeenCalledExactlyOnceWith(PROFILE);
  await userEvent.keyboard("{Enter}");
  await expect
    .element(screen.getByRole("button", { name: "Set up OpenCode Go" }))
    .not.toBeInTheDocument();
});

test("Go role choices use native catalog entries and keep provider/source outside the name", async () => {
  const view = models({});
  view.entries.push(
    entry("open_code_go", "example", "Example", false, ["lead"], {
      id: `opencode-go/${"a".repeat(32)}/example`,
      price: null,
      supports: {
        tools: true,
        vision: false,
        native_search: false,
        reasoning: false,
        prompt_cache: false,
      },
    }),
  );
  native.read.mockResolvedValue(view);
  const screen = await render(AiPage);
  await screen.getByRole("button", { name: "Work", exact: true }).click();
  await expect
    .element(page.getByRole("option", { name: "Example", exact: true }))
    .toHaveAccessibleDescription("OpenCode Go");
});
