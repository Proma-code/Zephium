import "$styles/global.css";
import { beforeEach, expect, test, vi } from "vitest";
import { render } from "vitest-browser-svelte";
import { page, userEvent } from "vitest/browser";
import type { WorkModelReasoning, WorkModelsReadyV1 } from "$shared/ipc/bindings";
import { preferences } from "$domain/preferences";
import ModelPicker from "../components/bar/ModelPicker.svelte";
import ModelCard from "../components/bar/ModelCard.svelte";
import ModelBarStage from "./ModelBarStage.svelte";
import { PROFILE, entry, models } from "$shared/testing/work-models";
import { goView, goQuotaReading } from "$shared/testing/opencode-go";
import { gatewayView } from "$shared/testing/gateway";
import { chatgptView } from "$shared/testing/chatgpt";
import { codexView } from "$shared/testing/codex";
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

const codex = vi.hoisted(() => ({
  read: vi.fn(),
  check: vi.fn(),
  connect: vi.fn(),
  cancel: vi.fn(),
  logout: vi.fn(),
  models: vi.fn(),
  select: vi.fn(),
}));
vi.mock("$shared/ipc/codex-bindings", () => ({
  commands: {
    codexAccount: codex.read,
    codexCheckConnection: codex.check,
    codexConnect: codex.connect,
    codexCancelLogin: codex.cancel,
    codexLogout: codex.logout,
    codexRefreshModels: codex.models,
    codexSelectModel: codex.select,
  },
}));
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
    chatgptSignOut: chatgpt.signOut,
  });
});
const go = vi.hoisted(() => ({
  read: vi.fn(),
  check: vi.fn(),
  quota: vi.fn(),
  disconnect: vi.fn(),
}));
vi.mock("$shared/ipc/go-bindings", async () => {
  const { mockGoBindings } = await import("$shared/testing/opencode-go");
  return mockGoBindings({
    goStatus: go.read,
    goCheck: go.check,
    goQuota: go.quota,
    goDisconnect: go.disconnect,
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
  native.effort.mockReset();
  native.choose.mockReset();
  for (const command of Object.values(codex)) command.mockReset();

  for (const command of Object.values(go)) command.mockReset();
  go.read.mockResolvedValue(goView(PROFILE));
  go.disconnect.mockResolvedValue(goView(PROFILE));
  for (const command of Object.values(gateway)) command.mockReset();
  gateway.read.mockResolvedValue(gatewayView(PROFILE));
  gateway.disconnect.mockResolvedValue(gatewayView(PROFILE));
  native.openUrl.mockReset();
  native.openUrl.mockRejectedValue(new Error("mock browser unavailable"));
  for (const command of Object.values(chatgpt)) command.mockReset();
  chatgpt.read.mockResolvedValue(chatgptView(PROFILE));
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

  const trigger = screen.getByRole("button", {
    name: "Work · Model: Claude Opus 5.5, Medium effort",
  });
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
    "OpenCode Go, choose a connection",
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
  await screen.getByRole("button", { name: "Work · Model: Mistral Medium 3.5" }).click();
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
  expect(
    page.getByRole("button", { name: "Legacy direct account", exact: true }).query(),
  ).toBeNull();
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

test("legacy direct catalogue is not offered or silently replaced by an API choice", async () => {
  native.ready.mockImplementation(ready(true));
  const state = bothConnections();
  state.effective.page = "chatgpt/gpt-6-luna";
  state.chosen.page = "chatgpt/gpt-6-luna";
  native.read.mockResolvedValue(state);
  const screen = await render(ModelPicker, { profile: PROFILE, role: "page" });
  await screen.getByRole("button", { name: "Model: GPT-6 Luna", exact: true }).click();
  await expect
    .element(screen.getByText("This saved direct connection is retired.", { exact: false }))
    .toBeVisible();
  expect(page.getByRole("button", { name: "Continue with ChatGPT" }).query()).toBeNull();
  expect(
    page.getByRole("button", { name: "Legacy direct account", exact: true }).query(),
  ).toBeNull();
  expect(
    page
      .getByRole("option")
      .all()
      .some((row) => row.element().getAttribute("data-id")?.startsWith("chatgpt/")),
  ).toBe(false);
  expect(native.choose).not.toHaveBeenCalled();
  expect(chatgpt.signIn).not.toHaveBeenCalled();
  expect(chatgpt.signOut).not.toHaveBeenCalled();
  expect(chatgpt.read).not.toHaveBeenCalled();
});

test("a narrow non-reasoning account card keeps its fixed-effort label in flow and exposes connection context accessibly", async () => {
  await page.viewport(640, 600);
  native.ready.mockImplementation(ready(true));
  const state = bothConnections();
  state.effective.lead = "chatgpt/gpt-6-luna";
  state.chosen.lead = "chatgpt/gpt-6-luna";
  native.read.mockResolvedValue(state);
  chatgpt.read.mockResolvedValue(chatgptView(PROFILE, { state: "connected", presence: "stored" }));
  const screen = await render(ModelPicker, { profile: PROFILE });
  await screen.getByRole("button", { name: "Work · Model: GPT-6 Luna", exact: true }).click();
  const card = page.getByRole("region", { name: "GPT-6 Luna", exact: true });
  await expect.element(card).toHaveAccessibleDescription(/Connection: Legacy direct account/u);
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
  await screen.getByRole("button", { name: "Work · Model: Example", exact: true }).click();
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

test("OpenCode Go has its own provider seat, clean models, and no automatic catalog request", async () => {
  await page.viewport(1000, 800);
  const id = `opencode-go/${"a".repeat(32)}/example`;
  const catalog = models({ keys: { open_ai: "valid" }, lead: id, chosen: id });
  catalog.entries.push(
    entry("open_code_go", "example", "Example", false, ["lead"], {
      id,
      price: null,
      supports: {
        tools: true,
        vision: false,
        prompt_cache: false,
        reasoning: false,
        native_search: false,
      },
    }),
  );
  native.read.mockResolvedValue(catalog);
  native.ready.mockImplementation(ready(true));
  go.read.mockResolvedValue(
    goView(PROFILE, {
      state: "catalogued",
      connection_id: "a".repeat(32),
      models: 1,
      last_billing: { source: "go_plus", observed_at_ms: "1700000000000" },
    }),
  );
  const screen = await render(ModelPicker, { profile: PROFILE });
  await screen.getByRole("button", { name: "Work · Model: Example", exact: true }).click();
  await expect
    .element(screen.getByRole("tab", { name: "OpenCode Go", exact: true }))
    .toHaveAttribute("aria-selected", "true");
  expect(screen.getByRole("tab", { name: /^OpenCode Go/u }).all()).toHaveLength(1);
  await expect.element(screen.getByRole("option", { name: /^Example(?:\s|$)/u })).toBeVisible();
  await expect
    .element(screen.getByRole("option", { name: /^Example(?:\s|$)/u }))
    .toHaveAttribute("data-id", id);
  await expect
    .element(screen.getByText("Public catalog; not key or plan verification."))
    .toBeVisible();
  await expect
    .element(
      screen.getByText("Last provider-reported billing: Go Plus allowance", { exact: false }),
    )
    .toBeVisible();
  await expect.element(screen.getByText("Remaining quota has not been checked.")).toBeVisible();
  expect(go.check).not.toHaveBeenCalled();
  const choices = native.choose.mock.calls.length;
  await screen.getByRole("tab", { name: "OpenAI", exact: true }).click();
  expect(native.choose).toHaveBeenCalledTimes(choices);
  await expect
    .element(screen.getByRole("button", { name: "Work · Model: Example", exact: true }))
    .toBeVisible();
  await screen.getByRole("tab", { name: "OpenCode Go", exact: true }).click();
  native.read.mockResolvedValue({
    ...catalog,
    entries: catalog.entries.filter((row) => row.id !== id),
    effective: { ...catalog.effective, lead: null },
  });
  await screen.getByRole("button", { name: "Disconnect OpenCode Go" }).click();
  emitNativeEvent("workModelsChanged", { version: 1 });
  await expect
    .element(screen.getByText("Saved OpenCode Go model unavailable", { exact: false }))
    .toBeVisible();
  expect(go.disconnect).toHaveBeenCalledExactlyOnceWith(PROFILE, "a".repeat(32));
  expect(native.choose).toHaveBeenCalledTimes(choices);
  await screen.getByRole("tab", { name: "OpenAI", exact: true }).click();
  await expect.element(screen.getByRole("option", { name: /^GPT-6 Luna(?:\s|$)/u })).toBeVisible();
});

test("Go setup uses its own settings flow, not the generic API-key or Custom form", async () => {
  await page.viewport(1000, 800);
  native.read.mockResolvedValue(models({}));
  native.ready.mockImplementation(ready(false));
  const onsettings = vi.fn();
  const screen = await render(ModelPicker, { profile: PROFILE, onsettings });
  await screen.getByRole("button", { name: "Connect a model" }).click();
  await screen.getByRole("tab", { name: "OpenCode Go, choose a connection", exact: true }).click();
  await expect.element(screen.getByText("No OpenCode Go key saved")).toBeVisible();
  await expect
    .element(screen.getByRole("button", { name: "Add key", exact: true }))
    .not.toBeInTheDocument();
  await screen.getByRole("button", { name: "Set up OpenCode Go" }).click();
  expect(onsettings).toHaveBeenCalledTimes(1);
  expect(go.check).not.toHaveBeenCalled();
});

test("Go quota refresh leaves model choice untouched and stale quota remains distinct from billing", async () => {
  await page.viewport(1000, 800);
  native.read.mockResolvedValue(models({}));
  native.ready.mockImplementation(ready(false));
  const connection = "a".repeat(32);
  go.read.mockResolvedValue(goView(PROFILE, { state: "configured", connection_id: connection }));
  const reading = goQuotaReading();
  go.quota.mockResolvedValue(
    goView(PROFILE, { state: "configured", connection_id: connection, quota: reading }),
  );
  const screen = await render(ModelPicker, { profile: PROFILE, onsettings: vi.fn() });
  await screen.getByRole("button", { name: "Connect a model" }).click();
  await screen.getByRole("tab", { name: "OpenCode Go, choose a connection", exact: true }).click();
  expect(go.quota).not.toHaveBeenCalled();
  const choices = native.choose.mock.calls.length;
  await screen.getByRole("button", { name: "Refresh Go quota" }).click();
  await expect.element(screen.getByText("80% remaining", { exact: false })).toBeVisible();
  await expect.element(screen.getByText("Provider-reported remaining quota")).toBeVisible();
  expect(go.quota).toHaveBeenCalledExactlyOnceWith(PROFILE, connection);
  expect(native.choose).toHaveBeenCalledTimes(choices);
  expect(go.check).not.toHaveBeenCalled();
  go.quota.mockResolvedValue(
    goView(PROFILE, {
      state: "configured",
      connection_id: connection,
      quota: { ...reading, state: "stale", error: "unavailable" },
    }),
  );
  await screen.getByRole("button", { name: "Refresh Go quota" }).click();
  await expect
    .element(screen.getByText("Stale quota — refresh for a current reading."))
    .toBeVisible();
  await expect.element(screen.getByText("80% remaining", { exact: false })).toBeVisible();
  expect(native.choose).toHaveBeenCalledTimes(choices);
  const apiSeat = screen.getByRole("tab", { name: "OpenAI, choose a connection", exact: true });
  await expect.element(apiSeat).toHaveAttribute("data-provider", "open_ai");
  await expect.element(apiSeat).toHaveAttribute("data-standing", "key");
  await apiSeat.click();
  await expect.element(apiSeat).toHaveAttribute("aria-selected", "true");
  await expect.element(screen.getByText("80% remaining", { exact: false })).not.toBeInTheDocument();
  expect(native.choose).toHaveBeenCalledTimes(choices);
});

test.each([
  ["open_ai", "chatgpt/example", "Legacy direct account", "32K context budget"],
  ["open_code_go", `opencode-go/${"a".repeat(32)}/example`, "OpenCode Go", "32K context budget"],
  ["compatible", `gateway/${"a".repeat(32)}/example`, "Local gateway", "32K context budget"],
  ["open_ai", "openai/example", "API key", "32K context"],
  ["compatible", "compatible/example", "Custom endpoint", "32K context"],
] as const)(
  "the %s model card describes %s without mistaking an adapter budget for vendor capacity",
  async (provider, id, connection, context) => {
    const screen = await render(ModelCard, {
      subject: {
        entry: entry(provider, "example", "Example", false, ["lead"], {
          id,
          context_window: 32768,
          max_output: 8192,
        }),
        tier: null,
        levels: [],
        effort: null,
      },
      expanded: false,
      region: "model-context-fixture",
      ontoggle: vi.fn(),
      onEffort: vi.fn(),
    });
    const card = screen.getByRole("region", { name: "Example", exact: true });
    await expect
      .element(card)
      .toHaveAccessibleDescription(new RegExp(`Connection: ${connection}`, "u"));
    await expect.element(card).toHaveAccessibleDescription(new RegExp(`${context}(?: ·|$)`, "u"));
    if (context === "32K context")
      await expect.element(card).not.toHaveAccessibleDescription(/context budget/u);
    await expect.element(card).toHaveAccessibleName("Example");
  },
);

test.each([true, false])(
  "retired Page selection never offers effort mutation or silently changes source (service available: %s)",
  async (available) => {
    const id = "chatgpt/gpt-6.1-sol";
    const state = models({});
    state.entries.push(
      entry("open_ai", "gpt-6.1-sol", "GPT-6.1 Sol", false, ["page"], { id, price: null }),
    );
    state.effective.page = id;
    state.chosen.page = id;
    state.efforts.push({ id, levels: ["low", "medium", "high", "xhigh", "max"], effort: "medium" });
    native.read.mockResolvedValue(state);
    native.account.mockResolvedValue({
      account: accountFixture({ available, signed_in: false }),
      fault: null,
      retry_after: null,
    });
    const screen = await render(ModelPicker, { profile: PROFILE, role: "page" });
    await screen.getByRole("button", { name: "Model: GPT-6.1 Sol", exact: true }).click();
    await expect
      .element(screen.getByText("This saved direct connection is retired.", { exact: false }))
      .toBeVisible();
    expect(screen.getByRole("button", { name: /^Effort:/u }).query()).toBeNull();
    expect(screen.getByRole("slider", { name: "Effort", exact: true }).query()).toBeNull();
    expect(native.effort).not.toHaveBeenCalled();
    expect(native.choose).not.toHaveBeenCalled();
    expect(chatgpt.read).not.toHaveBeenCalled();
  },
);

test("profile replacement discards queued effort without applying it to the new connection", async () => {
  native.ready.mockImplementation(ready(true));
  const nextProfile = "00000000000000000000000002";
  const id = "openai/gpt-6-luna";
  const previous = view(id);
  const next = { ...view(id), profile: nextProfile };
  let finish!: (value: typeof previous) => void;
  native.effort.mockReset();
  native.effort
    .mockReturnValueOnce(
      new Promise<typeof previous>((resolve) => {
        finish = resolve;
      }),
    )
    .mockImplementation(async (profile: string, effort: WorkModelReasoning) => ({
      ...next,
      profile,
      effort,
      efforts: next.efforts.map((row) => (row.id === id ? { ...row, effort } : row)),
    }));
  native.read.mockImplementation(async (profile: string) =>
    profile === PROFILE ? previous : next,
  );
  native.account.mockResolvedValue({
    account: accountFixture({ signed_in: false }),
    fault: null,
    retry_after: null,
  });
  const screen = await render(ModelPicker, { profile: PROFILE });
  await screen
    .getByRole("button", { name: "Work · Model: GPT-6 Luna, Medium effort", exact: true })
    .click();
  await screen.getByRole("button", { name: "Effort: Medium", exact: true }).click();
  await userEvent.keyboard("{ArrowRight}");
  await expect.poll(() => native.effort.mock.calls.length).toBe(1);
  await userEvent.keyboard("{ArrowRight}");
  await expect
    .element(screen.getByRole("slider", { name: "Effort", exact: true }))
    .toHaveAttribute("aria-valuetext", "Extra high");
  await screen.rerender({ profile: nextProfile });
  await expect
    .element(
      screen.getByRole("button", { name: "Work · Model: GPT-6 Luna, Medium effort", exact: true }),
    )
    .toBeVisible();
  finish({
    ...previous,
    effort: "high",
    efforts: previous.efforts.map((row) => (row.id === id ? { ...row, effort: "high" } : row)),
  });
  await expect
    .element(
      screen.getByRole("button", { name: "Work · Model: GPT-6 Luna, Medium effort", exact: true }),
    )
    .toBeVisible();
  expect(native.effort).toHaveBeenCalledExactlyOnceWith(PROFILE, "high");
  const slider = screen.getByRole("slider", { name: "Effort", exact: true });
  if (!slider.query())
    await screen.getByRole("button", { name: "Effort: Medium", exact: true }).click();
  (slider.element() as HTMLElement).focus();
  await userEvent.keyboard("{End}");
  await expect.poll(() => native.effort.mock.calls.length).toBe(2);
  expect(native.effort).toHaveBeenLastCalledWith(nextProfile, "max");
  await expect
    .element(
      screen.getByRole("button", { name: "Work · Model: GPT-6 Luna, Max effort", exact: true }),
    )
    .toBeVisible();
});

test("Codex is Page-only and opening other provider controls does not prepare it", async () => {
  native.read.mockResolvedValue(view("openai/gpt-6-sol"));
  const screen = await render(ModelPicker, { profile: PROFILE, role: "lead" });
  await screen.getByRole("button", { name: /Model:/u }).click();
  expect(page.getByRole("button", { name: "Codex", exact: true }).query()).toBeNull();
  expect(codex.read).not.toHaveBeenCalled();
});

test("Page Codex uses original catalog and exact supported effort without a Work model shim", async () => {
  native.read.mockResolvedValue(view("openai/gpt-6-sol"));
  const original = {
    id: "original-picker-id",
    name: "Original Native Name",
    efforts: ["minimal", "high"] as const,
    default_effort: "high" as const,
  };
  const catalog = codexView(PROFILE, {
    state: "connected",
    models: [{ ...original, efforts: [...original.efforts] }],
  });
  codex.read.mockResolvedValue(catalog);
  codex.select.mockImplementation(async (_profile: string, selection) => ({
    ...catalog,
    selection,
    page_selected: true,
  }));
  const screen = await render(ModelPicker, { profile: PROFILE, role: "page" });
  expect(codex.read).not.toHaveBeenCalled();
  await screen.getByRole("button", { name: /Model/u }).click();
  await page.getByRole("tab", { name: "OpenAI", exact: true }).click();
  await page.getByRole("button", { name: "Codex", exact: true }).click();
  await expect
    .element(page.getByRole("option", { name: "Original Native Name", exact: true }))
    .toBeVisible();
  expect(codex.read).toHaveBeenCalledExactlyOnceWith(PROFILE);
  expect(codex.select).not.toHaveBeenCalled();
  expect(codex.connect).not.toHaveBeenCalled();
  expect(page.getByRole("combobox", { name: "Codex model" }).query()).toBeNull();
  expect(page.getByRole("button", { name: "Use in sidebar", exact: true }).query()).toBeNull();
  await page.getByRole("option", { name: "Original Native Name", exact: true }).click();
  expect(codex.select).toHaveBeenCalledExactlyOnceWith(PROFILE, {
    model: "original-picker-id",
    effort: { kind: "default" },
  });
  expect(native.choose).not.toHaveBeenCalledWith(PROFILE, "page", "original-picker-id");
});

test("retained Codex trigger needs no process preparation until its panel opens", async () => {
  native.read.mockResolvedValue(view("openai/gpt-6-sol"));
  const screen = await render(ModelPicker, {
    profile: PROFILE,
    role: "page",
    nativeModel: {
      id: "native-id",
      name: "Original Native Name",
      source: "codex",
      capabilities: "text_only",
      hosted: false,
      effort: "high",
    },
  });
  await expect
    .element(screen.getByRole("button", { name: /Original Native Name · Codex/u }))
    .toBeVisible();
  const trigger = screen.getByRole("button", {
    name: /Original Native Name · Codex/u,
  });
  await expect.element(trigger).toHaveTextContent(/^Original Native Name\s*·\s*High$/u);
  await expect.element(trigger).not.toHaveTextContent("Codex");
  expect(codex.read).not.toHaveBeenCalled();
  expect(codex.select).not.toHaveBeenCalled();
});

test("Codex uses the existing effort pill and slider with only original advertised stops", async () => {
  native.read.mockResolvedValue(view("openai/gpt-6-sol"));
  const catalog = codexView(PROFILE, {
    state: "connected",
    page_selected: true,
    selection: { model: "original", effort: { kind: "explicit", effort: "high" } },
    models: [
      {
        id: "original",
        name: "Original Native Name",
        efforts: ["minimal", "high"],
        default_effort: "high",
      },
    ],
  });
  codex.read.mockResolvedValue(catalog);
  codex.select.mockImplementation(async (_profile: string, selection) => ({
    ...catalog,
    selection,
  }));
  const screen = await render(ModelPicker, {
    profile: PROFILE,
    role: "page",
    nativeModel: {
      id: "original",
      name: "Original Native Name",
      source: "codex",
      capabilities: "text_only",
      hosted: false,
      effort: "high",
    },
  });
  await screen.getByRole("button", { name: /Original Native Name · Codex/u }).click();
  await page.getByRole("button", { name: "Effort: High" }).click();
  const slider = page.getByRole("slider", { name: "Effort" });
  await expect.element(slider).toHaveAttribute("aria-valuemax", "1");
  await expect.element(slider).toHaveAttribute("aria-valuetext", "High");
  await expect.element(slider).toHaveFocus();
  // No input or page-focus transfer: normal reading time must not dismiss the
  // existing effort surface or commit a choice by itself.
  await new Promise((resolve) => setTimeout(resolve, 8_000));
  await expect.element(slider).toBeVisible();
  await expect.element(slider).toHaveFocus();
  expect(codex.select).not.toHaveBeenCalled();
  await userEvent.keyboard("{Home}");
  expect(codex.select).toHaveBeenLastCalledWith(PROFILE, {
    model: "original",
    effort: { kind: "explicit", effort: "minimal" },
  });
  await expect.element(slider).toHaveAttribute("aria-valuetext", "Minimal");
  expect(page.getByRole("combobox").query()).toBeNull();
  expect(codex.connect).not.toHaveBeenCalled();
  await userEvent.keyboard("{Escape}");
  await expect
    .element(page.getByRole("button", { name: "Effort: Minimal", exact: true }))
    .toHaveFocus();
  expect(slider.query()).toBeNull();
}, 20_000);

test("a held Codex effort pointer is not dismissed while focus moves within the picker", async () => {
  const catalog = codexView(PROFILE, {
    state: "connected",
    page_selected: true,
    selection: { model: "original", effort: { kind: "explicit", effort: "high" } },
    models: [
      {
        id: "original",
        name: "Original Native Name",
        efforts: ["minimal", "high"],
        default_effort: "high",
      },
    ],
  });
  codex.read.mockResolvedValue(catalog);
  const screen = await render(ModelPicker, {
    profile: PROFILE,
    role: "page",
    nativeModel: {
      id: "original",
      name: "Original Native Name",
      source: "codex",
      capabilities: "text_only",
      hosted: false,
      effort: "high",
    },
  });
  await screen.getByRole("button", { name: /Original Native Name · Codex/u }).click();
  await page.getByRole("button", { name: "Effort: High", exact: true }).click();
  const slider = page.getByRole("slider", { name: "Effort", exact: true });
  const face = slider.element().closest(".face")!;
  face.dispatchEvent(new PointerEvent("pointerdown", { bubbles: true, pointerId: 7, button: 0 }));
  const rows = page.getByRole("listbox", { name: "Codex model", exact: true });
  (rows.element() as HTMLElement).focus();
  await expect.element(rows).toHaveFocus();
  await new Promise((resolve) => setTimeout(resolve, 4_000));
  await expect.element(slider).toBeVisible();
  expect(codex.select).not.toHaveBeenCalled();
  // A release outside the face must end the hold and restore ordinary idle dismissal.
  window.dispatchEvent(new PointerEvent("pointerup", { pointerId: 7 }));
  await expect.poll(() => slider.query(), { timeout: 5_000 }).toBeNull();
  await expect
    .element(page.getByRole("button", { name: "Effort: High", exact: true }))
    .toHaveFocus();
  expect(codex.select).not.toHaveBeenCalled();
}, 15_000);

test("Page offers exactly Codex and API key without converting a retained legacy choice", async () => {
  await page.viewport(640, 600);
  native.ready.mockImplementation(ready(true));
  const state = bothConnections();
  state.effective.page = "chatgpt/gpt-6-luna";
  state.chosen.page = "chatgpt/gpt-6-luna";
  native.read.mockResolvedValue(state);
  const screen = await render(ModelPicker, { profile: PROFILE, role: "page" });
  await screen.getByRole("button", { name: "Model: GPT-6 Luna", exact: true }).click();
  const group = page
    .getByRole("group", { name: "OpenAI connection method", exact: true })
    .element();
  const buttons = Array.from(group.querySelectorAll<HTMLButtonElement>("button"));
  expect(
    buttons.map((button) => button.getAttribute("aria-label") ?? button.textContent?.trim()),
  ).toEqual(["API key", "Codex"]);
  for (const button of buttons) {
    expect(button.scrollWidth).toBeLessThanOrEqual(button.clientWidth);
    expect(button.getBoundingClientRect().right).toBeLessThanOrEqual(
      group.getBoundingClientRect().right,
    );
  }
  await page.getByRole("button", { name: "Codex", exact: true }).click();
  await page.getByRole("button", { name: "API key", exact: true }).click();
  expect(codex.select).not.toHaveBeenCalled();
  expect(codex.connect).not.toHaveBeenCalled();
  expect(chatgpt.signIn).not.toHaveBeenCalled();
  expect(chatgpt.signOut).not.toHaveBeenCalled();
  expect(native.choose).not.toHaveBeenCalled();
});

test("Codex default-only row selects original omission without inventing an effort slider", async () => {
  native.read.mockResolvedValue(view("openai/gpt-6-sol"));
  const catalog = codexView(PROFILE, {
    state: "connected",
    models: [{ id: "default-only", name: "Original Default", efforts: [], default_effort: "none" }],
  });
  codex.read.mockResolvedValue(catalog);
  codex.select.mockImplementation(async (_profile: string, selection) => ({
    ...catalog,
    selection,
    page_selected: true,
  }));
  const screen = await render(ModelPicker, { profile: PROFILE, role: "page" });
  await screen.getByRole("button", { name: /Model/u }).click();
  await page.getByRole("tab", { name: "OpenAI", exact: true }).click();
  await page.getByRole("button", { name: "Codex", exact: true }).click();
  await page.getByRole("option", { name: "Original Default", exact: true }).click();
  expect(codex.select).toHaveBeenCalledExactlyOnceWith(PROFILE, {
    model: "default-only",
    effort: { kind: "default" },
  });
  expect(native.effort).not.toHaveBeenCalled();
});

test("Codex model default is separate from ranked explicit effort and can be restored unchanged", async () => {
  native.read.mockResolvedValue(view("openai/gpt-6-sol"));
  const catalog = codexView(PROFILE, {
    state: "connected",
    page_selected: true,
    selection: { model: "original", effort: { kind: "default" } },
    models: [
      {
        id: "original",
        name: "Original Native Name",
        efforts: ["none", "high"],
        default_effort: "high",
      },
    ],
  });
  codex.read.mockResolvedValue(catalog);
  codex.select.mockImplementation(async (_profile: string, selection) => ({
    ...catalog,
    selection,
  }));
  const screen = await render(ModelPicker, {
    profile: PROFILE,
    role: "page",
    nativeModel: {
      id: "original",
      name: "Original Native Name",
      source: "codex",
      capabilities: "text_only",
      hosted: false,
      effort: null,
    },
  });
  await screen.getByRole("button", { name: /Original Native Name · Codex · Default/u }).click();
  await page.getByRole("button", { name: "Effort: Default", exact: true }).click();
  const choices = page.getByRole("group", { name: "Choose a supported effort", exact: true });
  await expect.element(choices.getByRole("button", { name: "None", exact: true })).toHaveFocus();
  await choices.getByRole("button", { name: "None", exact: true }).click();
  expect(codex.select).toHaveBeenLastCalledWith(PROFILE, {
    model: "original",
    effort: { kind: "explicit", effort: "none" },
  });
  await page.getByRole("button", { name: "Use model default", exact: true }).click();
  expect(codex.select).toHaveBeenLastCalledWith(PROFILE, {
    model: "original",
    effort: { kind: "default" },
  });
  expect(native.effort).not.toHaveBeenCalled();
  expect(codex.connect).not.toHaveBeenCalled();
});

test("Work keeps its configured model and clearly separates sidebar Codex scope", async () => {
  native.read.mockResolvedValue(view("openai/gpt-6-sol"));
  const screen = await render(ModelPicker, {
    profile: PROFILE,
    role: "lead",
    nativeModel: {
      id: "native-only",
      name: "Original Sidebar Model",
      source: "codex",
      capabilities: "text_only",
      hosted: false,
      effort: "high",
    },
  });
  await screen.getByRole("button", { name: /Work · Model: GPT-6 Sol/u }).click();
  await expect
    .element(page.getByText("Work and sidebar chat use separate model settings."))
    .toBeVisible();
  await page.getByRole("tab", { name: "OpenAI", exact: true }).click();
  await expect
    .element(page.getByRole("button", { name: "Codex", exact: true }))
    .not.toBeInTheDocument();
  await expect
    .element(screen.getByRole("button", { name: /Work · Model: GPT-6 Sol/u }))
    .toBeVisible();
  expect(codex.read).not.toHaveBeenCalled();
  expect(codex.select).not.toHaveBeenCalled();
  expect(native.choose).not.toHaveBeenCalled();
});

test("native Codex search and source-prefixed favourites never become Work choices", async () => {
  native.read.mockResolvedValue(view("openai/gpt-6-sol"));
  await preferences.init();
  const catalog = codexView(PROFILE, {
    state: "connected",
    models: [
      { id: "native/sol", name: "Native Sol", efforts: ["low"], default_effort: "low" },
      { id: "native-astra", name: "Native Astra", efforts: ["max"], default_effort: "max" },
    ],
  });
  codex.read.mockResolvedValue(catalog);
  codex.select.mockImplementation(async (_profile: string, selection) => ({
    ...catalog,
    selection,
    page_selected: true,
  }));
  const screen = await render(ModelPicker, { profile: PROFILE, role: "page" });
  await screen.getByRole("button", { name: /Model/u }).click();
  await page.getByRole("tab", { name: "OpenAI", exact: true }).click();
  await page.getByRole("button", { name: "Codex", exact: true }).click();
  await expect.element(page.getByRole("option", { name: "Native Sol", exact: true })).toBeVisible();
  await page.getByRole("button", { name: "Add Native Sol to favourites", exact: true }).click();
  await expect.poll(() => native.settings["work.favourite-models"]).toContain("codex/native%2Fsol");
  expect(codex.select).not.toHaveBeenCalled();
  await page.getByRole("button", { name: "Favourites", exact: true }).click();
  await expect.element(page.getByRole("option", { name: "Native Sol", exact: true })).toBeVisible();
  await expect
    .element(page.getByRole("option", { name: "Native Astra", exact: true }))
    .not.toBeInTheDocument();
  await page.getByRole("button", { name: "Search models", exact: true }).click();
  const search = page.getByRole("combobox", { name: "Search models", exact: true });
  await search.fill("astra");
  await expect
    .element(page.getByRole("option", { name: "Native Sol", exact: true }))
    .not.toBeInTheDocument();
  expect(codex.select).not.toHaveBeenCalled();
  await search.fill("sol");
  await userEvent.keyboard("{ArrowDown}{Enter}");
  expect(codex.select).toHaveBeenCalledExactlyOnceWith(PROFILE, {
    model: "native/sol",
    effort: { kind: "default" },
  });
  expect(native.choose).not.toHaveBeenCalledWith(PROFILE, "page", "codex/native%2Fsol");
  await screen.unmount();
});
