/** Your settings' pages, in the order the sidebar lists them when you are in them; each
 *  one takes its title from here. `path` is under /profile ("" is the profile itself). */
export const PROFILE_PAGES: { path: string; label: string }[] = [
  { path: "", label: "Profile" },
  { path: "keys", label: "SSH keys" },
  { path: "gpg_keys", label: "GPG keys" },
  { path: "tokens", label: "Access tokens" },
  { path: "security", label: "Password & 2FA" },
  { path: "notifications", label: "Notifications" },
];

/** The page of /profile/… a path is on (the profile itself for /profile). */
export function profilePage(pathname: string) {
  const rest = pathname.replace(/^\/profile\/?/, "").split("/")[0];
  return PROFILE_PAGES.find((p) => p.path === rest);
}
