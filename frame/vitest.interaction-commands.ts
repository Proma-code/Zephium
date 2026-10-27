import type {} from "@vitest/browser-playwright";
import type { BrowserCommand } from "vitest/node";

/** Test-only real mouse gesture. Playwright boxes use main-frame viewport coordinates. */
export const dragCodexEffortToEnd: BrowserCommand<[], void> = async ({ page, iframe }) => {
  const slider = iframe.getByRole("slider", { name: "Effort", exact: true });
  // Locator hover waits for the opening face animation to settle before raw
  // mouse coordinates are captured; no production motion is disabled.
  await slider.hover();
  const box = await slider.boundingBox();
  if (!box) throw new Error("The effort slider must be visible before dragging");
  const y = box.y + box.height / 2;
  await page.mouse.move(box.x + 8, y);
  await page.mouse.down();
  try {
    // Separate browser input packets cross both detents while the first native
    // commit remains deliberately unresolved in the component fixture.
    await page.mouse.move(box.x + box.width / 2, y);
    await page.mouse.move(box.x + box.width - 8, y);
  } finally {
    await page.mouse.up();
  }
};
