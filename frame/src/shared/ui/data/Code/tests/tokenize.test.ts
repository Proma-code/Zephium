import { expect, test } from "vitest";
import { tokenize, type Token } from "../tokenize";

const kinds = (row: Token[] | undefined, kind: Token["kind"]) =>
  (row ?? []).filter((token) => token.kind === kind).map((token) => token.text.trim());

test("rust: keywords, strings, numbers and line comments", () => {
  const [row] = tokenize(
    "rust",
    'pub fn size(x: u32) -> u32 { let s = "a\\"b"; x * 0x1F } // done',
  );
  expect(kinds(row, "keyword")).toEqual(["pub", "fn", "let"]);
  expect(kinds(row, "string")).toEqual(['"a\\"b"']);
  expect(kinds(row, "number")).toEqual(["0x1F"]);
  expect(kinds(row, "comment")).toEqual(["// done"]);
  expect(row?.map((token) => token.text).join("")).toBe(
    'pub fn size(x: u32) -> u32 { let s = "a\\"b"; x * 0x1F } // done',
  );
});

test("script family shares one table and template strings", () => {
  for (const language of ["typescript", "javascript", "tsx"]) {
    const [row] = tokenize(language, "const n = `t${1}`; return await f(2.5e3);");
    expect(kinds(row, "keyword")).toEqual(["const", "return", "await"]);
    expect(kinds(row, "string")).toEqual(["`t${1}`"]);
    expect(kinds(row, "number")).toEqual(["2.5e3"]);
  }
});

test("hash comment families", () => {
  expect(kinds(tokenize("python", "def f(): return None  # why")[0], "comment")).toEqual(["# why"]);
  expect(kinds(tokenize("python", "def f(): return None")[0], "keyword")).toEqual([
    "def",
    "return",
    "None",
  ]);
  expect(kinds(tokenize("ruby", "def x; end # c")[0], "keyword")).toEqual(["def", "end"]);
  expect(kinds(tokenize("bash", 'if [ -f "$f" ]; then echo ok; fi')[0], "keyword")).toEqual([
    "if",
    "then",
    "echo",
    "fi",
  ]);
  expect(kinds(tokenize("yaml", "on: true # x")[0], "keyword")).toEqual(["on", "true"]);
  expect(kinds(tokenize("toml", 'name = "z" # x')[0], "string")).toEqual(['"z"']);
});

test("c, jvm, go and php families", () => {
  expect(kinds(tokenize("cpp", "static int n = 42; // x")[0], "keyword")).toEqual(["static"]);
  expect(kinds(tokenize("cpp", "static int n = 42; // x")[0], "type")).toEqual(["int"]);
  expect(kinds(tokenize("csharp", "public class A {}")[0], "keyword")).toEqual(["public", "class"]);
  expect(kinds(tokenize("kotlin", "val x = when (y) {}")[0], "keyword")).toEqual(["val", "when"]);
  expect(kinds(tokenize("swift", "guard let x else { return }")[0], "keyword")).toEqual([
    "guard",
    "let",
    "else",
    "return",
  ]);
  expect(kinds(tokenize("go", "func main() { defer f() }")[0], "keyword")).toEqual([
    "func",
    "defer",
  ]);
  expect(kinds(tokenize("php", "echo $x; # note")[0], "comment")).toEqual(["# note"]);
});

test("sql and dockerfile keywords ignore case", () => {
  expect(kinds(tokenize("sql", "SELECT id FROM t WHERE n > 1 -- rows")[0], "keyword")).toEqual([
    "SELECT",
    "FROM",
    "WHERE",
  ]);
  expect(kinds(tokenize("dockerfile", "FROM node:24 AS build")[0], "keyword")).toEqual([
    "FROM",
    "AS",
  ]);
});

test("minimal and unknown languages", () => {
  expect(kinds(tokenize("json", '{"a": [1, true, null]}')[0], "keyword")).toEqual(["true", "null"]);
  expect(kinds(tokenize("css", "a { width: 12px; } /* c */")[0], "comment")).toEqual(["/* c */"]);
  expect(kinds(tokenize("html", '<p class="x"><!-- c --></p>')[0], "comment")).toEqual([
    "<!-- c -->",
  ]);
  expect(tokenize("markdown", "# Title")[0]).toEqual([{ kind: "plain", text: "# Title" }]);
  expect(tokenize("text", "fn if 1")).toEqual([[{ kind: "plain", text: "fn if 1" }]]);
  expect(tokenize("klingon", "a\n\nb")).toEqual([
    [{ kind: "plain", text: "a" }],
    [],
    [{ kind: "plain", text: "b" }],
  ]);
});

test("an open quote is not yet a string and block comments carry until closed", () => {
  const rows = tokenize("typescript", 'const s = "open\nlet x = 1; /* a\nstill */ let y\nlet z');
  expect(kinds(rows[0], "string")).toEqual([]);
  expect(kinds(rows[1], "keyword")).toEqual(["let"]);
  expect(kinds(rows[1], "comment")).toEqual(["/* a"]);
  expect(kinds(rows[2], "comment")).toEqual(["still */"]);
  expect(kinds(rows[2], "keyword")).toEqual(["let"]);
  expect(kinds(rows[3], "keyword")).toEqual(["let"]);
  expect(kinds(tokenize("python", "x = 'a\\")[0], "string")).toEqual([]);
});

test("a limit tokenizes only the leading lines and odd input never throws", () => {
  expect(tokenize("rust", "a\nb\nc", 2)).toHaveLength(2);
  expect(tokenize("rust", "a\r\nb\n")).toHaveLength(2);
  expect(() => tokenize("rust", "\u0000é😀\\\"'`/*")).not.toThrow();
  expect(
    tokenize("rust", "😀 é")[0]
      ?.map((token) => token.text)
      .join(""),
  ).toBe("😀 é");
});

test("calls, types and properties", () => {
  const [row] = tokenize("typescript", "const user: User = api.fetch(id).profile as string;");
  expect(kinds(row, "function")).toEqual(["fetch"]);
  expect(kinds(row, "type")).toEqual(["User", "string"]);
  expect(kinds(row, "property")).toEqual(["profile"]);
  // A keyword after a dot is a property: `x.default`.
  expect(kinds(tokenize("typescript", "x.default")[0], "property")).toEqual(["default"]);
  const [rust] = tokenize("rust", 'let v: Vec<u8> = vec![1]; println!("{}", v.len());');
  expect(kinds(rust, "function")).toEqual(["vec", "println", "len"]);
  expect(kinds(rust, "type")).toEqual(["Vec", "u8"]);
});

test("keys are properties in data", () => {
  expect(kinds(tokenize("json", '{"name": "zephium", "n": 1}')[0], "property")).toEqual([
    '"name"',
    '"n"',
  ]);
  expect(kinds(tokenize("json", '{"name": "zephium"}')[0], "string")).toEqual(['"zephium"']);
  expect(kinds(tokenize("yaml", "name: zephium")[0], "property")).toEqual(["name"]);
  expect(kinds(tokenize("toml", 'name = "z"')[0], "property")).toEqual(["name"]);
});

test("tags and attributes in HTML, Svelte and JSX, their script read as script", () => {
  const svelte = tokenize(
    "svelte",
    '<script lang="ts">\n  let count = $state(0);\n</script>\n\n{#if count}<button class="go" onclick={() => count++}>Add</button>{/if}',
  );
  expect(kinds(svelte[0], "tag")).toEqual(["script"]);
  expect(kinds(svelte[0], "attribute")).toEqual(["lang"]);
  expect(kinds(svelte[1], "keyword")).toEqual(["let"]);
  expect(kinds(svelte[1], "function")).toEqual(["$state"]);
  expect(kinds(svelte[4], "keyword")).toEqual(["#if", "/if"]);
  expect(kinds(svelte[4], "tag")).toEqual(["button", "button"]);
  expect(kinds(svelte[4], "attribute")).toEqual(["class", "onclick"]);
  const [jsx] = tokenize("tsx", 'return <Card title="x">{n > 1 ? <b>many</b> : null}</Card>;');
  expect(kinds(jsx, "tag")).toEqual(["Card", "b", "b", "Card"]);
  expect(kinds(jsx, "attribute")).toEqual(["title"]);
  expect(kinds(jsx, "keyword")).toEqual(["return", "null"]);
  // A comparison is not a tag.
  expect(kinds(tokenize("tsx", "if (a <b) f();")[0], "tag")).toEqual([]);
  expect(kinds(tokenize("html", '<p class="x"><!-- c --></p>')[0], "tag")).toEqual(["p", "p"]);
});

test("css: selectors, properties, at-rules and colours", () => {
  const [row] = tokenize("css", "@media screen { .card:hover { color: #fff; margin: 0 auto; } }");
  expect(kinds(row, "keyword")).toEqual(["@media"]);
  expect(kinds(row, "property")).toEqual(["color", "margin"]);
  expect(kinds(row, "number")).toEqual(["#fff", "0"]);
});

test("a 2,000-line file tokenizes in a few milliseconds", () => {
  const unit = [
    'import { thing } from "./thing";',
    "/** A doc comment. */",
    "export function run(value: number): Result<string> {",
    "  const out = value.toFixed(2) + `x${value}`; // why",
    "  return out.length > 3 ? ok(out) : fail(new Error('short'));",
    "}",
    "",
    "<Card title={name}>{items.map((item) => <Row key={item.id} />)}</Card>;",
  ].join("\n");
  const text = Array.from({ length: 250 }, () => unit).join("\n");
  for (let warm = 0; warm < 3; warm += 1) tokenize("tsx", text);
  // Shared CI runners can deschedule one sample or collect garbage between ticks.
  // Keep the same 40ms bound on the median of seven warmed, complete tokenizations:
  // a sustained slowdown still fails, but one scheduling outlier is not a regression.
  const samples = Array.from({ length: 7 }, () => {
    const started = performance.now();
    const rows = tokenize("tsx", text);
    const took = performance.now() - started;
    expect(rows).toHaveLength(2000);
    return took;
  }).sort((a, b) => a - b);
  expect(samples[3]).toBeLessThan(40);
});
