import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { expect, it } from "vitest";
import { build } from "vite";
import { frameConfig } from "./vite.frame";

it("keeps connection presentation lazy when the picker consumes effort through the public API", async () => {
  const source = (name: string) =>
    fileURLToPath(new URL(`./src/domain/codex/${name}.ts`, import.meta.url)).replaceAll("\\", "/");
  const index = source("index");
  const effort = source("effort");
  const presentation = source("presentation");
  const names = [
    ...new Set(
      [effort, presentation].flatMap((file) =>
        [...readFileSync(file, "utf8").matchAll(/m\.(\w+)/gu)].map((match) => match[1]!),
      ),
    ),
  ];
  const root = "/fixture/frame";
  const messages = `${root}/src/shared/i18n/messages`;
  const browser = `${root}/browser.js`;
  const panel = `${root}/panel.js`;
  const picker = `${root}/ModelPicker.js`;
  const settings = `${root}/AiPage.js`;
  const native = `${root}/ModelCodex.js`;
  const connection = `${root}/CodexConnection.js`;
  const session = `${root}/session.js`;
  const modules = new Map<string, string>([
    [
      browser,
      `globalThis.picker=()=>import('${picker}'); globalThis.settings=()=>import('${settings}');`,
    ],
    [panel, 'console.log("panel");'],
    [
      picker,
      `import {codexEffort} from '${index}'; globalThis.label=()=>codexEffort('high'); export const native=()=>import('${native}');`,
    ],
    [
      settings,
      `import {loadCodexSession} from '${index}'; globalThis.account=loadCodexSession; export const connection=()=>import('${connection}');`,
    ],
    ...[native, connection].map((id): [string, string] => [
      id,
      `import {codexFault,codexStatus,codexCapability} from '${index}'; export const describe=(view)=>[codexFault(view.fault),codexStatus(view),codexCapability(view)];`,
    ]),
    [session, "export class CodexSession { value=1; }"],
    [
      `${messages}/_index.js`,
      names.map((name) => `export {${name}} from './${name}.js';`).join("\n"),
    ],
    ...names.map((name): [string, string] => [
      `${messages}/${name}.js`,
      `export const ${name}=()=>globalThis.fixtureLocale??'${name}';`,
    ]),
  ]);
  const result = await build({
    configFile: false,
    publicDir: false,
    logLevel: "silent",
    plugins: [
      {
        name: "bounded-codex-entry-graph",
        resolveId(id, importer) {
          if (id === "$shared/i18n/messages") return `${messages}/_index.js`;
          // No account observer runs in this emitted-graph fixture. All other
          // Codex modules, including the public index, are the actual source.
          if (id === "./session.svelte" && importer === index) return session;
          if (modules.has(id)) return id;
          if (id.startsWith(".") && importer?.startsWith(root)) {
            const relative = new URL(id, `file://${importer}`).pathname;
            if (modules.has(relative)) return relative;
          }
        },
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
  const pickerModules = reachable(picker);
  expect(pickerModules).toContain(effort);
  expect(pickerModules).not.toContain(presentation);
  expect(pickerModules).not.toContain(session);
  for (const name of names) {
    const message = `${messages}/${name}.js`;
    if (name.startsWith("codex_effort_")) expect(pickerModules).toContain(message);
    else expect(pickerModules).not.toContain(message);
  }
  for (const entry of [native, connection]) {
    const optional = reachable(entry);
    expect(optional).toContain(presentation);
    expect(optional).toContain(`${messages}/codex_fault_busy.js`);
    expect(optional).toContain(`${messages}/codex_connected.js`);
    expect(optional).toContain(`${messages}/codex_browser_ready.js`);
  }
});
