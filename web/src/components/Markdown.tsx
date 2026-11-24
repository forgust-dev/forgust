import { useEffect, useMemo, useRef } from "react";
import { TableOfContents } from "lucide-react";
import { useToast } from "../shell/toast";
import { Menu } from "./controls";

/** Markdown the server rendered and sanitized. Math (`$x$`, ```math) and Mermaid
 * diagrams are drawn here, loading KaTeX or Mermaid only on pages that have some. A code block
 * gets a button to copy it, a picture made for each theme shows the one of the theme picked here,
 * and a link to a section (`#install`) finds it, as on GitHub (its ids read `user-content-…`). */
export function Markdown({ html, className }: { html: string; className?: string }) {
  const ref = useRef<HTMLDivElement>(null);
  const toast = useToast();
  const toastRef = useRef(toast);
  toastRef.current = toast;
  useEffect(() => {
    const root = ref.current;
    if (!root) return;
    copyButtons(root);
    themePictures(root);
    watchTheme();
    const stop = revealHash(root);
    void enhance(root);
    return stop;
  }, [html]);
  useEffect(() => {
    const root = ref.current;
    if (!root) return;
    const onClick = (e: MouseEvent) => {
      const t = e.target as Element;
      const copy = t.closest<HTMLButtonElement>("button.code-copy");
      if (copy) return void copyCode(copy);
      if (e.defaultPrevented || e.metaKey || e.ctrlKey || e.shiftKey || e.altKey) return;
      const a = t.closest<HTMLAnchorElement>("a[href^='#']");
      const frag = a?.getAttribute("href")?.slice(1);
      const el = frag && sectionOf(root, frag);
      if (!a || !frag || !el) return;
      e.preventDefault();
      goToSection(el, frag);
      // a heading's own link: its address, to share
      if (a.classList.contains("anchor") && navigator.clipboard) {
        const name = a.parentElement?.textContent?.trim();
        navigator.clipboard.writeText(location.href).then(
          () => toastRef.current.ok(name ? `Link to “${name}” copied` : "Link copied"),
          () => {},
        );
      }
    };
    // an address typed or followed to a section of this text
    const onHash = () => {
      const el = sectionOf(root, location.hash.slice(1));
      if (el) el.scrollIntoView();
    };
    root.addEventListener("click", onClick);
    window.addEventListener("hashchange", onHash);
    return () => {
      root.removeEventListener("click", onClick);
      window.removeEventListener("hashchange", onHash);
    };
  }, []);
  return <div ref={ref} className={className} dangerouslySetInnerHTML={{ __html: html }} />;
}

/** What the server starts a text's ids with (render.rs `USER_CONTENT`). */
const USER = "user-content-";

/** The section a link to `#frag` means in `root`: a heading or an anchor of the text
 * (`#install` → `user-content-install`), or an element with that very id. */
function sectionOf(root: ParentNode, frag: string): HTMLElement | null {
  const name = decoded(frag);
  if (!name) return null;
  for (const n of new Set([name, name.toLowerCase()])) {
    const id = CSS.escape(n.startsWith(USER) ? n : USER + n);
    const el = root.querySelector<HTMLElement>(`[id="${id}"], a[name="${id}"]`);
    if (el) return el;
  }
  return root.querySelector<HTMLElement>(`[id="${CSS.escape(name)}"]`);
}

function decoded(frag: string) {
  try {
    return decodeURIComponent(frag);
  } catch {
    // a `%` of its own: the address as written
    return frag;
  }
}

/** Show a section of a text: its address in the bar (replacing the page's: the router keeps the
 * history), the section at the top of the window. */
export function goToSection(el: Element, frag: string) {
  history.replaceState(history.state, "", `#${frag}`);
  el.scrollIntoView();
}

/** A page opened at a section (`…/shop#install`): that section, once the text is there, kept at
 * the top while what is above it settles (its images load), until the reader moves. */
function revealHash(root: HTMLElement): (() => void) | undefined {
  const frag = location.hash.slice(1);
  if (!frag) return;
  const el = sectionOf(root, frag);
  // the page's own (`#readme`) is not the text's
  const own = document.getElementById(decoded(frag));
  if (!el || (own && !root.contains(own))) return;
  el.scrollIntoView();
  const settle = new ResizeObserver(() => el.scrollIntoView());
  settle.observe(root);
  const moves = ["wheel", "touchstart", "keydown", "pointerdown"] as const;
  const stop = () => {
    settle.disconnect();
    clearTimeout(timer);
    for (const m of moves) window.removeEventListener(m, stop);
  };
  const timer = setTimeout(stop, 3000);
  for (const m of moves) window.addEventListener(m, stop, { passive: true });
  return stop;
}

/** A button to copy each code block (not a diagram, math, or a suggested change, which have their
 * own); where the browser cannot copy (an `http://` address but localhost), none. */
function copyButtons(root: HTMLElement) {
  if (!navigator.clipboard) return;
  for (const code of root.querySelectorAll<HTMLElement>("pre > code")) {
    const pre = code.parentElement!;
    if (pre.parentElement?.classList.contains("code-block")) continue;
    if (code.matches(".language-mermaid, [data-math-style], [class^='language-suggestion']")) continue;
    const box = document.createElement("div");
    box.className = "code-block";
    pre.replaceWith(box);
    box.append(pre);
    const b = document.createElement("button");
    b.type = "button";
    b.className = "icon-btn small-icon code-copy";
    b.title = "Copy";
    b.setAttribute("aria-label", "Copy the code");
    box.append(b);
  }
}

async function copyCode(b: HTMLButtonElement) {
  const text = b.parentElement?.querySelector("pre")?.textContent ?? "";
  try {
    // without the block's last line break: a pasted command waits for Enter
    await navigator.clipboard.writeText(text.replace(/\n$/, ""));
  } catch {
    return;
  }
  b.classList.add("copied");
  b.title = "Copied";
  b.setAttribute("aria-label", "Copied");
  clearTimeout(Number(b.dataset.timer));
  b.dataset.timer = String(
    setTimeout(() => {
      b.classList.remove("copied");
      b.title = "Copy";
      b.setAttribute("aria-label", "Copy the code");
    }, 1500),
  );
}

/** A picture's source for one theme (`media="(prefers-color-scheme: dark)"`) follows the theme
 * picked here, not only the system's. */
function themePictures(root: ParentNode) {
  const picked = document.documentElement.dataset.theme;
  for (const s of root.querySelectorAll<HTMLSourceElement>("picture > source[media]")) {
    const scheme = (s.dataset.scheme ??= /prefers-color-scheme:\s*(dark|light)/.exec(s.media)?.[1] ?? "");
    if (!scheme) continue;
    const media = picked === "dark" || picked === "light" ? (picked === scheme ? "all" : "not all") : `(prefers-color-scheme: ${scheme})`;
    if (s.media !== media) s.media = media;
  }
}

let watching = false;
/** Pictures change with the theme picked (theme.tsx sets `data-theme`, or removes it for the
 * system's). */
function watchTheme() {
  if (watching) return;
  watching = true;
  new MutationObserver(() => themePictures(document)).observe(document.documentElement, { attributes: true, attributeFilter: ["data-theme"] });
}

/** A text's sections, to go to one: a menu of its headings (a long README's, as on GitHub),
 * from three of them on. */
export function Outline({ html }: { html: string }) {
  const heads = useMemo(() => {
    const doc = new DOMParser().parseFromString(html, "text/html");
    return Array.from(doc.querySelectorAll<HTMLElement>("h1[id], h2[id], h3[id], h4[id], h5[id], h6[id]"), (h) => ({
      id: h.id,
      level: Number(h.tagName[1]),
      text: h.textContent?.trim() ?? "",
    })).filter((h) => h.text);
  }, [html]);
  if (heads.length < 3) return null;
  const top = Math.min(...heads.map((h) => h.level));
  return (
    <Menu
      trigger={
        <button type="button" className="icon-btn small-icon" title="Outline" aria-label="Outline">
          <TableOfContents size={16} />
        </button>
      }
      items={heads.map((h) => ({
        label: (
          <span className="outline-item" style={{ paddingLeft: (h.level - top) * 14 }}>
            {h.text}
          </span>
        ),
        onSelect: () => {
          const el = document.getElementById(h.id);
          if (el) goToSection(el, h.id.startsWith(USER) ? h.id.slice(USER.length) : h.id);
        },
      }))}
    />
  );
}

let diagrams = 0;

function dark() {
  const t = document.documentElement.dataset.theme;
  return t === "dark" || (t !== "light" && window.matchMedia("(prefers-color-scheme: dark)").matches);
}

/** Typeset the math and draw the diagrams under `root` (idempotent). */
export async function enhance(root: HTMLElement) {
  const math = Array.from(root.querySelectorAll<HTMLElement>("[data-math-style]"));
  if (math.length) {
    const [{ default: katex }] = await Promise.all([import("katex"), import("katex/dist/katex.min.css")]);
    for (const el of math) {
      if (!el.isConnected) continue;
      const display = el.dataset.mathStyle === "display";
      // ```math is <pre><code>: the whole block goes
      const target = el.tagName === "CODE" && el.parentElement?.tagName === "PRE" ? el.parentElement : el;
      const out = document.createElement(display ? "div" : "span");
      out.className = display ? "math-display" : "math-inline";
      katex.render(el.textContent ?? "", out, { displayMode: display, throwOnError: false, trust: false, maxSize: 50, maxExpand: 1000 });
      target.replaceWith(out);
    }
  }
  const blocks = Array.from(root.querySelectorAll<HTMLElement>("pre > code.language-mermaid"));
  if (blocks.length) {
    const { default: mermaid } = await import("mermaid");
    // strict: no scripts, no click handlers, labels sanitized
    mermaid.initialize({ startOnLoad: false, securityLevel: "strict", theme: dark() ? "dark" : "default" });
    for (const code of blocks) {
      const pre = code.parentElement!;
      if (!pre.isConnected) continue;
      try {
        const { svg } = await mermaid.render(`mermaid-${++diagrams}`, code.textContent ?? "");
        const div = document.createElement("div");
        div.className = "mermaid-diagram";
        div.innerHTML = svg;
        pre.replaceWith(div);
      } catch (e) {
        pre.classList.add("mermaid-error");
        pre.title = `Not a valid diagram: ${e instanceof Error ? e.message : String(e)}`;
      }
    }
  }
}
