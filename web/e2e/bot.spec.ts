import { test, expect, freshProject } from "./fixtures";

// A Maintainer turns the Forgust bot on; it joins the project and carries out
// the commands written in comments, answering what it cannot do.

test("forgust bot: turn on, a command in a comment, turn off", async ({ page }) => {
  const { web, api, project, remove } = await freshProject(page, "bot", { "README.md": "# Bot\n" });
  try {
    await page.goto(`${web}/-/settings/automation`);
    const section = page.locator("section.settings-section", { has: page.getByRole("heading", { name: "Forgust bot" }) });
    await section.getByRole("checkbox", { name: /commands written in comments/ }).check();
    await section.getByRole("button", { name: "Save", exact: true }).click();
    await expect(section.getByText("@forgust-bot joined the project")).toBeVisible();
    // no `[labels]` rules in the repository yet
    await expect(section.getByText("It labels no merge request yet.")).toBeVisible();
    await page.goto(`${web}/-/members`);
    await expect(page.getByText("forgust-bot").first()).toBeVisible();

    await api("post", `/projects/${project.id}/labels`, { name: "bug" });
    const issue = await api("post", `/projects/${project.id}/issues`, { title: "It crashes", description: "" });
    await page.goto(`${web}/-/issues/${issue.iid}`);
    // a `/` that starts a line suggests the commands; Enter takes the one shown
    const composer = page.locator(".composer textarea");
    await composer.fill("Seen it too.\n");
    await composer.press("End");
    // every command is within reach: the list scrolls, by wheel and with the keys
    await composer.press("/");
    const suggestions = page.getByRole("listbox", { name: "Suggestions" });
    await expect(suggestions.getByRole("option")).toHaveCount(8);
    await composer.press("ArrowUp");
    await expect(suggestions.getByRole("option").last()).toBeInViewport();
    await composer.press("ArrowDown");
    await expect(suggestions.getByRole("option").first()).toBeInViewport();
    await suggestions.hover();
    await page.mouse.wheel(0, 400);
    await expect(suggestions).toBeVisible();
    await expect.poll(() => suggestions.evaluate((el) => el.scrollTop)).toBeGreaterThan(0);
    await composer.pressSequentially("lab");
    await expect(suggestions.getByRole("option")).toHaveCount(1);
    await expect(suggestions.getByRole("option", { name: /\/label ~label/ })).toBeVisible();
    await composer.press("Enter");
    await expect(composer).toHaveValue("Seen it too.\n/label ");
    await composer.pressSequentially("~bug\n/frobnicate");
    await expect(composer).toHaveValue("Seen it too.\n/label ~bug\n/frobnicate");
    await page.getByRole("button", { name: "Comment", exact: true }).click();
    // done by the bot, and what it could not do is answered
    await expect(page.getByRole("complementary", { name: "Issue details" }).locator(".label-chip", { hasText: "bug" })).toBeVisible();
    await expect(page.locator(".thread.plain", { hasText: "there is no such command" })).toBeVisible();

    await page.goto(`${web}/-/settings/automation`);
    await section.getByRole("checkbox", { name: /commands written in comments/ }).uncheck();
    await section.getByRole("button", { name: "Save", exact: true }).click();
    await expect(section.getByText("The bot is off: it left the project.")).toBeVisible();
  } finally {
    await remove();
  }
});
