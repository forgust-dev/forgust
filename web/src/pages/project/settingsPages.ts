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
