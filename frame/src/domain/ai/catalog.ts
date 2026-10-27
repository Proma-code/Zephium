import * as m from "$shared/i18n/messages";
import type {
  WorkKeyStateV1,
  WorkModelEntry,
  WorkModelPrice,
  WorkModelProvider,
  WorkModelRole,
  WorkModelsV1,
} from "$shared/ipc/bindings";

/** Providers that take the person's own key, in the order they are shown. */
export const keyedProviders = [
  "anthropic",
  "open_ai",
  "google",
  "xai",
  "mistral",
  "deep_seek",
  "groq",
  "cerebras",
  "open_router",
  "compatible",
] as const satisfies readonly WorkModelProvider[];
export type KeyedProvider = (typeof keyedProviders)[number];

const names: Record<WorkModelProvider, string> = {
  anthropic: "Anthropic",
  open_ai: "OpenAI",
  google: "Google",
  deep_seek: "DeepSeek",
  open_router: "OpenRouter",
  xai: "xAI",
  mistral: "Mistral",
  groq: "Groq",
  cerebras: "Cerebras",
  compatible: "Custom",
  cloud: "Zephium",
  open_code_go: "OpenCode Go",
};

export function providerName(provider: WorkModelProvider): string {
  return names[provider];
}

/** Names stay vendor-native; connection identity belongs to surrounding UI, never the name. */
export function modelName(entry: WorkModelEntry): string {
  return entry.model.provider === "open_ai"
    ? entry.display_name.replace(/ · (?:ChatGPT|API)$/u, "")
    : entry.display_name;
}

/** Independent routing source; display metadata must not become a model identity. */
export function modelConnection(
  entry: WorkModelEntry,
): "chatgpt" | "api" | "gateway" | "endpoint" | "go" | null {
  if (entry.model.provider === "open_code_go") return "go";
  if (entry.model.provider === "open_ai")
    return entry.id.startsWith("chatgpt/") ? "chatgpt" : "api";
  if (entry.model.provider === "compatible")
    return entry.id.startsWith("gateway/") ? "gateway" : "endpoint";
  return null;
}

export function connectionName(source: NonNullable<ReturnType<typeof modelConnection>>): string {
  return {
    chatgpt: m.model_source_chatgpt,
    api: m.model_source_api,
    gateway: m.model_source_gateway,
    endpoint: m.model_source_endpoint,
    go: m.go_title,
  }[source]();
}

/** Protected adapters report local planning budgets, not verified provider capacity. */
export function modelContext(entry: WorkModelEntry): string {
  const source = modelConnection(entry);
  const size = contextSize(entry.context_window);
  return source === "chatgpt" || source === "gateway" || source === "go"
    ? m.model_context_budget({ size })
    : m.model_context({ size });
}

/** The model's name as the trigger shows it: the mark beside it says the family. */
export function shortName(entry: WorkModelEntry): string {
  const name = modelName(entry);
  for (const family of ["Claude ", "DeepSeek "]) {
    if (name.startsWith(family) && name.length > family.length) return name.slice(family.length);
  }
  return name;
}

export function keyState(models: WorkModelsV1 | null, provider: WorkModelProvider): WorkKeyStateV1 {
  return models?.providers.find((row) => row.provider === provider)?.key ?? "missing";
}

/** A provider can run calls now: its key is set and not refused. */
export function usable(models: WorkModelsV1 | null, provider: WorkModelProvider): boolean {
  if (!models || provider === "open_code_go") return false;
  if (provider === "cloud")
    return (
      models.cloud.signed_in &&
      models.entries.some(
        (entry) => entry.model.provider === "cloud" && !models.cloud.locked.includes(entry.id),
      )
    );
  if (provider === "compatible")
    return (
      !!models.providers.find((row) => row.provider === "compatible")?.base &&
      keyState(models, provider) !== "invalid"
    );
  const key = keyState(models, provider);
  return key === "set" || key === "valid";
}

type ModelGroup = { provider: WorkModelProvider; entries: WorkModelEntry[] };

/** Without a key, only these appear in the picker; Settings → AI offers every provider. */
const suggested: readonly WorkModelProvider[] = ["anthropic", "open_ai", "google"];

/**
 * The picker's list for a role: every curated model of usable providers,
 * recommended first, grouped by provider, the signed-in account's hosted
 * models first. The person's own server adds the models it serves.
 * The best-known providers without a key are listed apart, as needing one.
 */
export function pickerGroups(
  models: WorkModelsV1 | null,
  role: WorkModelRole,
  endpoint: readonly WorkModelEntry[] = [],
) {
  const entries = [...(models?.entries ?? []), ...endpoint];
  const order: readonly WorkModelProvider[] = ["cloud", ...keyedProviders];
  const ready: ModelGroup[] = [];
  const needsKey: WorkModelProvider[] = [];
  for (const provider of order) {
    const seen = new Set<string>();
    const offered = entries
      .filter((entry) => {
        if (
          /^(chatgpt|gateway|opencode-go)\//u.test(entry.id) ||
          entry.model.provider !== provider ||
          !(entry.roles.includes(role) || (role !== "decision" && entry.roles.includes("lead")))
        )
          return false;
        if (seen.has(entry.id)) return false;
        seen.add(entry.id);
        return true;
      })
      .sort((a, b) => Number(b.recommended) - Number(a.recommended));
    const providerUsable = usable(models, provider);
    const available = providerUsable ? offered : [];
    if (available.length) ready.push({ provider, entries: available });
    if (!providerUsable && !available.length && suggested.includes(provider))
      needsKey.push(provider);
  }
  return { ready, needsKey };
}

/** A hosted model the account's plan does not include: listed to explain the plan, never chosen. */
export function locked(models: WorkModelsV1 | null, id: string): boolean {
  return !!models?.cloud.locked.includes(id);
}

/** The plan a locked hosted model needs, as the server names it; null when it does not say. */
export function requiredPlan(models: WorkModelsV1 | null, id: string): string | null {
  return models?.cloud.required.find((row) => row.id === id)?.plan ?? null;
}

export function entryOf(models: WorkModelsV1 | null, id: string | null | undefined) {
  if (!models || !id) return null;
  return models.entries.find((entry) => entry.id === id) ?? null;
}

/** The efforts `id` takes as the lead, weakest first; empty when it takes none. */
export function effortLevels(models: WorkModelsV1 | null, id: string | null | undefined) {
  if (!models || !id || /^(chatgpt|gateway|opencode-go)\//u.test(id)) return [];
  return models.efforts.find((row) => row.id === id)?.levels ?? [];
}

/** The selected entry's native-resolved effort, independent of the Work/Lead route. */
export function modelEffort(models: WorkModelsV1 | null, id: string | null | undefined) {
  if (!models || !id) return null;
  const row = models.efforts.find((entry) => entry.id === id);
  return row?.effort && row.levels.includes(row.effort) ? row.effort : null;
}

/** A context window the way a model card says it: 1M, 400K, 128K. */
export function contextSize(tokens: number): string {
  const binary = tokens % 1024 === 0;
  if (tokens >= 1_000_000) {
    const millions = binary ? tokens / 1_048_576 : Math.floor(tokens / 100_000) / 10;
    return `${millions}M`;
  }
  return `${binary ? tokens / 1024 : Math.round(tokens / 1000)}K`;
}

/** One to four, from what a million output tokens cost; null when unpriced. */
export function priceTier(price: WorkModelPrice | null | undefined): 1 | 2 | 3 | 4 | null {
  if (!price) return null;
  const output = price.output / 1_000_000;
  if (output < 2) return 1;
  if (output < 15) return 2;
  if (output < 40) return 3;
  return 4;
}

/** Dollars per million tokens, as short as the number allows. */
export function dollars(micros: number): string {
  const value = micros / 1_000_000;
  const digits = value >= 1 ? (Number.isInteger(value) ? 0 : 2) : value >= 0.1 ? 2 : 3;
  return `$${value
    .toFixed(digits)
    .replace(/(\.\d*?)0+$/u, "$1")
    .replace(/\.$/u, "")}`;
}

/** The picker's matches for `query` across every ready provider, in the rail's order. */
export function searchEntries(groups: readonly ModelGroup[], query: string): WorkModelEntry[] {
  const words = query.toLocaleLowerCase().split(/\s+/u).filter(Boolean);
  if (!words.length) return [];
  return groups.flatMap((group) =>
    group.entries.filter((entry) => {
      const text =
        `${modelName(entry)} ${providerName(group.provider)} ${entry.model.model} ${modelConnection(entry) ?? ""}`.toLocaleLowerCase();
      return words.every((word) => text.includes(word));
    }),
  );
}
