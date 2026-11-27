import { useEffect, useRef } from "react";
import { Annotation, Compartment, EditorState, type Extension } from "@codemirror/state";
import {
  crosshairCursor,
  drawSelection,
  dropCursor,
  EditorView,
  highlightActiveLine,
  highlightActiveLineGutter,
  highlightSpecialChars,
  keymap,
  lineNumbers,
  rectangularSelection,
} from "@codemirror/view";
import { defaultKeymap, history, historyKeymap, indentWithTab } from "@codemirror/commands";
import { bracketMatching, indentOnInput, indentUnit, LanguageDescription, syntaxHighlighting } from "@codemirror/language";
import { closeBrackets, closeBracketsKeymap } from "@codemirror/autocomplete";
import { highlightSelectionMatches, searchKeymap } from "@codemirror/search";
import { languages } from "@codemirror/language-data";
import { tagHighlighter, tags as t } from "@lezer/highlight";
import { nameOf } from "../api/client";

/** Marks the text put in from outside (`value`): it is not something typed, to report back. */
const given = Annotation.define<boolean>();

/** Files whose indentation is a tab by rule or by convention. */
const TABBED = /(^|\/)(GNUmakefile|[Mm]akefile)$|\.(go|mk)$/;

/** What one level of indentation is: what the text already uses, else what its kind of file asks for. */
function indentFor(path: string, doc: string): string {
  const first = /^(\t| {2,8})\S/m.exec(doc)?.[1];
  return first ?? (TABBED.test(path) ? "\t" : "  ");
}

/** What the grammars find, as the classes styles.css colors (a rule also takes the finer tags under its own). */
const highlighter = tagHighlighter([
  { tag: [t.comment, t.meta], class: "tok-comment" },
  { tag: t.keyword, class: "tok-keyword" },
  { tag: [t.string, t.regexp], class: "tok-string" },
  { tag: [t.escape, t.link], class: "tok-escape" },
  { tag: [t.number, t.bool, t.atom, t.null, t.url, t.constant(t.variableName)], class: "tok-constant" },
  { tag: [t.function(t.variableName), t.function(t.propertyName), t.macroName, t.labelName], class: "tok-function" },
  { tag: [t.typeName, t.className, t.namespace], class: "tok-type" },
  { tag: t.tagName, class: "tok-tag" },
  { tag: t.attributeName, class: "tok-attribute" },
  { tag: [t.heading, t.strong], class: "tok-strong" },
  { tag: t.emphasis, class: "tok-emphasis" },
  { tag: t.strikethrough, class: "tok-strikethrough" },
  { tag: t.invalid, class: "tok-invalid" },
]);

// The colors are the page's own variables, so the editor follows the theme.
const theme = EditorView.theme({
  "&": { height: "100%", color: "var(--text)", backgroundColor: "var(--bg)", fontSize: "13px" },
  "&.cm-focused": { outline: "none" },
  ".cm-scroller": { fontFamily: "var(--mono)", lineHeight: "20px", overflow: "auto" },
  ".cm-content": { padding: "10px 0", caretColor: "var(--text)" },
  ".cm-cursor, .cm-dropCursor": { borderLeftColor: "var(--text)" },
  ".cm-gutters": { backgroundColor: "var(--bg-subtle)", color: "var(--text-faint)", border: "none", borderRight: "1px solid var(--border)" },
  ".cm-lineNumbers .cm-gutterElement": { padding: "0 10px 0 12px" },
  ".cm-activeLine": { backgroundColor: "color-mix(in srgb, var(--bg-hover) 55%, transparent)" },
  ".cm-activeLineGutter": { backgroundColor: "var(--bg-hover)", color: "var(--text)" },
  "&.cm-focused > .cm-scroller > .cm-selectionLayer .cm-selectionBackground, .cm-selectionBackground": {
    backgroundColor: "color-mix(in srgb, var(--accent) 22%, transparent)",
  },
  ".cm-selectionMatch": { backgroundColor: "color-mix(in srgb, var(--accent) 12%, transparent)" },
  ".cm-searchMatch": { backgroundColor: "var(--warn-soft)", outline: "1px solid var(--warn-border)" },
  ".cm-searchMatch.cm-searchMatch-selected": { backgroundColor: "color-mix(in srgb, var(--accent) 35%, transparent)" },
  "&.cm-focused .cm-matchingBracket": { backgroundColor: "var(--ok-soft)", outline: "1px solid var(--ok)" },
  "&.cm-focused .cm-nonmatchingBracket": { backgroundColor: "var(--danger-soft)" },
  ".cm-panels": { backgroundColor: "var(--bg-subtle)", color: "var(--text)" },
  ".cm-panels.cm-panels-top": { borderBottom: "1px solid var(--border)" },
  ".cm-panels.cm-panels-bottom": { borderTop: "1px solid var(--border)" },
  // the page's own rule for inputs (full width, 34px) is not for the search panel
  ".cm-panels input": { width: "auto", height: "auto" },
  ".cm-panels .cm-textfield": { padding: "3px 8px", border: "1px solid var(--border-strong)", borderRadius: "var(--radius-sm)", backgroundColor: "var(--bg)", fontSize: "12px" },
  ".cm-panels .cm-button": { padding: "3px 10px", border: "1px solid var(--border-strong)", borderRadius: "var(--radius-sm)", background: "var(--bg)", color: "var(--text)", fontSize: "12px" },
  ".cm-panels label": { fontSize: "12px" },
});

const setup: Extension = [
  lineNumbers(),
  highlightActiveLineGutter(),
  highlightSpecialChars(),
  history(),
  drawSelection(),
  dropCursor(),
  EditorState.allowMultipleSelections.of(true),
  indentOnInput(),
  bracketMatching(),
  closeBrackets(),
  rectangularSelection(),
  crosshairCursor(),
  highlightActiveLine(),
  highlightSelectionMatches(),
  syntaxHighlighting(highlighter),
  EditorState.tabSize.of(4),
  // Tab indents instead of leaving the editor (Escape then Tab leaves it)
  keymap.of([...closeBracketsKeymap, ...defaultKeymap, ...searchKeymap, ...historyKeymap, indentWithTab]),
  theme,
];

/**
 * Puts in a compartment an extension whose code is fetched when first asked for
 * (`load`), or empties it (null). A fetch that fails (offline, a new release) leaves
 * the editor as it was.
 */
function useLazy(view: { current: EditorView | null }, compartment: Compartment, load: (() => Promise<Extension>) | null, deps: unknown[]) {
  useEffect(() => {
    const v = view.current;
    if (!v) return;
    let current = true;
    const set = (ext: Extension) => {
      if (current && view.current === v) v.dispatch({ effects: compartment.reconfigure(ext) });
    };
    if (load) load().then(set, () => {});
    else set([]);
    return () => {
      current = false;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, deps);
}

/**
 * A code editor (CodeMirror): `value` is the text, `path` names the file, which picks
 * the language (its grammar is fetched when first needed) and the indentation. With
 * `original`, what differs from that text shows in place: the lines added are marked,
 * those removed stay in sight, and the text is edited as ever. `vim` gives it Vim's
 * modes and keys.
 */
export function CodeEditor({ value, onChange, path, label, autoFocus, original, vim }: { value: string; onChange: (text: string) => void; path: string; label: string; autoFocus?: boolean; original?: string; vim?: boolean }) {
  const host = useRef<HTMLDivElement>(null);
  const view = useRef<EditorView | null>(null);
  const perFile = useRef(new Compartment());
  const compared = useRef(new Compartment());
  const modal = useRef(new Compartment());
  // the text the editor last reported: a `value` that differs comes from outside
  const reported = useRef(value);
  const changed = useRef(onChange);
  changed.current = onChange;

  useEffect(() => {
    // a file whose every line ends in CRLF keeps them (any other mix comes out as LF). The
    // editor itself splits on any line break, so a paste of LF lines still makes lines.
    const lineBreak = reported.current.includes("\r\n") && !/(^|[^\r])\n/.test(reported.current) ? "\r\n" : "\n";
    const v = new EditorView({
      parent: host.current!,
      state: EditorState.create({
        doc: reported.current,
        extensions: [
          // before every other keymap: in its normal mode Vim takes the keys first
          modal.current.of([]),
          setup,
          perFile.current.of([]),
          compared.current.of([]),
          EditorView.contentAttributes.of({ "aria-label": label }),
          EditorView.updateListener.of((u) => {
            if (!u.docChanged || u.transactions.some((tr) => tr.annotation(given))) return;
            reported.current = u.state.doc.sliceString(0, undefined, lineBreak);
            changed.current(reported.current);
          }),
        ],
      }),
    });
    view.current = v;
    return () => {
      view.current = null;
      v.destroy();
    };
    // the view lives as long as the component: the props that change are followed below
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useEffect(() => {
    const v = view.current;
    if (!v || value === reported.current) return;
    reported.current = value;
    v.dispatch({ changes: { from: 0, to: v.state.doc.length, insert: value }, annotations: given.of(true) });
  }, [value]);

  useEffect(() => {
    const v = view.current;
    if (!v) return;
    let current = true;
    const set = (language: Extension) => {
      if (current && view.current === v) v.dispatch({ effects: perFile.current.reconfigure([indentUnit.of(indentFor(path, v.state.doc.toString())), language]) });
    };
    set([]);
    LanguageDescription.matchFilename(languages, nameOf(path))
      ?.load()
      .then(set, () => {});
    return () => {
      current = false;
    };
  }, [path]);

  // fetched with the first comparison, and by those who ask for Vim
  useLazy(view, compared.current, original === undefined ? null : () => import("@codemirror/merge").then((m) => m.unifiedMergeView({ original, mergeControls: false })), [original]);
  useLazy(view, modal.current, vim ? () => import("@replit/codemirror-vim").then((m) => m.vim({ status: true })) : null, [vim]);
  // the editor brought to the front takes the keyboard
  useEffect(() => {
    if (autoFocus) view.current?.focus();
  }, [autoFocus]);

  return <div ref={host} className="code-editor" />;
}
