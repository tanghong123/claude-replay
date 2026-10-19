import assert from "node:assert/strict";

/** Choose a canonical ID through the visible custom menu and a real pointer click. */
export async function chooseProject(page, triggerId, projectId) {
  await page.locator(`#${triggerId}`).click();
  const rows = page.locator(`#${triggerId}Options [role=option]`);
  const index = await rows.evaluateAll((rows, id) => rows.findIndex(row => row.dataset.projectValue === id), projectId);
  assert(index >= 0, "Requested canonical project ID must have a current option");
  await rows.nth(index).click();
}

/** Verify keyboard selection/dismissal and identity independent of duplicated display labels. */
export async function assertProjectPickerInteractions(page) {
  const trigger = page.locator("#sharedProjectFilter"), menu = page.locator("#sharedProjectFilterOptions"), rows = menu.getByRole("option");
  assert.equal(await trigger.evaluate(node => node.tagName), "BUTTON");
  const before = await trigger.evaluate(node => node.value);
  await trigger.focus(); await trigger.press("ArrowDown"); await menu.waitFor({ state: "visible" });
  assert.equal(await page.evaluate(() => document.activeElement.dataset.projectValue), "");
  await page.keyboard.press("End");
  assert.equal(await page.evaluate(() => document.activeElement.dataset.projectValue), await rows.last().getAttribute("data-project-value"));
  await page.keyboard.press("Home"); await page.keyboard.press("ArrowDown"); await page.keyboard.press("Escape");
  await menu.waitFor({ state: "hidden" }); assert.equal(await trigger.evaluate(node => node.value), before); assert.equal(await trigger.evaluate(node => node === document.activeElement), true);
  await trigger.click(); await page.locator("#sessionTitle").click(); await menu.waitFor({ state: "hidden" });
  await trigger.press("ArrowDown"); await page.keyboard.press("Tab"); await menu.waitFor({ state: "hidden" });
  assert.equal(await page.evaluate(() => document.activeElement.closest("[role=listbox]") !== null), false);

  // A disposable widget uses duplicate labels and markup-like text without changing native state.
  await page.evaluate(async () => {
    const { ProjectPicker } = await import("/codex-workspace/project-picker.js");
    window.projectPickerProbe = { calls: [] };
    const picker = new ProjectPicker({ id: "projectPickerProbe", label: "Probe project", onChange: id => window.projectPickerProbe.calls.push(id) });
    picker.setOptions([{ id: "canonical-a", label: "Same name", detail: "<img src=x onerror=alert(1)>" }, { id: "canonical-b", label: "Same name", detail: "/second" }], "canonical-a");
    document.querySelector(".nav").append(picker.root); window.projectPickerProbe.picker = picker;
  });
  const probe = page.locator("#projectPickerProbe");
  await probe.press("ArrowDown"); await page.keyboard.press("End"); await page.keyboard.press("Enter");
  assert.deepEqual(await page.evaluate(() => window.projectPickerProbe.calls), ["canonical-b"]);
  assert.equal(await probe.evaluate(node => node.value), "canonical-b");
  assert.equal(await page.locator("#projectPickerProbeOptions img").count(), 0);
  const calls = await page.evaluate(() => {
    const { picker, calls } = window.projectPickerProbe;
    picker.setOptions([{ id: "canonical-b", label: "Renamed" }, { id: "canonical-a", label: "Other" }], "canonical-b");
    const old = picker.menu.firstElementChild; picker.destroy(); old.click(); return calls;
  });
  assert.deepEqual(calls, ["canonical-b"], "Catalog repaint and disposed options cannot issue a selection callback");
  assert.equal(await probe.count(), 0);
  await page.evaluate(() => { delete window.projectPickerProbe; });
}
