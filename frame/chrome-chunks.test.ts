import { expect, it } from "vitest";
import { build } from "vite";
import { frameConfig } from "./vite.frame";
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

it("keeps browser resident code and CSS out of Panel's actual emitted shared graph", async () => {
  const root = "/fixture/frame";
  const browser = `${root}/browser.js`;
  const panel = `${root}/panel.js`;
  const ipc = `${root}/src/shared/ipc/commands.ts`;
  const icon = `${root}/src/shared/ui/Icon/Icon.svelte`;
  const glyph = `${root}/src/features/downloads/components/FileGlyph.svelte`;
  const style = `${glyph}?svelte&type=style&lang.css`;
  // This matches the resident helper list, but only a dynamic import reaches it.
  const lazy = `${root}/src/features/bookmarks/lib/compose.svelte.ts`;
  const modules = new Map([
    [
      browser,
      `import { glyph } from ${JSON.stringify(glyph)}; import { ipc } from ${JSON.stringify(ipc)}; console.log(glyph, ipc); globalThis.openFixture = () => import(${JSON.stringify(lazy)});`,
    ],
    [
      panel,
      `import { icon } from ${JSON.stringify(icon)}; import { ipc } from ${JSON.stringify(ipc)}; console.log(icon, ipc);`,
    ],
    [
      glyph,
      `import ${JSON.stringify(style)}; import { icon } from ${JSON.stringify(icon)}; import { ipc } from ${JSON.stringify(ipc)}; export const glyph = icon + ipc;`,
    ],
    [style, ".glyph { color: red; }"],
    [icon, 'export const icon = "shared icon";'],
    [ipc, 'export const ipc = "shared IPC";'],
    [lazy, 'export const optional = "lazy only";'],
  ]);
  const result = await build({
    configFile: false,
    publicDir: false,
    logLevel: "silent",
    plugins: [
      {
        name: "bounded-chrome-graph-fixture",
        resolveId: (id) => (modules.has(id) ? id : undefined),
        load: (id) => modules.get(id),
      },
    ],
    build: {
      write: false,
      minify: false,
      modulePreload: false,
      rolldownOptions: {
        input: { browser, panel },
        output: frameConfig(["browser", "panel"]).build!.rolldownOptions!.output,
      },
    },
  });
  if (Array.isArray(result) || !("output" in result)) throw new Error("expected one emitted graph");
  const chunks = result.output.filter((item) => item.type === "chunk");
  const reachable = (entry: string) => {
    const visited = new Set<string>();
    const found = new Set<string>();
    const walk = (file: string) => {
      if (visited.has(file)) return;
      visited.add(file);
      const chunk = chunks.find((item) => item.fileName === file);
      if (!chunk) throw new Error(`missing emitted chunk: ${file}`);
      Object.keys(chunk.modules).forEach((id) => found.add(id));
      chunk.imports.forEach(walk);
    };
    const chunk = chunks.find((item) => item.facadeModuleId === entry);
    if (!chunk) throw new Error(`missing emitted entry: ${entry}`);
    walk(chunk.fileName);
    return found;
  };
  const panelModules = reachable(panel);
  expect(panelModules).toContain(ipc);
  expect(panelModules).toContain(icon);
  expect(panelModules).not.toContain(glyph);
  expect(panelModules).not.toContain(style);
  expect(panelModules).not.toContain(lazy);
  const browserModules = reachable(browser);
  expect(browserModules).toContain(glyph);
  expect(browserModules).toContain(style);
  expect(browserModules).not.toContain(lazy);
  expect(chunks.some((item) => Object.hasOwn(item.modules, lazy))).toBe(true);
});

it("still rejects browser downloads CSS from any Panel-reachable chunk", () => {
  const plugin = bootstrapReport(["panel"]);
  const hook = plugin.generateBundle;
  const run = typeof hook === "function" ? hook : hook?.handler;
  const bundle = {
    "panel.js": {
      type: "chunk",
      fileName: "panel.js",
      isEntry: true,
      facadeModuleId: "/project/frame/panel.html",
      imports: ["shared.js"],
      dynamicImports: [],
      modules: {},
      code: "",
    },
    "shared.js": {
      type: "chunk",
      fileName: "shared.js",
      imports: [],
      dynamicImports: [],
      modules: {
        "/project/frame/src/features/downloads/components/FileGlyph.svelte?svelte&type=style&lang.css":
          {},
      },
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
  ).toThrow("which the browser already hosts");
});
