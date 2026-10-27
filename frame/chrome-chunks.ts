/**
 * Browser-only helpers shared with lazy destinations, never Panel. They are
 * already in the browser's static graph; `$initial` on the group remains the
 * admission condition so a future lazy-only consumer is not made eager here.
 */
export const chromeResidentHelpers =
  /[\\/]src[\\/](?:domain[\\/]downloads[\\/](?:downloads\.svelte|download-model|index)\.ts|features[\\/](?:tabs[\\/](?:lib[\\/]space-look\.ts|components[\\/]space-hue\.css)|downloads[\\/](?:lib[\\/](?:errors|transfer)\.ts|components[\\/]FileGlyph\.svelte(?:\?svelte&type=style&lang\.css)?)|bookmarks[\\/]lib[\\/](?:reveal|compose)\.svelte\.ts))$/u;
