import type { KnipConfig } from "knip";
import { migrationIssues } from "./knip-migration";
import ts from "typescript";
import { compile } from "svelte/compiler";

export default {
  // Public primitive modules form the copy-owned UI library; only explicit imports enter production.
  entry: [
    "src/entries/*.ts",
    "src/shared/ui/*/index.ts",
    "src/shared/ui/data/*/index.ts",
    "src/**/*.test.ts",
    "*.test.ts",
    "*.config.{js,ts}",
    "bootstrap-report.ts",
  ],
  ignoreIssues: {
    ...migrationIssues,
    // Native-generated DTOs are transitively referenced protocol types; Rust
    // regeneration checks their complete shape. Keep value/export checks.
    "src/shared/ipc/chatgpt-bindings.ts": ["types"],
    "src/shared/ipc/codex-bindings.ts": ["types"],
  },
  project: ["src/**/*.{ts,svelte,css}", "*.{js,ts}", "scripts/**/*.mjs"],
  ignore: ["src/shared/ipc/bindings.ts"],
  // Compile the template too: extracting only <script> imports misclassifies
  // domain namespace members used by markup as unused exports.
  compilers: {
    svelte: (source, filename) => {
      const script = [...source.matchAll(/<script[^>]*>([\s\S]*?)<\/script>/gu)]
        .map((match) => match[1])
        .join("\n");
      const ast = ts.createSourceFile(
        filename + ".ts",
        script,
        ts.ScriptTarget.Latest,
        true,
        ts.ScriptKind.TS,
      );
      const types: string[] = [];
      for (const statement of ast.statements) {
        if (!ts.isImportDeclaration(statement) || !statement.importClause) continue;
        const clause = statement.importClause;
        if (clause.isTypeOnly) types.push(statement.getText(ast));
        else if (clause.namedBindings && ts.isNamedImports(clause.namedBindings)) {
          const names = clause.namedBindings.elements
            .filter((item) => item.isTypeOnly)
            .map((item) => item.getText(ast).replace(/^type\s+/u, ""));
          if (names.length)
            types.push(
              `import type { ${names.join(", ")} } from ${statement.moduleSpecifier.getText(ast)};`,
            );
        }
      }
      return compile(source, { filename, generate: "client" }).js.code + "\n" + types.join("\n");
    },
  },
  // Loaded by the Inlang JSON configuration and Tailwind's CSS import respectively.
  ignoreDependencies: ["@inlang/plugin-message-format"],
  // macOS tools the WebKit benches read memory with, and the sky baker's encoder.
  ignoreBinaries: ["footprint", "ffmpeg", "sips"],
} satisfies KnipConfig;
