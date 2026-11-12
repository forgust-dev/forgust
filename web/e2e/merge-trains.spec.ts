import { test, expect, freshProject } from "./fixtures";

// With merge trains on, the merge box puts the merge request on the train
// and shows its place; Remove takes it off. A project of its own, so the shared
// one keeps merging directly for the other specs.

test("merge trains: turn on, join, leave", async ({ page }) => {
  const { project, web, api, remove } = await freshProject(page, "trains", { ".gitlab-ci.yml": "test:\n  script: [make test]\n" });
  try {
    await api("post", `/projects/${project.id}/repository/commits`, {
      branch: "feature",
      start_branch: "main",
      commit_message: "a feature",
      actions: [{ action: "create", file_path: "feature.txt", content: "x\n" }],
    });
    const mr = await api("post", `/projects/${project.id}/merge_requests`, { source_branch: "feature", title: "A feature" });

    // the setting needs "pipelines must succeed"
    await page.goto(`${web}/-/settings/ci_cd`);
    const trains = page.getByRole("checkbox", { name: /merge trains/ });
    await expect(trains).toBeDisabled();
    await page.getByRole("checkbox", { name: /only be merged when the pipeline succeeds/ }).check();
    await trains.check();
    await page.getByRole("button", { name: "Save", exact: true }).first().click();
    await expect.poll(async () => (await api("get", `/projects/${project.id}/ci/settings`)).merge_trains_enabled).toBe(true);

    await page.goto(`${web}/-/merge_requests/${mr.iid}`);
    await page.getByRole("button", { name: "Add to merge train" }).click();
    // no runner: the car waits for its pipeline, first in line
    await expect(page.getByText("Merges next")).toBeVisible();
    await page.getByRole("button", { name: "Remove", exact: true }).click();
    await expect(page.getByRole("button", { name: "Add to merge train" })).toBeVisible();
  } finally {
    await remove();
  }
});
