import { test, expect, P, at } from "./fixtures";

test("open an issue, label it, comment and close it", async ({ page }) => {
  const name = `bug-${Date.now() % 100000}`;
  // a label to pick
  await page.goto(`${P}/-/labels`);
  await page.getByRole("button", { name: "New label" }).click();
  await page.getByLabel("Name").fill(name);
  await page.getByRole("button", { name: "Create label" }).click();
  await expect(page.locator(".label-row", { hasText: name })).toBeVisible();

  // a new issue with that label
  await page.goto(`${P}/-/issues/new`);
  const title = `Crash ${Date.now()}`;
  await page.getByLabel("Title").fill(title);
  await page.getByRole("button", { name: "Labels" }).click();
  await page.getByRole("option", { name }).click();
  await page.keyboard.press("Escape");
  await page.getByRole("button", { name: "Create issue" }).click();
  await at(page, new RegExp(`^${P}/-/issues/\\d+$`));
  await expect(page.getByRole("heading", { name: title })).toBeVisible();
  await expect(page.getByRole("complementary", { name: "Issue details" }).locator(".label-chip", { hasText: name })).toBeVisible();

  // comment, then close with the composer's button
  await page.locator(".composer textarea").fill("Seen on **main** too");
  await page.getByRole("button", { name: "Comment", exact: true }).click();
  await expect(page.locator(".tl-body strong", { hasText: "main" })).toBeVisible();
  await page.getByRole("button", { name: "Close issue" }).last().click();
  await expect(page.locator(".mr-state.closed")).toBeVisible();
  await expect(page.locator(".tl-body", { hasText: "closed" })).toBeVisible();

  // the list filters by label and shows it under Closed
  await page.goto(`${P}/-/issues?state=closed&label=${name}`);
  await expect(page.locator(".issue-row", { hasText: title })).toBeVisible();
});

test("milestones: create, plan an issue with a due date, follow progress, close", async ({ page }) => {
  const title = `M ${Date.now() % 100000}`;
  await page.goto(`${P}/-/milestones`);
  await page.getByRole("link", { name: "New milestone" }).click();
  await page.getByLabel("Title").fill(title);
  await page.getByLabel(/^Due date/).fill("2031-06-30");
  await page.getByRole("button", { name: "Create milestone" }).click();
  await expect(page.getByRole("heading", { name: title })).toBeVisible();
  await expect(page.getByText("Due Jun 30, 2031")).toBeVisible();

  // an issue on it, with a due date
  await page.goto(`${P}/-/issues/new`);
  const issue = `Planned ${Date.now()}`;
  await page.getByLabel("Title").fill(issue);
  await page.getByRole("button", { name: "Create issue" }).click();
  await at(page, new RegExp(`^${P}/-/issues/\\d+$`));
  const aside = page.getByRole("complementary", { name: "Issue details" });
  await aside.getByRole("combobox", { name: "Milestone" }).click();
  await page.getByRole("option", { name: title }).click();
  await expect(page.locator(".tl-body", { hasText: "changed milestone" })).toBeVisible();
  await aside.getByLabel("Due date").fill("2031-06-01");
  await expect(page.locator(".tl-body", { hasText: "changed due date to June 1, 2031" })).toBeVisible();

  // the list filters by it and shows it
  await page.goto(`${P}/-/issues`);
  await page.getByRole("combobox", { name: "Milestone" }).click();
  await page.getByRole("option", { name: title }).click();
  await expect(page.locator(".issue-row")).toHaveCount(1);
  await expect(page.locator(".issue-row").first().locator(".milestone-chip")).toHaveText(title);

  // its page lists the issue; closing the issue completes it
  await page.locator(".issue-row").first().locator(".milestone-chip").click();
  await expect(page.getByRole("region", { name: "Issues" }).getByRole("link", { name: issue })).toBeVisible();
  await expect(page.getByText("0% complete")).toBeVisible();
  await page.getByRole("region", { name: "Issues" }).getByRole("link", { name: issue }).click();
  await page.getByRole("button", { name: "Close issue" }).last().click();
  await expect(page.locator(".mr-state.closed")).toBeVisible();
  await page.goBack();
  await page.reload();
  await expect(page.getByText("100% complete")).toBeVisible();
  await page.getByRole("button", { name: "Close milestone" }).click();
  await expect(page.getByRole("button", { name: "Reopen milestone" })).toBeVisible();
});

test("react to an issue and to a comment", async ({ page }) => {
  await page.goto(`${P}/-/issues/new`);
  await page.getByLabel("Title").fill(`React ${Date.now()}`);
  await page.getByRole("button", { name: "Create issue" }).click();
  await at(page, new RegExp(`^${P}/-/issues/\\d+$`));
  await page.locator(".composer textarea").fill("A comment");
  await page.getByRole("button", { name: "Comment", exact: true }).click();
  await expect(page.locator(".thread.plain", { hasText: "A comment" })).toBeVisible();

  // on the issue
  const bars = page.getByRole("group", { name: "Reactions" });
  await bars.first().getByRole("button", { name: "Add a reaction" }).click();
  await page.getByRole("button", { name: "tada" }).click();
  const tada = bars.first().getByRole("button", { name: "tada: 1" });
  await expect(tada).toHaveAttribute("aria-pressed", "true");
  // on the comment
  const comment = page.locator(".thread.plain", { hasText: "A comment" });
  await comment.getByRole("button", { name: "Add a reaction" }).click();
  await page.getByRole("button", { name: "heart" }).click();
  await expect(comment.getByRole("button", { name: "heart: 1" })).toBeVisible();
  // taken back
  await tada.click();
  await expect(bars.first().getByRole("button", { name: /tada/ })).toHaveCount(0);
  await page.reload();
  await expect(comment.getByRole("button", { name: "heart: 1" })).toBeVisible();
});
