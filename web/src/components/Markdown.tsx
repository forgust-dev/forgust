import { useEffect, useRef } from "react";

/** Markdown the server rendered and sanitized. Math (`$x$`, ```math) and Mermaid
 * diagrams are drawn here, loading KaTeX or Mermaid only on pages that have some. */
export function Markdown({ html, className }: { html: string; className?: string }) {
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (ref.current) void enhance(ref.current);
  }, [html]);
  return <div ref={ref} className={className} dangerouslySetInnerHTML={{ __html: html }} />;
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
