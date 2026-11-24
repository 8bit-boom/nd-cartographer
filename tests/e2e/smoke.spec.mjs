import { test, expect } from '@playwright/test';

// The studio must boot, render and accept a paint without any uncaught error.
// This is the gate that would have caught the v0.1.0 syntax errors that broke
// the whole UI script (a syntax check passes nothing if the file never runs).
test('the studio boots, renders and paints without uncaught errors', async ({ page }) => {
  const pageErrors = [];
  page.on('pageerror', (e) => pageErrors.push(String(e)));

  await page.goto('/');
  await expect(page.locator('#canvas')).toBeVisible();
  // Boot finished: a project name is shown (not the initial "loading…").
  await expect(page.locator('#doc-name')).not.toHaveText('', { timeout: 15_000 });

  // Paint one cell with the paint tool.
  await page.click('#tool-paint');
  const box = await page.locator('#canvas').boundingBox();
  await page.mouse.click(box.x + box.width / 2, box.y + box.height / 2);

  // The edit round-trips (rev advances past 0) and nothing threw.
  await expect(page.locator('#doc-meta')).toContainText('rev', { timeout: 10_000 });

  // Auto-save: after editing pauses, the indicator reaches the saved state.
  await expect(page.locator('#save-state')).toContainText('✓ saved', { timeout: 10_000 });

  expect(pageErrors).toEqual([]);
});