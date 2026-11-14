import { test, expect, P, ADMIN, apiCall, at, csrfHeaders, freshProject, isMobile } from "./fixtures";

const MR = `${P}/-/merge_requests`;

async function details(page: import("@playwright/test").Page) {
  const aside = page.getByRole("complementary", { name: "Merge request details" });
  await expect(aside).toBeVisible();
  return aside;
}

/** PUT on the API as the signed-in admin (CSRF token from the session). */
async function apiPut(page: import("@playwright/test").Page, path: string, data: unknown) {
  const r = await page.request.put(`/api/v1${path}`, { data, headers: await csrfHeaders(page) });
  expect(r.ok(), await r.text()).toBe(true);
}

test("assign and request a review from the sidebar", async ({ page }) => {
  // other tests comment on !1 (a comment marks a reviewer "reviewed"): start clean
  const project = await (await page.request.get(`/api/v1/resolve?path=${P.slice(1)}`)).json();
  await apiPut(page, `/projects/${project.id}/merge_requests/1`, { assignee_ids: [], reviewer_ids: [] });
  await page.goto(`${MR}/1`);
  const aside = await details(page);
  const assignees = aside.locator(".fact", { hasText: "Assignees" });
  await assignees.getByRole("button", { name: "Edit" }).click();
  await page.getByRole("option", { name: /root/ }).click();
  await expect(assignees.locator(".person")).toContainText("root");
  await page.keyboard.press("Escape");
  // clicking again unassigns
  await assignees.getByRole("button", { name: "Edit" }).click();
  await page.getByRole("option", { name: /root/ }).click();
  await expect(assignees).toContainText("None");
  await page.keyboard.press("Escape");
  // the author cannot be a reviewer that approves, but can be asked to review
  const reviewers = aside.locator(".fact", { hasText: "Reviewers" });
  await reviewers.getByRole("button", { name: "Edit" }).click();
  await page.getByRole("option", { name: /root/ }).click();
  await expect(reviewers.locator(".person")).toContainText("root");
  await expect(reviewers.getByLabel("Review requested")).toBeVisible();
});

/** !2 on !1 (the review tests close and reopen !1, which takes the stack apart). */
async function restack(page: import("@playwright/test").Page) {
  const csrf = await csrfHeaders(page);
  const project = await (await page.request.get(`/api/v1/resolve?path=${P.slice(1)}`)).json();
  const path = `/api/v1/projects/${project.id}/merge_requests`;
  for (const iid of [1, 2]) {
    const m = await (await page.request.get(`${path}/${iid}`)).json();
    if (m.state === "closed") await page.request.post(`${path}/${iid}/reopen`, { headers: csrf });
  }
  const two = await (await page.request.get(`${path}/2`)).json();
  if (!two.stack) {
    const r = await page.request.put(`${path}/2/stack`, { data: { on_iid: 1 }, headers: csrf });
    expect(r.ok(), await r.text()).toBe(true);
  }
}

test("the stack map shows both layers and what merges next", async ({ page }) => {
  await restack(page);
  await page.goto(`${MR}/2`);
  const map = page.getByRole("region", { name: "Stack" });
  await expect(map).toContainText("layer 2 of 2");
  await expect(map.locator("li")).toHaveCount(2);
  await expect(map.locator("li.current")).toContainText("Checkout page");
  await expect(map.locator("li", { hasText: "Checkout with discounts" })).toContainText("merges next");
  // the upper layer cannot merge before the one below
  await expect(page.getByText("merge !1 first")).toBeVisible();
  await map.locator("li", { hasText: "Checkout with discounts" }).getByRole("link").click();
  await expect(page.getByRole("region", { name: "Stack" })).toContainText("layer 1 of 2");
  if (!isMobile(page)) {
    await page.goto(MR);
    await expect(page.locator(".mr-row", { hasText: "Checkout page" }).locator(".stack-chip")).toHaveText("2/2");
  }
});

test("your work lists your merge requests; projects moved to their own page", async ({ page }) => {
  await page.goto("/");
  await expect(page.getByRole("heading", { name: "Your work" })).toBeVisible();
  await page.getByRole("navigation", { name: "Your work" }).getByRole("button", { name: /Merge requests/ }).click();
  const yours = page.locator("section", { has: page.getByRole("heading", { name: "Opened by you" }) });
  await expect(yours.getByRole("link", { name: "Checkout with discounts" })).toBeVisible();
  await yours.getByRole("link", { name: "Checkout with discounts" }).click();
  await expect(page.getByRole("heading", { name: "Checkout with discounts" })).toBeVisible();
  await page.goto("/dashboard/projects");
  await expect(page.getByRole("heading", { name: "Your projects" })).toBeVisible();
});

test("your work keeps review requests you already reviewed apart from those waiting", async ({ page }) => {
  const call = await apiCall(page);
  const project = await call("get", `/resolve?path=${P.slice(1)}`);
  const me = await call("get", `/users/${ADMIN}`);
  const mr2 = `/projects/${project.id}/merge_requests/2`;
  const section = (name: string) => page.locator("section", { has: page.getByRole("heading", { name }) });
  const reviewRequests = async () => {
    await page.goto("/");
    await page.getByRole("navigation", { name: "Your work" }).getByRole("button", { name: /Review requests/ }).click();
  };
  // !2 is shared: whatever happens, leave no reviewer and no open thread behind
  let note: { id: number } | null = null;
  try {
    // asked afresh: the request waits for the admin
    await call("put", mr2, { reviewer_ids: [] });
    await call("put", mr2, { reviewer_ids: [me.id] });
    await reviewRequests();
    await expect(section("Waiting for your review").getByRole("link", { name: "Checkout page" })).toBeVisible();
    // a comment marks it reviewed: it moves to its own section, out of the count
    note = await call("post", `${mr2}/notes`, { body: "Looked at it." });
    await reviewRequests();
    await expect(section("Reviewed by you").getByRole("link", { name: "Checkout page" })).toBeVisible();
    await expect(section("Waiting for your review").getByRole("link", { name: "Checkout page" })).toHaveCount(0);
  } finally {
    if (note) await call("delete", `${mr2}/notes/${note.id}`);
    await call("put", mr2, { reviewer_ids: [] });
  }
});

test("references in a comment preview link to what they name", async ({ page }) => {
  await page.goto(`${MR}/2`);
  const box = page.getByRole("textbox", { name: /comment/i }).last();
  await box.fill("Builds on !1, cc @root");
  await page.getByRole("tab", { name: "Preview" }).last().click();
  const preview = page.locator(".markdown", { hasText: "Builds on" }).last();
  await expect(preview.locator("a.ref-mr")).toHaveAttribute("href", `${P}/-/merge_requests/1`);
  await expect(preview.locator("a.user-mention")).toHaveText("@root");
});

test("a user's page shows the contribution calendar and recent activity", async ({ page }) => {
  // an event of its own, newest in the list whatever ran before
  const { project, api, remove } = await freshProject(page, "activity", { "README.md": "# Activity\n" });
  const title = `Activity check ${Date.now() % 1_000_000}`;
  await api("post", `/projects/${project.id}/issues`, { title });
  try {
    await page.goto(`/${ADMIN}`);
    const cal = page.getByRole("grid", { name: "Contributions per day" });
    await expect(cal).toBeVisible();
    await expect(page.locator(".contrib-caption")).toContainText(/[1-9]\d* contributions? in the last year/);
    await expect(cal.locator(".contrib-cell.l0, .contrib-cell.l1, .contrib-cell.l2, .contrib-cell.l3, .contrib-cell.l4").first()).toBeVisible();
    await expect(page.locator(".contrib-cell:not(.l0):not(.future)").first()).toBeAttached();
    const row = page.locator(".activity-list li", { has: page.getByRole("link", { name: title }) });
    await expect(row).toContainText("opened");
  } finally {
    await remove();
  }
});

test("set auto-merge while the pipeline runs, then cancel it", async ({ page }) => {
  await page.goto(`${MR}/1`);
  const box = page.locator(".readiness");
  await box.getByRole("button", { name: "Set to auto-merge" }).click();
  await expect(page.locator(".toast.ok").last()).toContainText("auto-merge");
  await expect(box.locator(".auto-merge-on")).toContainText("Set to auto-merge by root");
  await box.locator(".auto-merge-on").getByRole("button", { name: "Cancel" }).click();
  await expect(box.locator(".auto-merge-on")).toHaveCount(0);
  await expect(box.getByRole("button", { name: "Set to auto-merge" })).toBeVisible();
});

test("usernames and the user menu lead to profiles", async ({ page }) => {
  // an author's name on a merge request
  await page.goto(`${MR}/1`);
  await page.locator(".mr-status").getByRole("link", { name: "root" }).click();
  await expect(page.getByRole("heading", { name: "root", level: 1 })).toBeVisible();
  await expect(page.locator(".profile-sub")).toContainText("@root");
  await expect(page.getByRole("link", { name: "Edit profile" })).toBeVisible();
  await expect(page.locator(".contrib")).toBeVisible();
  // the name at the top of the user menu
  await page.goto("/explore");
  await page.locator(".avatar-btn").click();
  await page.getByRole("menuitem", { name: /@root/ }).click();
  await expect(page.locator(".profile-sub")).toContainText("@root");
  await page.locator(".avatar-btn").click();
  await page.getByRole("menuitem", { name: "Settings" }).click();
  await expect(page).toHaveURL(/\/profile$/);
});

test("@ and ! suggest people and merge requests while typing", async ({ page }) => {
  await page.goto(`${MR}/2`);
  const box = page.getByRole("textbox", { name: /comment/i }).last();
  await box.click();
  await box.pressSequentially("cc @ro");
  const list = page.getByRole("listbox", { name: "Suggestions" });
  await expect(list.getByRole("option", { name: /root/ })).toBeVisible();
  await page.keyboard.press("Enter");
  await expect(box).toHaveValue("cc @root ");
  await box.pressSequentially("see !");
  await expect(list.getByRole("option", { name: /Checkout with discounts/ })).toBeVisible();
  // pick !1 with the arrows, wherever it sits in the list
  const options = await list.getByRole("option").allInnerTexts();
  const at = options.findIndex((t) => t.includes("Checkout with discounts"));
  for (let i = 0; i < at; i++) await page.keyboard.press("ArrowDown");
  await page.keyboard.press("Tab");
  await expect(box).toHaveValue("cc @root see !1 ");
  // Escape closes the list and keeps the text
  await box.pressSequentially("#");
  await page.keyboard.press("Escape");
  await expect(list).toHaveCount(0);
});

test("hovering a reference or a name shows a card", async ({ page }) => {
  test.skip(isMobile(page), "hover is a pointer feature");
  await page.goto(`${MR}/2`);
  const box = page.getByRole("textbox", { name: /comment/i }).last();
  await box.fill("Builds on !1, cc @root");
  await page.getByRole("tab", { name: "Preview" }).last().click();
  const preview = page.locator(".markdown", { hasText: "Builds on" }).last();
  await preview.locator("a.ref-mr").hover();
  const card = page.getByRole("tooltip");
  await expect(card).toContainText("Checkout with discounts");
  await expect(card).toContainText("feature/checkout");
  await expect(card).toContainText("main");
  await page.mouse.move(0, 0);
  await expect(card).toHaveCount(0);
  await preview.locator("a.user-mention").hover();
  await expect(page.getByRole("tooltip")).toContainText("@root");
  await expect(page.getByRole("tooltip")).toContainText("Member since");
  await page.mouse.move(0, 0);
  // branches (their chips) and commits
  await page.locator("a.ref-chip", { hasText: "feature/checkout" }).first().hover();
  await expect(page.getByRole("tooltip")).toContainText("Branch");
  await expect(page.getByRole("tooltip")).toContainText("feature/checkout");
  await page.mouse.move(0, 0);
  await page.goto(P);
  await page.locator(`a[href*="/-/commit/"]`).first().hover();
  await expect(page.getByRole("tooltip")).toContainText("Commit");
  await expect(page.getByRole("tooltip")).toContainText("authored");
});

test("suggest a change on a line, then apply it", async ({ page }) => {
  test.skip(isMobile(page), "the file list is desktop-only");
  await page.goto(`${MR}/1/diffs`);
  await page.locator(".diff-tree-item", { hasText: "app.ts" }).click();
  const line = page.locator(".diff-line.add", { hasText: "export const VERSION = 2;" }).first();
  await line.hover();
  await line.getByRole("button", { name: "Comment on this line" }).click();
  await page.getByRole("button", { name: "Suggest a change" }).click();
  const box = page.getByRole("textbox", { name: "Line comment" });
  await expect(box).toHaveValue("```suggestion\nexport const VERSION = 2;\n```\n");
  // same text: applying it changes nothing in the fixture other tests read
  await page.getByRole("button", { name: "Comment", exact: true }).click();
  const thread = page.locator(".thread", { has: page.locator("code[class^='language-suggestion']") }).last();
  await expect(thread.locator("pre")).toBeVisible();
  await thread.getByRole("button", { name: "Apply suggestion" }).click();
  await expect(page.locator(".toast.ok").last()).toContainText("Suggestion committed to feature/checkout");
  await expect(thread.locator(".suggestion-state")).toContainText("Applied in");
});

test("j/k move a line cursor, J extends it, c comments on the lines", async ({ page }) => {
  test.skip(isMobile(page), "keyboard review is a desktop flow");
  // !2 (one small file): its threads stay out of !1's n/p test
  await page.goto(`${MR}/2/diffs`);
  await expect(page.locator(".diff-line.add").first()).toBeVisible();
  await page.keyboard.press("j");
  await expect(page.locator(".diff-line.cursor")).toHaveCount(1);
  await page.keyboard.press("Shift+J");
  await page.keyboard.press("Shift+J");
  await expect(page.locator(".diff-line.selected")).toHaveCount(3);
  await page.keyboard.press("c");
  const composer = page.locator("form.thread", { has: page.getByRole("textbox", { name: "Line comment" }) });
  await expect(composer).toContainText(/Commenting on lines \d+–\d+/);
  await composer.getByRole("textbox", { name: "Line comment" }).fill("three lines at once");
  await composer.getByRole("button", { name: "Comment", exact: true }).click();
  await expect(page.locator(".thread-head", { hasText: /checkout\.ts:\d+–\d+/ }).first()).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(page.locator(".diff-line.selected")).toHaveCount(0);
});

test("drag over line numbers selects a range", async ({ page }) => {
  test.skip(isMobile(page), "dragging is a pointer feature");
  // the file view: #L3-7
  await page.goto(`${P}/-/blob/main/long.txt`);
  const ln = (n: number) => page.locator(`#L${n} .ln a`);
  await ln(3).hover();
  await page.mouse.down();
  await ln(5).hover();
  await ln(7).hover();
  await page.mouse.up();
  await expect(page).toHaveURL(/#L3-7$/);
  await expect(page.locator(".code-line.target")).toHaveCount(5);
  // a diff: dragging over two lines opens a comment on both
  await page.goto(`${MR}/1/diffs`);
  const rows = page.locator(".diff-line.add");
  await expect(rows.nth(1)).toBeVisible();
  await rows.nth(0).locator(".n").last().hover({ position: { x: 30, y: 10 } });
  await page.mouse.down();
  await rows.nth(1).locator(".n").last().hover({ position: { x: 30, y: 10 } });
  await page.mouse.up();
  await expect(page.locator("form.thread")).toContainText(/Commenting on lines \d+–\d+/);
});

test("forgot password, reset and email links", async ({ browser }) => {
  // signed out
  const ctx = await browser.newContext({ storageState: { cookies: [], origins: [] } });
  const page = await ctx.newPage();
  await page.goto("/login");
  await page.getByLabel("Username or email").fill("root");
  await page.getByRole("link", { name: "Forgot your password?" }).click();
  await expect(page.getByLabel("Username or email")).toHaveValue("root");
  await page.getByRole("button", { name: "Send the link" }).click();
  await expect(page.getByText(/If an account matches “root”, we sent a link/)).toBeVisible();
  // a link that is not (or no longer) valid says so
  await page.goto("/reset-password?token=not-a-real-token");
  await page.getByLabel("New password").fill("a-long-new-password");
  await page.getByLabel("Repeat it").fill("a-long-new-password-x");
  await expect(page.getByText("The two passwords differ.")).toBeVisible();
  await page.getByLabel("Repeat it").fill("a-long-new-password");
  await page.getByRole("button", { name: "Set the password" }).click();
  await expect(page.locator(".alert, .error-box, [role=alert]").first()).toContainText(/invalid or has expired/);
  // signed out, a confirmation link asks to sign in first
  await page.goto("/verify-email?token=not-a-real-token");
  await expect(page.getByRole("link", { name: "Sign in to confirm" })).toHaveAttribute("href", /\/login\?next=%2Fverify-email%3Ftoken%3Dnot-a-real-token/);
  await ctx.close();
});

// a 2x2 red PNG
const PNG = Buffer.from(
  "iVBORw0KGgoAAAANSUhEUgAAAAIAAAACCAIAAAD91JpzAAAAEElEQVR4nGO4o6YGRAwQCgAjRgSh7v9IEwAAAABJRU5ErkJggg==",
  "base64",
);

test("upload a picture, see it next to your name, remove it", async ({ page }) => {
  await page.goto("/profile");
  const section = page.locator("section", { has: page.getByRole("heading", { name: "Picture" }) });
  await section.getByLabel("Picture file").setInputFiles({ name: "me.png", mimeType: "image/png", buffer: PNG });
  await expect(page.locator(".toast.ok").last()).toContainText("Picture updated");
  // the top-right menu shows it, and so do other places by username
  await expect(page.locator(".avatar-btn .avatar img.shown")).toBeVisible();
  await page.goto("/root");
  await expect(page.locator(".profile-head .avatar img.shown")).toBeVisible();
  await page.goto("/profile");
  await section.getByRole("button", { name: "Remove" }).click();
  await expect(page.locator(".toast.ok").last()).toContainText("Picture removed");
  await expect(page.locator(".avatar-btn .avatar img")).toHaveCount(0);
});

test("edit a file in the browser onto a new branch, add and delete files, manage branches", async ({ page }) => {
  const project = await (await page.request.get(`/api/v1/resolve?path=${P.slice(1)}`)).json();
  const csrf = await csrfHeaders(page);
  const branch = `e2e-edit-${Date.now() % 100000}`;
  try {
    // edit README on main, committed to a new branch → the new merge request form
    await page.goto(`${P}/-/blob/main/README.md`);
    await page.getByRole("link", { name: "Edit this file" }).click();
    const editor = page.getByRole("textbox", { name: "File content" });
    await expect(editor).toHaveValue(/A fixture project/);
    await editor.fill("# Shop\n\nEdited in the browser.\n");
    await page.getByRole("radio", { name: /new branch and open a merge request/ }).check();
    await page.getByRole("textbox", { name: "New branch name" }).fill(branch);
    await page.getByRole("button", { name: "Commit changes" }).click();
    await expect(page.getByRole("heading", { name: "New merge request" })).toBeVisible();
    await expect(page).toHaveURL(new RegExp(`source=${branch}`));
    // a new file, straight on that branch
    await page.goto(`${P}/-/tree/${branch}/docs`);
    await page.getByRole("link", { name: /New file/ }).click();
    await page.getByLabel("File path").fill("docs/added.md");
    await page.getByRole("textbox", { name: "File content" }).fill("Added\n");
    await page.getByRole("button", { name: "Commit changes" }).click();
    await expect(page.getByText("Added", { exact: true })).toBeVisible();
    // deleted again
    await page.getByRole("link", { name: "Delete this file" }).click();
    await page.getByRole("button", { name: "Delete the file" }).click();
    await expect(page.getByRole("link", { name: "added.md" })).toHaveCount(0);
    // the branches page: create and delete one
    await page.goto(`${P}/-/branches`);
    await page.getByRole("button", { name: "New branch" }).click();
    await page.getByLabel("Branch name").fill(`${branch}-b`);
    await page.getByRole("button", { name: "Create branch" }).click();
    await expect(page.locator(".toast.ok").last()).toContainText("created");
    page.once("dialog", (d) => d.accept());
    await page.getByRole("button", { name: `Delete branch ${branch}-b` }).click();
    await expect(page.getByRole("link", { name: `${branch}-b`, exact: true })).toHaveCount(0);
  } finally {
    // keep the "recently pushed branches" banner of the other tests intact
    for (const b of [branch, `${branch}-b`]) await page.request.delete(`/api/v1/projects/${project.id}/repository/branches/${b}`, { headers: csrf });
  }
});

test("tag, release with a file, download it, edit and delete", async ({ page }) => {
  const project = await (await page.request.get(`/api/v1/resolve?path=${P.slice(1)}`)).json();
  const csrf = await csrfHeaders(page);
  const tag = `e2e-v${Date.now() % 100000}`;
  try {
    // a tag from the Tags page
    await page.goto(`${P}/-/tags`);
    await page.getByRole("button", { name: "New tag" }).click();
    await page.getByLabel("Tag name").fill(tag);
    await page.getByLabel(/^Message/).fill("Annotated");
    await page.getByRole("button", { name: "Create tag" }).click();
    await expect(page.locator(".toast.ok").last()).toContainText("created");
    // its release, with notes (and a file attached in them) and a release file
    await page.getByRole("link", { name: "Create release" }).first().click();
    await expect(page.getByRole("heading", { name: "New release" })).toBeVisible();
    await page.getByLabel(/^Tag/).first().fill(tag);
    await page.getByLabel(/^Title/).fill(`Shop ${tag}`);
    await page.getByRole("textbox", { name: "Release notes" }).fill("First cut.\n");
    await page.getByLabel("Files to attach").setInputFiles({ name: "shot.png", mimeType: "image/png", buffer: Buffer.from("png") });
    await expect(page.getByRole("textbox", { name: "Release notes" })).toHaveValue(/!\[shot\.png\]\(\/-\/project\/\d+\/uploads\//);
    await page.getByLabel("Release files").setInputFiles({ name: "shop.tar.gz", mimeType: "application/gzip", buffer: Buffer.from("tarball bytes") });
    await expect(page.getByText("shop.tar.gz")).toBeVisible();
    await page.getByRole("button", { name: "Create release" }).click();
    await expect(page.getByRole("heading", { name: `Shop ${tag}` })).toBeVisible();
    // the asset's permanent link serves the file
    const href = await page.getByRole("link", { name: "shop.tar.gz" }).getAttribute("href");
    expect(href).toContain(`/-/releases/${tag}/downloads/shop.tar.gz`);
    const file = await page.request.get(href!);
    expect(await file.text()).toBe("tarball bytes");
    // listed, and shown on its tag
    await page.goto(`${P}/-/releases`);
    await expect(page.getByRole("article", { name: `Release Shop ${tag}` })).toBeVisible();
    await page.goto(`${P}/-/tags`);
    await expect(page.getByRole("link", { name: `Shop ${tag}` })).toBeVisible();
    // edited
    await page.getByRole("link", { name: `Shop ${tag}` }).click();
    await page.getByRole("link", { name: `Edit release Shop ${tag}` }).click();
    await page.getByLabel(/^Title/).fill(`Shop ${tag} final`);
    await page.getByRole("button", { name: "Save changes" }).click();
    await expect(page.getByRole("heading", { name: `Shop ${tag} final` })).toBeVisible();
    // deleted: the tag stays
    page.once("dialog", (d) => d.accept());
    await page.getByRole("button", { name: `Delete release Shop ${tag} final` }).click();
    await expect(page).toHaveURL(new RegExp(`${P}/-/releases$`));
    await expect(page.getByRole("article", { name: `Release Shop ${tag} final` })).toHaveCount(0);
  } finally {
    await page.request.delete(`/api/v1/projects/${project.id}/repository/tags/${tag}`, { headers: csrf });
  }
});

test("choose notification levels: a default, and per project", async ({ page }) => {
  const csrf = await csrfHeaders(page);
  const project = await (await page.request.get(`/api/v1/resolve?path=${P.slice(1)}`)).json();
  try {
    await page.goto("/profile/notifications");
    const group = page.getByRole("radiogroup", { name: "Default notification level" });
    await expect(group.getByRole("radio", { name: /Participate/ })).toBeChecked();
    await group.getByRole("radio", { name: /On mention/ }).check();
    await expect(page.locator(".toast.ok").last()).toContainText("On mention");
    await page.reload();
    await expect(group.getByRole("radio", { name: /On mention/ })).toBeChecked();

    await page.goto(P);
    await page.getByRole("button", { name: "Notifications: On mention" }).click();
    await page.getByRole("menuitem", { name: /^Watch/ }).click();
    await expect(page.getByRole("button", { name: "Notifications: Watch" })).toBeVisible();
    await page.reload();
    await expect(page.getByRole("button", { name: "Notifications: Watch" })).toBeVisible();
  } finally {
    await page.request.put("/api/v1/user/notification_settings", { data: { level: "participating" }, headers: csrf });
    await page.request.put(`/api/v1/projects/${project.id}/notification_settings`, { data: { level: "global" }, headers: csrf });
  }
});

test("protect tags in settings; protected tags are marked", async ({ page }) => {
  const csrf = await csrfHeaders(page);
  const project = await (await page.request.get(`/api/v1/resolve?path=${P.slice(1)}`)).json();
  const pattern = `e2e-p${Date.now() % 100000}-*`;
  const tag = pattern.replace("*", "1");
  try {
    await page.goto(`${P}/-/settings`);
    await page.getByLabel(/^Tag or pattern/).fill(pattern);
    await page.getByRole("button", { name: "Protect tags" }).click();
    await expect(page.locator('[aria-label="Protected tags"]')).toContainText(pattern);
    await page.request.post(`/api/v1/projects/${project.id}/repository/tags`, { headers: csrf, data: { tag_name: tag, ref: "main" } });
    await page.goto(`${P}/-/tags`);
    await expect(page.locator(".ref-row", { hasText: tag }).locator(".pill.warn")).toHaveText("protected");
  } finally {
    await page.request.delete(`/api/v1/projects/${project.id}/repository/tags/${tag}`, { headers: csrf });
    const rules = await (await page.request.get(`/api/v1/projects/${project.id}/protected_tags`)).json();
    for (const r of rules) if (r.name === pattern) await page.request.delete(`/api/v1/projects/${project.id}/protected_tags/${r.id}`, { headers: csrf });
  }
});

test("an unsubscribe link that is not valid says so", async ({ page }) => {
  await page.goto("/unsubscribe?u=1&p=1&s=forged");
  await expect(page.getByText("This link is not valid any more.")).toBeVisible();
});

test("an unsubscribe link cannot point the page at another API route", async ({ page }) => {
  const posts: string[] = [];
  page.on("request", (r) => r.method() === "POST" && posts.push(r.url()));
  await page.goto("/unsubscribe?u=1&p=1&s=..%2F..%2Fprojects%2F1%2Fhooks");
  await expect(page.getByText("This link is not valid any more.")).toBeVisible();
  await expect(page.getByRole("button", { name: /Stop emails/ })).toHaveCount(0);
  expect(posts).toEqual([]);
});

test("the bell: a failed pipeline arrives live, and each kind has its switches", async ({ page }) => {
  const csrf = await csrfHeaders(page);
  await page.request.post("/api/v1/user/notifications/read", { headers: csrf });
  await page.goto("/");
  await expect(page.getByRole("button", { name: "Notifications", exact: true })).toBeVisible();
  // a configuration that does not load: the pipeline of the push is born failed
  const fresh = await freshProject(page, "bell", { ".gitlab-ci.yml": "job:\n  script: x\n  secrets:\n    DB: {vault: db/pw}\n" });
  try {
    // no reload: the event stream brings it, and it shows on screen
    await expect(page.locator(".toast.notice").last()).toContainText("failed: Pipeline #1 on main");
    await page.getByRole("button", { name: "Notifications: 1 unread" }).click();
    const item = page.locator(".inbox").getByRole("link", { name: /Pipeline #1 on main/ });
    await expect(item).toContainText("failed");
    await item.click();
    await at(page, `${fresh.web}/-/pipelines/1`);
    // the click focused the link (its hover card is asked for) and the list closed
    // under it: no card may show up in a corner afterwards
    await page.waitForTimeout(700);
    await expect(page.locator(".hover-card")).toHaveCount(0);
    await expect(page.getByRole("button", { name: "Notifications", exact: true })).toBeVisible();

    await page.goto("/profile/notifications");
    const email = page.getByRole("checkbox", { name: "Failed pipelines: email" });
    await expect(email).toBeChecked();
    await expect(page.getByRole("checkbox", { name: "Fixed pipelines: email" })).not.toBeChecked();
    await email.uncheck();
    await expect(email).not.toBeChecked();
    await page.reload();
    await expect(email).not.toBeChecked();
    await expect(page.getByRole("checkbox", { name: "Failed pipelines: in app" })).toBeChecked();
  } finally {
    await page.request.put("/api/v1/user/notification_channels", { data: { kind: "pipeline_failed", channel: "email", enabled: true }, headers: csrf });
    await fresh.remove();
  }
});
