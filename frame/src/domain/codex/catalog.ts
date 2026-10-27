import type { CodexModelV1 } from "$shared/ipc/codex-bindings";

/** Display preference identity only. Never send this key as an original model ID. */
export const codexFavouriteKey = (id: string): string => `codex/${encodeURIComponent(id)}`;

/** Local catalog presentation; native selection remains the authority. */
export function filterCodexCatalog(
  models: readonly CodexModelV1[],
  query: string,
  favouritesOnly: boolean,
  favourites: ReadonlySet<string>,
): CodexModelV1[] {
  const words = query.trim().normalize("NFKC").toLocaleLowerCase().split(/\s+/u).filter(Boolean);
  return models.filter((model) => {
    if (favouritesOnly && !favourites.has(codexFavouriteKey(model.id))) return false;
    const text = `${model.name} ${model.id}`.normalize("NFKC").toLocaleLowerCase();
    return words.every((word) => text.includes(word));
  });
}
