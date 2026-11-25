/**
 * The pages of a project's settings, in sidebar order: `settings` (general, no slug)
 * and `settings/<slug>`. The sidebar, the breadcrumbs and the routing all read this;
 * `Settings.tsx` maps each slug to its page.
 */
export const SETTINGS_PAGES = [
  { slug: "", label: "General" },
  { slug: "repository", label: "Repository" },
  { slug: "merge_requests", label: "Merge requests" },
  { slug: "ci_cd", label: "CI/CD" },
  { slug: "automation", label: "Automation" },
  { slug: "integrations", label: "Integrations" },
] as const;

export type SettingsSlug = (typeof SETTINGS_PAGES)[number]["slug"];

/** The settings page at `settings/<slug>` (`""` for the general one), if there is one. */
export function settingsPage(slug: string) {
  return SETTINGS_PAGES.find((s) => s.slug === slug);
}

/**
 * Every section of the settings pages, by page: its anchor (`settings/<slug>#<id>`, the id its
 * section carries), its name and the words it is found by in the Ctrl+K palette. `own`: for the
 * project's Owners only.
 */
export const SETTINGS_SECTIONS: { slug: SettingsSlug; id: string; label: string; words?: string; own?: true }[] = [
  { slug: "", id: "picture", label: "Picture", words: "avatar image logo" },
  { slug: "", id: "general", label: "Name and visibility", words: "description default branch private public internal" },
  { slug: "", id: "storage", label: "Storage", words: "quota size disk usage" },
  { slug: "", id: "path", label: "Path and place", words: "rename move transfer address url", own: true },
  { slug: "", id: "danger-zone", label: "Danger zone", words: "archive unarchive delete remove", own: true },
  { slug: "repository", id: "protected-branches", label: "Protected branches", words: "protect push force" },
  { slug: "repository", id: "protected-tags", label: "Protected tags", words: "protect release" },
  { slug: "repository", id: "push-rules", label: "Push rules", words: "commit messages regex secrets signed signatures file size" },
  { slug: "repository", id: "locked-files", label: "Locked files", words: "lfs lock" },
  // no "ssh": that is your own SSH keys, too often added as a deploy key
  { slug: "repository", id: "deploy-keys", label: "Deploy keys" },
  { slug: "repository", id: "deploy-tokens", label: "Deploy tokens", words: "clone credentials" },
  { slug: "repository", id: "mirroring", label: "Mirroring", words: "mirrors push pull sync github gitlab" },
  { slug: "merge_requests", id: "merge-checks", label: "Merge checks", words: "pipeline must succeed merge trains security findings" },
  { slug: "merge_requests", id: "approvals", label: "Approval rules", words: "approvals approve" },
  { slug: "ci_cd", id: "pipelines", label: "Pipeline configuration", words: "gitlab-ci yml timeout clone depth cancel interruptible" },
  { slug: "ci_cd", id: "runners", label: "Runners", words: "shared runners" },
  { slug: "ci_cd", id: "variables", label: "CI/CD variables", words: "variables secrets environment" },
  { slug: "ci_cd", id: "protected-environments", label: "Protected environments", words: "deploy production" },
  { slug: "ci_cd", id: "pages", label: "Pages", words: "static site website" },
  { slug: "ci_cd", id: "job-token-access", label: "Job token access", words: "ci_job_token allowlist" },
  { slug: "automation", id: "security-scanning", label: "Security scanning", words: "sast secrets dependencies sbom trivy semgrep" },
  { slug: "automation", id: "dependency-updates", label: "Dependency updates", words: "renovate registries" },
  { slug: "automation", id: "ai-review", label: "AI review", words: "model llm" },
  { slug: "automation", id: "smart-view", label: "Smart View", words: "map blocks" },
  { slug: "integrations", id: "webhooks", label: "Webhooks", words: "hooks slack discord teams matrix mattermost" },
  { slug: "integrations", id: "access-tokens", label: "Access tokens", words: "tokens api bot" },
];
