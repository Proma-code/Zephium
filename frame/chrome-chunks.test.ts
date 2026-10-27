import { expect, it } from "vitest";
import { chromeResidentHelpers } from "./chrome-chunks";
import { bootstrapReport } from "./bootstrap-report";

const resident = [
  "domain/downloads/downloads.svelte.ts",
  "domain/downloads/download-model.ts",
  "domain/downloads/index.ts",
  "features/tabs/lib/space-look.ts",
  "features/tabs/components/space-hue.css",
  "features/downloads/lib/errors.ts",
  "features/downloads/lib/transfer.ts",
  "features/downloads/components/FileGlyph.svelte",
  "features/downloads/components/FileGlyph.svelte?svelte&type=style&lang.css",
  "features/bookmarks/lib/reveal.svelte.ts",
  "features/bookmarks/lib/compose.svelte.ts",
];

it("partitions only the observed resident helpers with either platform separator", () => {
  for (const name of resident) {
    expect(chromeResidentHelpers.test(`/project/frame/src/${name}`), name).toBe(true);
    expect(
      chromeResidentHelpers.test(`C:/project/frame/src/${name}`.replaceAll("/", "\\")),
      name,
    ).toBe(true);
  }
});

it("does not select optional views, account observation, or unrelated same-named files", () => {
  for (const name of [
    "domain/codex/session.svelte.ts",
    "features/codex/components/CodexConnection.svelte",
    "features/downloads/components/DownloadsList.svelte",
    "features/tabs/components/SpaceEditor.svelte",
    "features/bookmarks/components/BookmarksPanel.svelte",
    "features/work/components/bar/ModelCodex.svelte",
    "features/other/lib/errors.ts",
    "domain/downloads/tests/downloads.test.ts",
  ])
    expect(chromeResidentHelpers.test(`/project/frame/src/${name}`), name).toBe(false);
});

it("rejects a native connection module in the actual emitted startup guard", async () => {
  const plugin = bootstrapReport(["browser"]);
  const hook = plugin.generateBundle;
  const run = typeof hook === "function" ? hook : hook?.handler;
  expect(run).toBeTypeOf("function");
  const bundle = {
    "browser.js": {
      type: "chunk",
      fileName: "browser.js",
      isEntry: true,
      facadeModuleId: "/project/frame/browser.html",
      imports: [],
      dynamicImports: [],
      modules: { "/project/frame/src/domain/codex/session.svelte.ts": {} },
      code: "",
    },
  };
  expect(() =>
    run!.call(
      {
        error: (message: string) => {
          throw new Error(message);
        },
      } as never,
      {} as never,
      bundle as never,
      false,
    ),
  ).toThrow("Optional assistant code in browser startup");
});
