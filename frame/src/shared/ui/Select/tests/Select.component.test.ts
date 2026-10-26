import "$styles/global.css";
import { expect, test } from "vitest";
import { render } from "vitest-browser-svelte";
import { page } from "vitest/browser";
import Select from "../Select.svelte";

const options = ["Small", "Medium", "Large", "Extra large"].map((label) => ({
  value: label.toLowerCase(),
  label,
}));

test("the chosen row opens over the trigger, its label on the trigger's", async () => {
  await page.viewport(640, 480);
  document.body.style.padding = "200px 160px";
  const screen = await render(Select, { label: "Size", options, value: "large" });
  const trigger = screen.getByRole("button", { name: "Size" }).element() as HTMLElement;
  await screen.getByRole("button", { name: "Size" }).click();
  const row = page.getByRole("option", { name: "Large", exact: true });
  await expect.element(row).toBeVisible();
  await new Promise((done) => setTimeout(done, 400));
  const at = trigger.getBoundingClientRect();
  const on = (row.element() as HTMLElement).getBoundingClientRect();
  expect(Math.abs(on.top + on.height / 2 - (at.top + at.height / 2))).toBeLessThan(2);
  await page.screenshot({ path: "../../../../../../target/look/select/popup.png" });
});

test("connection details identify equal model names without changing the chosen label", async () => {
  const screen = await render(Select, {
    label: "Model",
    value: "account/model",
    options: [
      { value: "api/model", label: "Example model", detail: "API key" },
      { value: "account/model", label: "Example model", detail: "ChatGPT" },
    ],
  });
  const trigger = screen.getByLabelText("Model", { exact: true });
  await expect.element(trigger).toHaveTextContent("Example model");
  await expect.element(trigger).not.toHaveTextContent("ChatGPT");
  await trigger.click();
  const rows = page.getByRole("option", { name: "Example model", exact: true }).all();
  expect(rows).toHaveLength(2);
  await expect.element(rows[0]!).toHaveAccessibleDescription("API key");
  await expect.element(rows[1]!).toHaveAccessibleDescription("ChatGPT");
});
