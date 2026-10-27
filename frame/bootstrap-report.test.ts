import { beforeEach, expect, it, vi } from "vitest";

const disk = vi.hoisted(() => ({ writes: new Map<string, string>() }));
vi.mock("node:fs", () => ({
  mkdirSync: vi.fn(),
  readFileSync: () =>
    JSON.stringify({ "browser-panel": { browser: { js: 20, css: 0 }, panel: { js: 20, css: 0 } } }),
  writeFileSync: (path: URL, value: string) => disk.writes.set(path.pathname, value),
}));
import { bootstrapReport } from "./bootstrap-report";

beforeEach(() => disk.writes.clear());

it("preserves exact charged chunks before rejecting an over-budget graph without writing a baseline", () => {
  const plugin = bootstrapReport(["browser", "panel"]);
  const hook = plugin.generateBundle;
  const run = typeof hook === "function" ? hook : hook?.handler;
  const root = "/project/frame";
  const model = `${root}/src/features/work/components/bar/ModelPicker.svelte`;
  const bundle = {
    "browser.js": {
      type: "chunk",
      fileName: "browser.js",
      isEntry: true,
      facadeModuleId: `${root}/browser.html`,
      imports: ["shared.js"],
      dynamicImports: ["picker.js"],
      modules: {},
      code: "browser",
    },
    "panel.js": {
      type: "chunk",
      fileName: "panel.js",
      isEntry: true,
      facadeModuleId: `${root}/panel.html`,
      imports: ["shared.js"],
      dynamicImports: [],
      modules: {},
      code: "panel",
    },
    "shared.js": {
      type: "chunk",
      fileName: "shared.js",
      imports: [],
      dynamicImports: [],
      modules: {},
      code: "shared",
    },
    "picker.js": {
      type: "chunk",
      fileName: "picker.js",
      isDynamicEntry: true,
      facadeModuleId: model,
      imports: ["shared.js"],
      dynamicImports: [],
      modules: { [model]: { renderedLength: 50_000 } },
      code: "x".repeat(50_000),
      viteMetadata: { importedCss: new Set(["picker.css"]) },
    },
    "picker.css": { type: "asset", fileName: "picker.css", source: ".picker{}" },
  };
  expect(() =>
    run!.call(
      {
        error: (message: string) => {
          throw new Error(message);
        },
        warn: vi.fn(),
      } as never,
      {} as never,
      bundle as never,
      false,
    ),
  ).toThrow("adds 50000 JS bytes with no recorded size");
  const reportEntry = [...disk.writes].find(([path]) =>
    path.endsWith("/reports/bootstrap-report.json"),
  );
  const chunksEntry = [...disk.writes].find(([path]) =>
    path.endsWith("/reports/bootstrap-chunks.json"),
  );
  expect(reportEntry).toBeDefined();
  expect(chunksEntry).toBeDefined();
  const report = JSON.parse(reportEntry![1]);
  const picker = report["lazy:features/work/components/bar/ModelPicker.svelte"];
  expect(picker.charged).toEqual({ js: ["picker.js"], css: ["picker.css"] });
  expect(picker.budgeted).toEqual({ js: 50_000, css: 9 });
  const graph = JSON.parse(chunksEntry![1]);
  expect(graph.chunks.find((chunk: { file: string }) => chunk.file === "picker.js")).toMatchObject({
    bytes: 50_000,
    imports: ["shared.js"],
    modules: [{ id: model, renderedLength: 50_000 }],
  });
  expect(graph.assets).toEqual([{ file: "picker.css", bytes: 9 }]);
  expect([...disk.writes.keys()]).toHaveLength(2);
  expect(
    [...disk.writes.keys()].every((path) => path.includes("/reports/") && !path.includes("/dist/")),
  ).toBe(true);
  expect([...disk.writes.keys()].some((path) => path.endsWith("bundle-budgets.json"))).toBe(false);
});
