// The licenses of what ships in the web app: every package `npm ci` installs that is no
// devDependency (the bundle is made of them; Vite, TypeScript and Playwright only build and test
// it). ADR-0007, which cargo-deny applies to the crates (../deny.toml): every dependency is under
// a license compatible with MIT, or is named below with the reason it is fine.
import { readFileSync } from "node:fs";

/** As deny.toml allows them. */
const ALLOWED = new Set([
  "MIT",
  "MIT-0",
  "0BSD",
  "Apache-2.0",
  "Apache-2.0 WITH LLVM-exception",
  "BSD-2-Clause",
  "BSD-3-Clause",
  "ISC",
  "Zlib",
  "Unicode-3.0",
  "Unicode-DFS-2016",
  "CC0-1.0",
  "CDLA-Permissive-2.0",
  "BSL-1.0",
  "MPL-2.0",
]);

/** Packages under another license, and the license each must still declare: a change is looked at again. */
const EXCEPTIONS = {
  // the Smart View's layout, and mermaid's: shipped unmodified, so only its own files are under
  // the EPL; its notice ships with it, its source is https://github.com/kieler/elkjs
  elkjs: "EPL-2.0 OR GPL-3.0-or-later",
  // fonts, which the OFL lets ship inside an application
  "@fontsource-variable/bricolage-grotesque": "OFL-1.1",
  "@fontsource-variable/inter": "OFL-1.1",
  "@fontsource/jetbrains-mono": "OFL-1.1",
  // public domain, more permissive than MIT (d3-delaunay's, through mermaid)
  "robust-predicates": "Unlicense",
};

/** Packages whose package.json names no license, and the one their LICENSE file gives. */
const CLARIFY = {
  khroma: "MIT",
};

/** Whether an SPDX expression is allowed: `A OR B` either, `A AND B` both, parentheses group. */
function allowed(expression) {
  const tokens = expression.replace(/[()]/g, (p) => ` ${p} `).split(/\s+/).filter(Boolean);
  let i = 0;
  const atom = () => {
    if (tokens[i] === "(") {
      i++;
      const v = or();
      i++;
      return v;
    }
    let id = tokens[i++];
    if (tokens[i] === "WITH") {
      id += ` WITH ${tokens[i + 1]}`;
      i += 2;
    }
    return ALLOWED.has(id);
  };
  const and = () => {
    let v = atom();
    while (tokens[i] === "AND") {
      i++;
      v = atom() && v;
    }
    return v;
  };
  const or = () => {
    let v = and();
    while (tokens[i] === "OR") {
      i++;
      v = and() || v;
    }
    return v;
  };
  return or();
}

const lock = JSON.parse(readFileSync(new URL("../package-lock.json", import.meta.url), "utf8"));
const problems = [];
const seen = new Set();
let shipped = 0;
for (const [path, p] of Object.entries(lock.packages)) {
  // the app itself, and what only builds or tests it
  if (!path || p.dev || p.devOptional) continue;
  const name = path.slice(path.lastIndexOf("node_modules/") + "node_modules/".length);
  const declared = typeof p.license === "string" ? p.license : p.license?.type;
  const license = declared ?? CLARIFY[name];
  shipped++;
  seen.add(name);
  if (name in EXCEPTIONS) {
    if (license !== EXCEPTIONS[name]) problems.push(`${name}@${p.version}: ${license ?? "no license"}, where its exception says ${EXCEPTIONS[name]}`);
  } else if (!license) {
    problems.push(`${name}@${p.version}: no license (name the one its LICENSE file gives in CLARIFY)`);
  } else if (!allowed(license)) {
    problems.push(`${name}@${p.version}: ${license}`);
  }
}
const stale = [...Object.keys(EXCEPTIONS), ...Object.keys(CLARIFY)].filter((n) => !seen.has(n));
if (stale.length) console.warn(`No longer shipped, to take out of scripts/licenses.mjs: ${stale.join(", ")}`);
if (problems.length) {
  console.error(`Licenses ADR-0007 does not allow (name them in scripts/licenses.mjs with a reason, or do without the package):\n  ${problems.join("\n  ")}`);
  process.exit(1);
}
console.log(`${shipped} packages ship, each under a license ADR-0007 allows`);
