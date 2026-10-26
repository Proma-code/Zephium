import "$styles/global.css";
import { beforeEach, expect, test, vi } from "vitest";
import { render } from "vitest-browser-svelte";
import { page, userEvent } from "vitest/browser";
import type { WorkModelReasoning, WorkModelsReadyV1 } from "$shared/ipc/bindings";
import { preferences } from "$domain/preferences";
import ModelPicker from "../components/bar/ModelPicker.svelte";
import ModelBarStage from "./ModelBarStage.svelte";
import { PROFILE, entry, models } from "$shared/testing/work-models";
import { gatewayView } from "$shared/testing/gateway";
import { chatgptView } from "$shared/testing/chatgpt";
import type { ChatGptViewV1 } from "$shared/ipc/chatgpt-bindings";
import { emitNativeEvent } from "$shared/testing/native-events";
import { accountFixture } from "$shared/testing/account";

const native = vi.hoisted(() => ({
  read: vi.fn(),
  choose: vi.fn(),
  effort: vi.fn(),
  more: vi.fn(),
  account: vi.fn(),
  openUrl: vi.fn(),
  command: vi.fn(async () => ({ operation_id: "op", accepted: true })),
  settings: {} as Record<string, string>,
  ready: vi.fn(async (profile: string): Promise<WorkModelsReadyV1> => ({
    version: 1,
    profile,
    ready: true,
    missing_roles: [],
    fault: null,
  })),
}));
vi.mock("$shared/ipc/bindings", async () => {
  const { mockBindings } = await import("$shared/testing/bindings");
  return mockBindings({
    workModels: native.read,
    workChooseModel: native.choose,
    workSetModelEffort: native.effort,
    workMoreModels: native.more,
    workModelsReady: native.ready,
    accountGet: native.account,
    browserOpenUrl: native.openUrl,
    runCommand: native.command,
    settingGet: async (key: string) => native.settings[key] ?? null,
    settingSet: async (key: string, value: string) => {
      native.settings[key] = value;
      return { accepted: true, operation_id: null };
    },
  });
});

const chatgpt = vi.hoisted(() => ({
  read: vi.fn(),
  signIn: vi.fn(),
  cancel: vi.fn(),
  signOut: vi.fn(),
  models: vi.fn(),
}));
vi.mock("$shared/ipc/chatgpt-bindings", async () => {
  const { mockChatGptBindings } = await import("$shared/testing/chatgpt");
  return mockChatGptBindings({
    chatgptAccount: chatgpt.read,
    chatgptSignIn: chatgpt.signIn,
    chatgptCancel: chatgpt.cancel,
    chatgptSignOut: chatgpt.signOut,
    chatgptRefreshModels: chatgpt.models,
  });
});
const gateway = vi.hoisted(() => ({ read: vi.fn(), check: vi.fn(), disconnect: vi.fn() }));
vi.mock("$shared/ipc/gateway-bindings", async () => {
  const { mockGatewayBindings } = await import("$shared/testing/gateway");
  return mockGatewayBindings({
    gatewayStatus: gateway.read,
    gatewayCheck: gateway.check,
    gatewayDisconnect: gateway.disconnect,
  });
});
beforeEach(() => {
  for (const command of Object.values(gateway)) command.mockReset();
  gateway.read.mockResolvedValue(gatewayView(PROFILE));
  gateway.disconnect.mockResolvedValue(gatewayView(PROFILE));
  native.openUrl.mockReset();
  native.openUrl.mockRejectedValue(new Error("mock browser unavailable"));
  for (const command of Object.values(chatgpt)) command.mockReset();
  chatgpt.read.mockResolvedValue(chatgptView(PROFILE));
  chatgpt.signIn.mockImplementation(async (_profile: string, attempt: string) =>
    chatgptView(PROFILE, { state: "signing_in", login_attempt: attempt }),
  );
  chatgpt.cancel.mockResolvedValue(chatgptView(PROFILE));
  chatgpt.signOut.mockResolvedValue(chatgptView(PROFILE));
});

const view = (lead: string, chosen: string | null = null, effort?: WorkModelReasoning) =>
  models({
    keys: { anthropic: "valid", open_ai: "set", open_router: "valid" },
    lead,
    chosen,
    effort,
  });

const ready = (value: boolean) => async (profile: string) => ({
  version: 1,
  profile,
  ready: value,
  missing_roles: value ? [] : (["lead"] as const).slice(),
  fault: null,
});

test("the trigger is the model's mark and words, and the menu changes the model", async () => {
  await page.viewport(1200, 800);
  native.ready.mockImplementation(ready(true));
  native.read.mockResolvedValue(view("anthropic/claude-opus-5-5"));
  native.choose.mockResolvedValue(view("openai/gpt-6-sol", "openai/gpt-6-sol"));
  const screen = await render(ModelPicker, { profile: PROFILE, onsettings: vi.fn() });

  const trigger = screen.getByRole("button", { name: "Model: Claude Opus 5.5, Medium effort" });
  await expect.element(trigger).toHaveTextContent(/Opus 5\.5\s*·\s*Medium/u);
  expect(trigger.element().querySelector("svg")).not.toBeNull();
  await trigger.click();

  await expect.element(page.getByRole("listbox")).toHaveFocus();
  const card = page.getByRole("region", { name: "Claude Opus 5.5" });
  await expect.element(card).toHaveTextContent(/Deepest reasoning/u);
  await expect
    .element(page.getByRole("option", { name: /Claude Opus 5\.5/u }))
    .toHaveAttribute("aria-selected", "true");
  await expect
    .element(page.getByRole("tab", { name: "Anthropic" }))
    .toHaveAttribute("aria-selected", "true");
  // Only the running model's provider is listed until another is chosen.
  expect(page.getByRole("option", { name: /GPT-6 Sol/u }).query()).toBeNull();

  await page.getByRole("tab", { name: "OpenAI" }).click();
  await page.getByRole("option", { name: /GPT-6 Sol/u }).click();
  expect(native.choose).toHaveBeenCalledWith(PROFILE, "lead", "openai/gpt-6-sol");
  await expect.element(screen.getByRole("button", { name: /Model: GPT-6 Sol/u })).toBeVisible();
});

test("the rail holds every provider, each with its own state", async () => {
  await page.viewport(1200, 800);
  native.ready.mockImplementation(ready(true));
  native.read.mockResolvedValue(view("anthropic/claude-opus-5-5"));
  const screen = await render(ModelPicker, { profile: PROFILE, onsettings: vi.fn() });
  await screen.getByRole("button", { name: /Model: Claude Opus 5\.5/u }).click();

  const names = page
    .getByRole("tab")
    .all()
    .map((tab) => tab.element().getAttribute("aria-label"));
  expect(names).toEqual([
    "Anthropic",
    "OpenAI",
    "OpenRouter",
    "Google, needs a key",
    "xAI, needs a key",
    "Mistral, needs a key",
    "DeepSeek, needs a key",
    "Groq, needs a key",
    "Cerebras, needs a key",
    "Custom server, connect a server",
  ]);
});

test("a provider without a key leads to adding one", async () => {
  await page.viewport(1200, 800);
  native.ready.mockImplementation(ready(true));
  native.read.mockResolvedValue(view("anthropic/claude-opus-5-5"));
  const onsettings = vi.fn();
  const screen = await render(ModelPicker, { profile: PROFILE, onsettings });
  await screen.getByRole("button", { name: /Model: Claude Opus 5\.5/u }).click();

  await page.getByRole("tab", { name: "Google, needs a key" }).click();
  await expect.element(page.getByText("Add your Google key", { exact: false })).toBeVisible();
  expect(page.getByRole("option").query()).toBeNull();
  await page.getByRole("button", { name: "Add key" }).click();
  expect(onsettings).toHaveBeenCalledTimes(1);

  await screen.getByRole("button", { name: /Model: Claude Opus 5\.5/u }).click();
  await page.getByRole("tab", { name: "Custom server, connect a server" }).click();
  await expect
    .element(page.getByText("Connect a server of your own", { exact: false }))
    .toBeVisible();
  document.querySelector<HTMLElement>(".model-menu [role=tabpanel]")!.focus();
  await userEvent.keyboard("{Enter}");
  expect(onsettings).toHaveBeenCalledTimes(2);
});

test("the header opens Settings, and no foot row asks for a key", async () => {
  await page.viewport(1200, 800);
  native.ready.mockImplementation(ready(true));
  native.read.mockResolvedValue(view("anthropic/claude-opus-5-5"));
  const onsettings = vi.fn();
  const screen = await render(ModelPicker, { profile: PROFILE, onsettings });
  await screen.getByRole("button", { name: /Model: Claude Opus 5\.5/u }).click();
  expect(page.getByRole("button", { name: /Add a key/u }).query()).toBeNull();
  await page.getByRole("button", { name: "Models and keys" }).click();
  expect(onsettings).toHaveBeenCalledTimes(1);
});

test("a provider whose key is refused says so", async () => {
  await page.viewport(1200, 800);
  native.ready.mockImplementation(ready(true));
  native.read.mockResolvedValue(
    models({ keys: { anthropic: "valid", google: "invalid" }, lead: "anthropic/claude-opus-5-5" }),
  );
  const screen = await render(ModelPicker, { profile: PROFILE, onsettings: vi.fn() });
  await screen.getByRole("button", { name: /Model: Claude Opus 5\.5/u }).click();
  await page.getByRole("tab", { name: "Google, key refused" }).click();
  await expect.element(page.getByText("Google refused the key", { exact: false })).toBeVisible();
});

test("the rail moves with the arrow keys, and arrowing the list moves the row Enter takes", async () => {
  await page.viewport(1200, 800);
  native.choose.mockClear();
  native.ready.mockImplementation(ready(true));
  native.read.mockResolvedValue(view("anthropic/claude-opus-5-5"));
  native.choose.mockResolvedValue(
    view("anthropic/claude-sonnet-5-5", "anthropic/claude-sonnet-5-5"),
  );
  const screen = await render(ModelPicker, { profile: PROFILE, onsettings: vi.fn() });
  await screen.getByRole("button", { name: /Model: Claude Opus 5\.5/u }).click();
  const list = page.getByRole("listbox");
  await expect.element(list).toHaveFocus();

  const anthropic = page.getByRole("tab", { name: "Anthropic" });
  (anthropic.element() as HTMLElement).focus();
  await userEvent.keyboard("{ArrowDown}");
  await expect.element(page.getByRole("tab", { name: "OpenAI" })).toHaveFocus();
  await expect.element(page.getByRole("option", { name: /GPT-6 Sol/u })).toBeVisible();
  await userEvent.keyboard("{ArrowUp}");
  await expect.element(anthropic).toHaveFocus();
  await expect.element(page.getByRole("option", { name: /Claude Sonnet 5\.5/u })).toBeVisible();

  (list.element() as HTMLElement).focus();
  await userEvent.keyboard("{ArrowDown}");
  const sonnet = page.getByRole("option", { name: /Claude Sonnet 5\.5/u });
  await expect.element(list).toHaveAttribute("aria-activedescendant", sonnet.element().id);
  expect(native.choose).not.toHaveBeenCalled();
  await userEvent.keyboard("{Enter}");
  expect(native.choose).toHaveBeenCalledWith(PROFILE, "lead", "anthropic/claude-sonnet-5-5");
});

test("typing slides the search in over every provider, Enter takes the first match, Escape puts it away", async () => {
  await page.viewport(1200, 800);
  native.ready.mockImplementation(ready(true));
  native.read.mockResolvedValue(view("openai/gpt-6-luna"));
  native.choose.mockResolvedValue(
    view("anthropic/claude-sonnet-5-5", "anthropic/claude-sonnet-5-5"),
  );
  const screen = await render(ModelPicker, { profile: PROFILE, onsettings: vi.fn() });
  await screen.getByRole("button", { name: /Model: GPT-6 Luna/u }).click();

  await userEvent.keyboard("sonnet");
  const field = page.getByRole("combobox", { name: "Search models" });
  await expect.element(field).toHaveValue("sonnet");
  await expect.poll(() => page.getByRole("option").all().length).toBe(2);
  await expect.element(page.getByRole("group", { name: "Anthropic" })).toBeVisible();
  await expect.element(page.getByRole("group", { name: "OpenRouter" })).toBeVisible();

  await userEvent.keyboard("{Escape}");
  await expect.element(field).toHaveValue("");
  await expect
    .element(page.getByRole("button", { name: "Search models" }))
    .toHaveAttribute("aria-expanded", "false");
  await expect.element(page.getByRole("option", { name: /GPT-6 Astra/u })).toBeVisible();

  await userEvent.keyboard("sonnet{Enter}");
  expect(native.choose).toHaveBeenCalledWith(PROFILE, "lead", "anthropic/claude-sonnet-5-5");
});

test("the clear button empties the search and returns to the provider's list", async () => {
  await page.viewport(1200, 800);
  native.ready.mockImplementation(ready(true));
  native.read.mockResolvedValue(view("openai/gpt-6-luna"));
  const screen = await render(ModelPicker, { profile: PROFILE, onsettings: vi.fn() });
  await screen.getByRole("button", { name: /Model: GPT-6 Luna/u }).click();
  await userEvent.keyboard("zzz");
  await expect.element(page.getByText("No model matches")).toBeVisible();
  await page.getByRole("button", { name: "Clear search" }).click();
  await expect.element(page.getByRole("combobox", { name: "Search models" })).toHaveValue("");
  await expect.element(page.getByRole("option", { name: /GPT-6 Sol/u })).toBeVisible();
});

test("the effort shows in a word, and the card turns into its control to set it", async () => {
  await page.viewport(1200, 800);
  native.ready.mockImplementation(ready(true));
  native.read.mockResolvedValue(view("openai/gpt-6-luna"));
  native.effort.mockImplementation(async (_profile: string, effort: WorkModelReasoning) =>
    view("openai/gpt-6-luna", null, effort),
  );
  const screen = await render(ModelPicker, { profile: PROFILE, onsettings: vi.fn() });
  await screen.getByRole("button", { name: /Model: GPT-6 Luna, Medium effort/u }).click();

  // At rest the card is the model, with its effort in a word; the control is not built.
  const pill = page.getByRole("button", { name: "Effort: Medium" });
  await expect.element(pill).toHaveAttribute("aria-expanded", "false");
  expect(page.getByRole("slider").query()).toBeNull();
  expect(document.querySelector(".model-menu .core")).toBeNull();

  await pill.click();
  const effort = page.getByRole("slider", { name: "Effort" });
  await expect.element(effort).toHaveFocus();
  await expect.element(effort).toHaveAttribute("aria-valuemax", "4");
  await expect.element(effort).toHaveAttribute("aria-valuetext", "Medium");
  // The model is never shown beside it.
  expect(document.querySelector(".model-menu .face.model")?.hasAttribute("inert")).toBe(true);
  const pace = () =>
    document
      .querySelector(".model-menu .core")!
      .getAnimations({ subtree: true })
      .map((run) => run.playbackRate)[0];
  await expect.poll(pace).toBeGreaterThan(0);
  const calm = pace()!;

  await userEvent.keyboard("{End}");
  await expect.element(effort).toHaveAttribute("aria-valuetext", "Max");
  expect(native.effort).toHaveBeenLastCalledWith(PROFILE, "max");
  await expect.poll(pace).toBeGreaterThan(calm);
  await expect
    .element(screen.getByRole("button", { name: /Model: GPT-6 Luna, Max effort/u }))
    .toBeInTheDocument();

  // Used, it turns back to the model and hands the keys to the button that opened it.
  await expect.element(page.getByRole("button", { name: "Effort: Max" })).toHaveFocus();
  expect(document.querySelector(".model-menu .face.effort")?.hasAttribute("inert")).toBe(true);
});

test("Escape turns the open effort back first, and only then closes the menu", async () => {
  await page.viewport(1200, 800);
  native.ready.mockImplementation(ready(true));
  native.read.mockResolvedValue(view("openai/gpt-6-luna"));
  const screen = await render(ModelPicker, { profile: PROFILE, onsettings: vi.fn() });
  const trigger = screen.getByRole("button", { name: /Model: GPT-6 Luna/u });
  await trigger.click();
  await page.getByRole("button", { name: "Effort: Medium" }).click();
  await expect.element(page.getByRole("slider", { name: "Effort" })).toBeVisible();
  await userEvent.keyboard("{Escape}");
  await expect
    .element(page.getByRole("button", { name: "Effort: Medium" }))
    .toHaveAttribute("aria-expanded", "false");
  await expect.element(page.getByRole("region", { name: "GPT-6 Luna" })).toBeVisible();
  await userEvent.keyboard("{Escape}");
  await expect.element(page.getByRole("region", { name: "GPT-6 Luna" })).not.toBeInTheDocument();
});

test("a star on a row keeps the model under Favourites, across providers", async () => {
  await page.viewport(1200, 800);
  native.ready.mockImplementation(ready(true));
  native.read.mockResolvedValue(view("anthropic/claude-opus-5-5"));
  await preferences.init();
  const screen = await render(ModelPicker, { profile: PROFILE, onsettings: vi.fn() });
  await screen.getByRole("button", { name: /Model: Claude Opus 5\.5/u }).click();
  expect(page.getByRole("tab", { name: "Favourites" }).query()).toBeNull();
  const favourites = page.getByRole("button", { name: "Favourites", exact: true });
  await expect.element(favourites).toHaveAttribute("aria-pressed", "false");

  await page.getByRole("button", { name: "Add Claude Sonnet 5.5 to favourites" }).click();
  await page.getByRole("tab", { name: "OpenAI" }).click();
  await page.getByRole("button", { name: "Add GPT-6 Sol to favourites" }).click();
  await favourites.click();
  await expect.element(favourites).toHaveAttribute("aria-pressed", "true");
  await expect.poll(() => page.getByRole("option").all().length).toBe(2);
  await expect.element(page.getByRole("group", { name: "Anthropic" })).toBeVisible();
  await expect.element(page.getByRole("group", { name: "OpenAI" })).toBeVisible();

  await page.getByRole("button", { name: "Remove GPT-6 Sol from favourites" }).click();
  await expect.poll(() => page.getByRole("option").all().length).toBe(1);
  await expect
    .poll(() => native.settings["work.favourite-models"])
    .toBe("anthropic/claude-sonnet-5-5");

  // The same button goes back to the provider that was open.
  await favourites.click();
  await expect.element(favourites).toHaveAttribute("aria-pressed", "false");
  await expect
    .element(page.getByRole("tab", { name: "OpenAI" }))
    .toHaveAttribute("aria-selected", "true");
  preferences.dispose();
});

test("a row shows no tier word, and its check stands at the end where the star takes its place", async () => {
  await page.viewport(1200, 800);
  native.ready.mockImplementation(ready(true));
  native.read.mockResolvedValue(view("anthropic/claude-opus-5-5"));
  const screen = await render(ModelPicker, { profile: PROFILE, onsettings: vi.fn() });
  await screen.getByRole("button", { name: /Model: Claude Opus 5\.5/u }).click();
  const list = document.querySelector(".model-menu [role=listbox]")!;
  expect(list.textContent).not.toMatch(/Flagship|Balanced|Lightweight/u);
  const row = list.querySelector('[aria-selected="true"]')!;
  const slot = row.querySelector(".slot")!;
  expect(slot.querySelector(".check svg")).not.toBeNull();
  const edge = (element: Element) => element.getBoundingClientRect().right;
  expect(edge(slot)).toBeGreaterThan(edge(row.querySelector(".option-name")!));
  expect(row.firstElementChild?.classList.contains("option-name")).toBe(true);
});

test("an empty search puts itself away when the menu is clicked elsewhere", async () => {
  await page.viewport(1200, 800);
  native.ready.mockImplementation(ready(true));
  native.read.mockResolvedValue(view("anthropic/claude-opus-5-5"));
  const screen = await render(ModelPicker, { profile: PROFILE, onsettings: vi.fn() });
  await screen.getByRole("button", { name: /Model: Claude Opus 5\.5/u }).click();
  const search = page.getByRole("button", { name: "Search models" });
  await search.click();
  await expect.element(search).toHaveAttribute("aria-expanded", "true");
  await page.getByRole("region", { name: "Claude Opus 5.5" }).click();
  await expect.element(search).toHaveAttribute("aria-expanded", "false");

  // With words in it, the search stays until it is cleared or a place is picked.
  await search.click();
  await userEvent.keyboard("sol");
  await page.getByRole("region", { name: "Claude Opus 5.5" }).click();
  await expect.element(page.getByRole("combobox", { name: "Search models" })).toHaveValue("sol");
  await page.getByRole("tab", { name: "OpenAI" }).click();
  await expect.element(search).toHaveAttribute("aria-expanded", "false");
  await expect.element(page.getByRole("combobox", { name: "Search models" })).toHaveValue("");
});

test("the effort control is set by a click and a drag, and turns back after a rest, a click elsewhere or Escape", async () => {
  await page.viewport(1200, 800);
  native.ready.mockImplementation(ready(true));
  native.read.mockResolvedValue(view("openai/gpt-6-luna"));
  native.effort.mockImplementation(async (_profile: string, effort: WorkModelReasoning) => {
    await new Promise((done) => setTimeout(done, 30));
    return view("openai/gpt-6-luna", null, effort);
  });
  const screen = await render(ModelPicker, { profile: PROFILE, onsettings: vi.fn() });
  await screen.getByRole("button", { name: /Model: GPT-6 Luna/u }).click();
  const pill = () => page.getByRole("button", { name: /^Effort:/u });
  const slider = page.getByRole("slider", { name: "Effort" });
  const asked = native.effort.mock.calls.length;
  const open = async () => {
    await pill().click();
    await expect.element(slider).toBeVisible();
  };

  await open();
  await slider.click({ position: { x: 96, y: 18 } });
  await expect.element(slider).toHaveAttribute("aria-valuetext", "Max");
  await slider.click({ position: { x: 6, y: 18 } });
  await expect.element(slider).toHaveAttribute("aria-valuetext", "Low");
  expect(native.effort.mock.calls.slice(asked).map((call) => call[1])).toEqual(["max", "low"]);

  // Dragged from the thumb, it follows the pointer and lands on the stop it is let go at.
  const thumb = page.elementLocator(document.querySelector(".model-menu .thumb")!);
  await userEvent.dragAndDrop(thumb, slider, { targetPosition: { x: 52, y: 18 } });
  await expect.element(slider).toHaveAttribute("aria-valuetext", "High");
  expect(document.querySelector(".model-menu .thumb")).not.toBeNull();

  // Used, it turns back a moment after the last change.
  await expect
    .poll(() => pill().element().getAttribute("aria-expanded"), { timeout: 4000 })
    .toBe("false");

  // Left alone, it turns back after a rest.
  const opened = performance.now();
  await open();
  await new Promise((done) => setTimeout(done, 1500));
  await expect.element(pill()).toHaveAttribute("aria-expanded", "true");
  await expect
    .poll(() => pill().element().getAttribute("aria-expanded"), { timeout: 4000 })
    .toBe("false");
  expect(performance.now() - opened).toBeGreaterThan(2500);

  // A click elsewhere in the menu puts it away at once.
  await open();
  await page.getByRole("tab", { name: "Anthropic" }).click();
  await expect.element(pill()).toHaveAttribute("aria-expanded", "false");
});

test("a model whose wire sets no effort shows no effort control", async () => {
  await page.viewport(1200, 800);
  native.ready.mockImplementation(ready(true));
  const plain = models({ keys: { mistral: "valid" }, lead: "mistral/mistral-medium-3-5" });
  plain.entries.push({
    ...plain.entries[0]!,
    id: "mistral/mistral-medium-3-5",
    model: { provider: "mistral", wire: "chat_completions", model: "mistral-medium-3-5" },
    display_name: "Mistral Medium 3.5",
  });
  native.read.mockResolvedValue(plain);
  const screen = await render(ModelPicker, { profile: PROFILE, onsettings: vi.fn() });
  await screen.getByRole("button", { name: "Model: Mistral Medium 3.5" }).click();
  await expect
    .element(page.getByRole("option", { name: /Mistral Medium 3\.5/u }))
    .toHaveAttribute("aria-selected", "true");
  expect(page.getByRole("slider").query()).toBeNull();
  await expect.element(page.getByText("Its own pace")).toBeVisible();
});

test("without any key the control opens provider connection choices, not an API-only Settings detour", async () => {
  await page.viewport(1200, 800);
  native.read.mockResolvedValue(models({}));
  native.ready.mockImplementation(ready(false));
  const onsettings = vi.fn();
  const screen = await render(ModelPicker, { profile: PROFILE, onsettings });
  await screen.getByRole("button", { name: "Connect a model" }).click();
  await page.getByRole("tab", { name: "OpenAI, choose a connection" }).click();
  await expect.element(page.getByRole("button", { name: "API key", exact: true })).toBeVisible();
  await expect.element(page.getByRole("button", { name: "ChatGPT", exact: true })).toBeVisible();
  expect(onsettings).not.toHaveBeenCalled();
  expect(chatgpt.signIn).not.toHaveBeenCalled();
});

test("signed out where hosted models exist, Zephium AI is one sign-in away", async () => {
  await page.viewport(1200, 800);
  native.read.mockResolvedValue(view("anthropic/claude-opus-5-5"));
  native.account.mockResolvedValue({
    account: {
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
    },
    fault: null,
    retry_after: null,
  });
  native.ready.mockImplementation(ready(false));
  const keyless = await render(ModelPicker, { profile: PROFILE });
  await expect.element(keyless.getByRole("button", { name: "Connect a model" })).toBeVisible();
  await keyless.unmount();

  native.ready.mockImplementation(ready(true));
  const screen = await render(ModelPicker, { profile: PROFILE });
  await screen.getByRole("button", { name: /Model: Claude Opus 5\.5/u }).click();
  // Zephium's own is pinned above every provider, ready or not.
  const rail = page.getByRole("tab").all();
  expect(rail[0]!.element().getAttribute("aria-label")).toBe("Zephium AI, sign in to use");
  expect(rail[1]!.element().getAttribute("aria-label")).toBe("Anthropic");
  await page.getByRole("tab", { name: "Zephium AI, sign in to use" }).click();
  await expect.element(page.getByText("hosted models", { exact: false })).toBeVisible();
  await page.getByRole("button", { name: "Sign in" }).click();
  expect(native.command).toHaveBeenCalledWith("settings.account");
});

test("the menu says so when no chosen model can search the web", async () => {
  await page.viewport(1200, 800);
  native.read.mockResolvedValue(
    models({ keys: { deep_seek: "valid" }, lead: "deepseek/deepseek-v4-pro" }),
  );
  native.ready.mockImplementation(async (profile: string) => ({
    version: 1,
    profile,
    ready: true,
    missing_roles: [],
    search: "unavailable",
    fault: null,
  }));
  const screen = await render(ModelPicker, { profile: PROFILE });
  await screen.getByRole("button", { name: /Model: DeepSeek V4 Pro/u }).click();
  await expect.element(page.getByText("Web search is off", { exact: false })).toBeVisible();
});

test("choosing a model leaves the composer open and the cursor in the words", async () => {
  await page.viewport(1200, 800);
  native.ready.mockImplementation(ready(true));
  native.read.mockResolvedValue(view("anthropic/claude-opus-5-5"));
  native.choose.mockResolvedValue(view("openai/gpt-6-sol", "openai/gpt-6-sol"));
  const screen = await render(ModelBarStage, { profile: PROFILE });
  const bar = () => screen.container.querySelector<HTMLElement>(".work-bar")!;
  const words = screen.getByRole("textbox");
  await words.click();
  await words.fill("Plan the trip");
  await screen.getByRole("button", { name: /Model: Claude Opus 5\.5/u }).click();
  await page.getByRole("tab", { name: "OpenAI" }).click();
  await page.getByRole("option", { name: /GPT-6 Sol/u }).click();
  await expect.element(page.getByRole("listbox")).not.toBeInTheDocument();
  await expect.element(words).toHaveFocus();
  expect(bar().dataset.engaged).toBe("true");
  expect(bar().dataset.mode).toBe("focus");
  await expect.element(words).toHaveValue("Plan the trip");
});

test("closing the menu from the chip leaves an empty composer engaged and focused", async () => {
  await page.viewport(1200, 800);
  native.ready.mockImplementation(ready(true));
  native.read.mockResolvedValue(view("anthropic/claude-opus-5-5"));
  const screen = await render(ModelBarStage, { profile: PROFILE });
  const bar = () => screen.container.querySelector<HTMLElement>(".work-bar")!;
  const words = screen.getByRole("textbox");
  await words.click();
  const chip = screen.getByRole("button", { name: /Model: Claude Opus 5\.5/u });
  await chip.click();
  await expect.element(page.getByRole("listbox")).toBeVisible();
  await chip.click();
  await expect.element(page.getByRole("listbox")).not.toBeInTheDocument();
  await expect.element(words).toHaveFocus();
  await new Promise((done) => setTimeout(done, 100));
  expect(bar().dataset.mode).toBe("focus");
});

test("a press on the words while the menu is open closes it and puts the cursor there", async () => {
  await page.viewport(1200, 800);
  native.ready.mockImplementation(ready(true));
  native.read.mockResolvedValue(view("anthropic/claude-opus-5-5"));
  const screen = await render(ModelBarStage, { profile: PROFILE });
  const words = screen.getByRole("textbox");
  await words.click();
  await screen.getByRole("button", { name: /Model: Claude Opus 5\.5/u }).click();
  await expect.element(page.getByRole("listbox")).toBeVisible();
  await words.click();
  await expect.element(page.getByRole("listbox")).not.toBeInTheDocument();
  await expect.element(words).toHaveFocus();
});

test("a hosted model the plan leaves out names the plan that opens it and leads to Account", async () => {
  await page.viewport(1200, 800);
  native.ready.mockImplementation(ready(true));
  native.command.mockClear();
  native.choose.mockClear();
  const hosted = models({
    cloud: true,
    lead: "zephium/claude-sonnet-5-5",
    chosen: "zephium/claude-sonnet-5-5",
  });
  const max = entry("cloud", "claude-opus-5-5", "Claude Opus 5.5", true, ["lead"]);
  const other = entry("cloud", "claude-fable-5-1", "Claude Fable 5.1", true, ["lead"]);
  native.read.mockResolvedValue({
    ...hosted,
    entries: [...hosted.entries, max, other],
    cloud: {
      ...hosted.cloud,
      locked: [max.id, other.id],
      required: [{ id: max.id, plan: "max" }],
    },
  });
  native.account.mockResolvedValue({
    account: accountFixture({ signed_in: true, plan: "pro" }),
    fault: null,
    retry_after: null,
  });
  const screen = await render(ModelPicker, { profile: PROFILE, onsettings: vi.fn() });
  await screen.getByRole("button", { name: /Model: Claude Sonnet 5\.5/u }).click();

  // The plan the server names is the badge; one it does not name says Upgrade.
  const row = page.getByRole("option", { name: /Claude Opus 5\.5 needs the Max plan/u });
  await expect.element(row).toHaveTextContent(/Claude Opus 5\.5\s*Max/u);
  const plain = page.getByRole("option", { name: /Claude Fable 5\.1 needs a higher plan/u });
  await expect.element(plain).toHaveTextContent(/Claude Fable 5\.1\s*Upgrade/u);
  expect(page.getByText("Not in your plan").query()).toBeNull();

  await row.click();
  expect(native.choose).not.toHaveBeenCalled();
  expect(native.command).toHaveBeenCalledWith("settings.account");
});

test("ChatGPT models have a usable picker seat without an OpenAI API key", async () => {
  native.ready.mockImplementation(ready(true));
  native.account.mockResolvedValue({ account: accountFixture(), fault: null, retry_after: null });
  const accountModel = entry("open_ai", "gpt-account", "GPT Account", false, ["lead"], {
    id: "chatgpt/gpt-account",
    price: null,
  });
  const accountView = models({});
  accountView.entries.push(accountModel);
  accountView.effective.lead = accountModel.id;
  native.read.mockResolvedValue(accountView);
  const screen = await render(ModelPicker, { profile: PROFILE, onsettings: vi.fn() });
  await screen.getByRole("button", { name: "Model: GPT Account" }).click();
  await expect.element(page.getByRole("tab", { name: "OpenAI", exact: true })).toBeVisible();
  await expect.element(page.getByRole("option", { name: /GPT Account/u })).toBeVisible();
  await expect.element(page.getByRole("option", { name: /GPT-6 Sol/u })).not.toBeInTheDocument();
});

function bothConnections() {
  const value = view("openai/gpt-6-luna");
  value.entries.push(
    entry("open_ai", "gpt-6-luna", "GPT-6 Luna", false, ["lead", "page"], {
      id: "chatgpt/gpt-6-luna",
      price: null,
    }),
  );
  return value;
}

test("one OpenAI provider keeps clean model names and separates source catalogs outside their titles", async () => {
  native.ready.mockImplementation(ready(true));
  native.read.mockResolvedValue(bothConnections());
  native.account.mockResolvedValue({ account: accountFixture(), fault: null, retry_after: null });
  native.choose.mockClear();
  chatgpt.read.mockResolvedValue(chatgptView(PROFILE, { state: "connected", models: 1 }));
  const screen = await render(ModelPicker, { profile: PROFILE });
  const trigger = screen.getByRole("button", { name: /Model: GPT-6 Luna/u });
  await trigger.click();
  expect(page.getByRole("tab", { name: "OpenAI", exact: true }).all()).toHaveLength(1);
  await expect
    .element(page.getByRole("option", { name: /GPT-6 Luna/u }))
    .toHaveAttribute("data-id", "openai/gpt-6-luna");
  await page.getByRole("button", { name: "ChatGPT", exact: true }).click();
  await expect
    .element(page.getByRole("option", { name: /GPT-6 Luna/u }))
    .toHaveAttribute("data-id", "chatgpt/gpt-6-luna");
  await expect.element(trigger).not.toHaveTextContent("API");
  await expect.element(trigger).not.toHaveTextContent("ChatGPT");
  expect(native.choose).not.toHaveBeenCalled();
  await page.getByRole("button", { name: "Search models" }).click();
  await page.getByRole("combobox").fill("gpt-6 luna");
  const api = page.getByRole("group", { name: "OpenAI · API key", exact: true });
  const account = page.getByRole("group", { name: "OpenAI · ChatGPT", exact: true });
  await expect
    .element(api.getByRole("option", { name: /GPT-6 Luna/u }))
    .toHaveAttribute("data-id", "openai/gpt-6-luna");
  await expect
    .element(account.getByRole("option", { name: /GPT-6 Luna/u }))
    .toHaveAttribute("data-id", "chatgpt/gpt-6-luna");
});

test("the OpenAI source choice starts native login only after explicit consent, and switching cancels its own pending attempt", async () => {
  native.ready.mockImplementation(ready(false));
  native.read.mockResolvedValue(models({}));
  native.choose.mockClear();
  let finish!: (view: ChatGptViewV1) => void;
  chatgpt.signIn.mockImplementation(
    () => new Promise<ChatGptViewV1>((resolve) => (finish = resolve)),
  );
  const screen = await render(ModelPicker, { profile: PROFILE });
  await screen.getByRole("button", { name: "Connect a model" }).click();
  await page.getByRole("tab", { name: "OpenAI, choose a connection" }).click();
  const source = page.getByRole("button", { name: "ChatGPT", exact: true });
  (source.element() as HTMLElement).focus();
  await userEvent.keyboard("{Enter}");
  await expect
    .element(page.getByText("Continue opens your system browser", { exact: false }))
    .toBeVisible();
  expect(chatgpt.signIn).not.toHaveBeenCalled();
  const consent = page.getByRole("button", { name: "Continue with ChatGPT" });
  (consent.element() as HTMLElement).focus();
  await userEvent.keyboard("{Enter}");
  await expect.element(page.getByRole("button", { name: "Cancel sign-in" })).toBeVisible();
  await expect.element(page.getByRole("button", { name: "Cancel sign-in" })).toHaveFocus();
  await page.getByRole("button", { name: "API key", exact: true }).click();
  await expect.element(page.getByRole("button", { name: "Add key", exact: true })).toBeVisible();
  await expect.poll(() => chatgpt.cancel.mock.calls.length).toBe(1);
  expect(chatgpt.cancel).toHaveBeenCalledWith(PROFILE, chatgpt.signIn.mock.calls[0]![1]);
  finish(chatgptView(PROFILE, { state: "connected", email: "stale@example.com", models: 1 }));
  await page.getByRole("button", { name: "ChatGPT", exact: true }).click();
  await expect.element(page.getByText("Not connected", { exact: true })).toBeVisible();
  expect(page.getByText("stale@example.com", { exact: false }).query()).toBeNull();
  expect(native.choose).not.toHaveBeenCalled();
});

test("Escape cancels a pending popover sign-in and reopening observes instead of restarting", async () => {
  native.ready.mockImplementation(ready(true));
  native.read.mockResolvedValue(view("openai/gpt-6-luna"));
  const screen = await render(ModelPicker, { profile: PROFILE });
  await screen.getByRole("button", { name: /Model: GPT-6 Luna/u }).click();
  await page.getByRole("button", { name: "ChatGPT", exact: true }).click();
  await page.getByRole("button", { name: "Continue with ChatGPT" }).click();
  await userEvent.keyboard("{Escape}");
  await expect.poll(() => chatgpt.cancel.mock.calls.length).toBe(1);
  await screen.getByRole("button", { name: /Model: GPT-6 Luna/u }).click();
  await page.getByRole("button", { name: "ChatGPT", exact: true }).click();
  await expect.element(page.getByText("Not connected", { exact: true })).toBeVisible();
  expect(chatgpt.signIn).toHaveBeenCalledTimes(1);
});

test("disconnecting ChatGPT keeps the API catalog and does not select a billing fallback", async () => {
  native.ready.mockImplementation(ready(true));
  native.read.mockResolvedValue(bothConnections());
  native.choose.mockClear();
  chatgpt.read.mockResolvedValue(chatgptView(PROFILE, { state: "connected", models: 1 }));
  const screen = await render(ModelPicker, { profile: PROFILE });
  await screen.getByRole("button", { name: /Model: GPT-6 Luna/u }).click();
  await page.getByRole("button", { name: "ChatGPT", exact: true }).click();
  await page.getByRole("button", { name: "Disconnect ChatGPT" }).click();
  await expect.element(page.getByText("Not connected", { exact: true })).toBeVisible();
  native.read.mockResolvedValue(view("openai/gpt-6-luna"));
  emitNativeEvent("workModelsChanged", { version: 1 });
  await page.getByRole("button", { name: "API key", exact: true }).click();
  await expect.element(page.getByRole("option", { name: /GPT-6 Luna/u })).toBeVisible();
  expect(chatgpt.signOut).toHaveBeenCalledExactlyOnceWith(PROFILE);
  expect(native.choose).not.toHaveBeenCalled();
});

test("subscription quota stays unknown and the official usage link cannot masquerade as a model choice", async () => {
  native.ready.mockImplementation(ready(true));
  native.read.mockResolvedValue(bothConnections());
  native.choose.mockClear();
  chatgpt.read.mockResolvedValue(chatgptView(PROFILE, { state: "connected", models: 1 }));
  const screen = await render(ModelPicker, { profile: PROFILE });
  await screen.getByRole("button", { name: /Model: GPT-6 Luna/u }).click();
  await page.getByRole("button", { name: "ChatGPT", exact: true }).click();
  await expect.element(page.getByText("Quota not provided by this connection.")).toBeVisible();
  expect(native.openUrl).not.toHaveBeenCalled();
  await page.getByRole("button", { name: "ChatGPT usage" }).click();
  await expect.element(page.getByRole("alert")).toHaveTextContent("Could not confirm");
  expect(native.openUrl).toHaveBeenCalledExactlyOnceWith(
    "https://chatgpt.com/settings/usage",
    true,
  );
  expect(native.choose).not.toHaveBeenCalled();
  expect(chatgpt.signIn).not.toHaveBeenCalled();
});

test("an unavailable saved subscription keeps its source pane instead of presenting an API billing fallback", async () => {
  native.ready.mockImplementation(ready(true));
  const unavailable = view("openai/gpt-6-luna");
  unavailable.chosen.page = "chatgpt/gpt-6-luna";
  unavailable.effective.page = null;
  native.read.mockResolvedValue(unavailable);
  native.choose.mockClear();
  const screen = await render(ModelPicker, { profile: PROFILE, role: "page" });
  await screen.getByRole("button", { name: "Model" }).click();
  await expect
    .element(page.getByRole("tab", { name: "OpenAI", exact: true }))
    .toHaveAttribute("aria-selected", "true");
  await expect
    .element(page.getByRole("button", { name: "ChatGPT", exact: true }))
    .toHaveAttribute("aria-pressed", "true");
  await expect
    .element(page.getByText("Saved ChatGPT model unavailable", { exact: false }))
    .toBeVisible();
  await expect.element(page.getByRole("button", { name: "Continue with ChatGPT" })).toBeVisible();
  expect(page.getByRole("option", { name: /GPT-6 Luna/u }).query()).toBeNull();
  expect(native.choose).not.toHaveBeenCalled();
});

test("connected account controls remain compact enough to show model choices beside quota errors", async () => {
  await page.viewport(1200, 800);
  native.ready.mockImplementation(ready(true));
  const state = bothConnections();
  for (const model of ["gpt-small", "gpt-large"])
    state.entries.push(
      entry("open_ai", model, model, false, ["lead", "page"], {
        id: `chatgpt/${model}`,
        price: null,
      }),
    );
  native.read.mockResolvedValue(state);
  chatgpt.read.mockResolvedValue(
    chatgptView(PROFILE, {
      state: "connected",
      models: 3,
      email: "reader@example.com",
      error: "usage_limit",
    }),
  );
  const screen = await render(ModelPicker, { profile: PROFILE });
  await screen.getByRole("button", { name: /Model: GPT-6 Luna/u }).click();
  await page.getByRole("button", { name: "ChatGPT", exact: true }).click();
  await expect.element(page.getByRole("alert")).toHaveTextContent("usage limit");
  await expect.element(page.getByRole("button", { name: "Refresh models" })).toBeVisible();
  await expect.element(page.getByRole("button", { name: "Disconnect ChatGPT" })).toBeVisible();
  await expect.element(page.getByRole("button", { name: "ChatGPT usage" })).toBeVisible();
  const list = page.getByRole("listbox").element() as HTMLElement;
  await expect.poll(() => list.getBoundingClientRect().height).toBeGreaterThanOrEqual(102);
  expect(list.querySelectorAll('[role="option"]')).toHaveLength(3);
});

test("a narrow non-reasoning account card keeps its fixed-effort label in flow and exposes connection context accessibly", async () => {
  await page.viewport(640, 600);
  native.ready.mockImplementation(ready(true));
  const state = bothConnections();
  state.effective.lead = "chatgpt/gpt-6-luna";
  state.chosen.lead = "chatgpt/gpt-6-luna";
  native.read.mockResolvedValue(state);
  chatgpt.read.mockResolvedValue(chatgptView(PROFILE, { state: "connected", models: 1 }));
  const screen = await render(ModelPicker, { profile: PROFILE });
  await screen.getByRole("button", { name: "Model: GPT-6 Luna", exact: true }).click();
  const card = page.getByRole("region", { name: "GPT-6 Luna", exact: true });
  await expect.element(card).toHaveAccessibleDescription(/Connection: ChatGPT/u);
  const fixed = page.getByText("Its own pace", { exact: true }).element() as HTMLElement;
  const identity = card.element().querySelector<HTMLElement>(".identity")!;
  expect(getComputedStyle(fixed).position).toBe("static");
  await expect
    .poll(() => fixed.getBoundingClientRect().left - identity.getBoundingClientRect().right)
    .toBeGreaterThanOrEqual(8);
  expect(fixed.getBoundingClientRect().right).toBeLessThanOrEqual(
    card.element().getBoundingClientRect().right,
  );
  expect(page.getByRole("button", { name: /Effort:/u }).query()).toBeNull();
});

test("one custom provider seat offers independent endpoint and local gateway sources", async () => {
  await page.viewport(1000, 800);
  const id = `gateway/${"a".repeat(32)}/example`;
  const catalog = models({ base: "http://localhost:11434/v1", lead: id, chosen: id });
  catalog.entries.push(
    entry("compatible", "example", "Example", false, ["lead"], {
      id,
      price: null,
      supports: {
        tools: true,
        vision: false,
        reasoning: false,
        prompt_cache: false,
        native_search: false,
      },
    }),
  );
  native.read.mockResolvedValue(catalog);
  native.ready.mockImplementation(ready(true));
  native.more.mockResolvedValue({
    provider: "compatible",
    entries: [entry("compatible", "endpoint-model", "Endpoint model", false)],
    fault: null,
  });
  gateway.read.mockResolvedValue(
    gatewayView(PROFILE, {
      state: "checked",
      connection_id: "a".repeat(32),
      base: "http://127.0.0.1:8080/v1",
      label: "Local",
      models: 1,
    }),
  );
  const screen = await render(ModelPicker, { profile: PROFILE });
  await screen.getByRole("button", { name: "Model: Example", exact: true }).click();
  await expect
    .element(screen.getByRole("button", { name: "Local gateway", exact: true }))
    .toHaveAttribute("aria-pressed", "true");
  await expect.element(screen.getByRole("option", { name: /^Example(?:\s|$)/u })).toBeVisible();
  await expect
    .element(screen.getByRole("option", { name: /^Example(?:\s|$)/u }))
    .toHaveAttribute("data-id", id);
  await expect
    .element(screen.getByText("Gateway quota and pricing are not reported."))
    .toBeVisible();
  const calls = native.choose.mock.calls.length;
  await screen.getByRole("button", { name: "Custom endpoint", exact: true }).click();
  await expect
    .element(screen.getByRole("option", { name: /^Example(?:\s|$)/u }))
    .not.toBeInTheDocument();
  expect(native.choose).toHaveBeenCalledTimes(calls);
  expect(gateway.check).not.toHaveBeenCalled();
  await screen.getByRole("button", { name: "Local gateway", exact: true }).click();
  native.read.mockResolvedValue({
    ...catalog,
    entries: catalog.entries.filter((row) => row.id !== id),
    effective: { ...catalog.effective, lead: null },
  });
  await screen.getByRole("button", { name: "Disconnect gateway" }).click();
  emitNativeEvent("workModelsChanged", { version: 1 });
  await expect
    .element(screen.getByText("Saved gateway model unavailable", { exact: false }))
    .toBeVisible();
  expect(gateway.disconnect).toHaveBeenCalledExactlyOnceWith(PROFILE, "a".repeat(32));
  expect(native.choose).toHaveBeenCalledTimes(calls);
});

test("gateway setup opens existing Settings, never inline secrets or automatic model checks", async () => {
  await page.viewport(1000, 800);
  native.read.mockResolvedValue(models({}));
  native.ready.mockImplementation(ready(false));
  const onsettings = vi.fn();
  const screen = await render(ModelPicker, { profile: PROFILE, onsettings });
  await screen.getByRole("button", { name: "Connect a model" }).click();
  await screen.getByRole("tab", { name: /Custom/ }).click();
  await screen.getByRole("button", { name: "Local gateway", exact: true }).click();
  await expect.element(screen.getByText("No local gateway configured")).toBeVisible();
  await screen.getByRole("button", { name: "Set up local gateway" }).click();
  expect(onsettings).toHaveBeenCalledTimes(1);
  expect(gateway.check).not.toHaveBeenCalled();
});
