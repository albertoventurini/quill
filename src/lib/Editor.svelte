<script lang="ts">
  //! CodeMirror 6 SQL editor.
  //!
  //! Owns the EditorView; mirrors document changes back to the parent via
  //! the `onChange` callback.  The parent retains the source of truth in
  //! a `$state<string>` and writes back into the editor when needed via
  //! the exposed `setDoc` method (e.g. M5's "open saved query" flow).

  import { onMount } from "svelte";
  import { Compartment, EditorState } from "@codemirror/state";
  import { EditorView, keymap, lineNumbers, highlightActiveLine } from "@codemirror/view";
  import {
    defaultKeymap,
    history,
    historyKeymap,
    indentWithTab,
  } from "@codemirror/commands";
  import { sql, PostgreSQL } from "@codemirror/lang-sql";
  import { searchKeymap } from "@codemirror/search";
  import { bracketMatching, indentOnInput, syntaxHighlighting, defaultHighlightStyle, HighlightStyle } from "@codemirror/language";
  import { tags } from "@lezer/highlight";
  import { autocompletion } from "@codemirror/autocomplete";

  import { statementAtCursor } from "./statement";
  import { makeCompletionSource, type EditorContext } from "./completion";
  import { getEffectiveTheme } from "./theme.svelte";

  let {
    initial = "SELECT 1",
    height = 220,
    onChange,
    onRun,
    getContext = () => null as EditorContext,
  }: {
    initial?: string;
    height?: number;
    onChange: (doc: string) => void;
    onRun: (payload: {
      text: string;
      isSelection: boolean;
      multiStatement: boolean;
    }) => void;
    getContext?: () => EditorContext;
  } = $props();

  let host = $state<HTMLDivElement | null>(null);
  let view: EditorView | null = null;

  const highlightCompartment = new Compartment();

  const darkHighlightStyle = HighlightStyle.define([
    { tag: tags.keyword, color: "#569cd6" },
    { tag: tags.controlKeyword, color: "#569cd6" },
    { tag: tags.definitionKeyword, color: "#569cd6" },
    { tag: tags.modifier, color: "#569cd6" },
    { tag: tags.operator, color: "#d4d4d4" },
    { tag: tags.operatorKeyword, color: "#569cd6" },
    { tag: tags.string, color: "#ce9178" },
    { tag: tags.number, color: "#b5cea8" },
    { tag: tags.typeName, color: "#4ec9b0" },
    { tag: tags.className, color: "#4ec9b0" },
    { tag: tags.comment, color: "#6a9955", fontStyle: "italic" },
    { tag: tags.lineComment, color: "#6a9955", fontStyle: "italic" },
    { tag: tags.blockComment, color: "#6a9955", fontStyle: "italic" },
    { tag: tags.bracket, color: "#d4d4d4" },
    { tag: tags.paren, color: "#d4d4d4" },
    { tag: tags.bool, color: "#569cd6" },
    { tag: tags.null, color: "#569cd6" },
    { tag: tags.variableName, color: "#9cdcfe" },
    { tag: tags.definition(tags.variableName), color: "#9cdcfe" },
    { tag: tags.local(tags.variableName), color: "#9cdcfe" },
    { tag: tags.function(tags.variableName), color: "#dcdcaa" },
    { tag: tags.propertyName, color: "#9cdcfe" },
    { tag: tags.labelName, color: "#d4d4d4" },
    { tag: tags.color, color: "#d4d4d4" },
    { tag: tags.name, color: "#d4d4d4" },
    { tag: tags.escape, color: "#d7ba7d" },
    { tag: tags.regexp, color: "#d16969" },
    { tag: tags.strong, fontWeight: "bold" },
    { tag: tags.emphasis, fontStyle: "italic" },
    { tag: tags.separator, color: "#d4d4d4" },
    { tag: tags.character, color: "#ce9178" },
    { tag: tags.attributeName, color: "#9cdcfe" },
    { tag: tags.attributeValue, color: "#ce9178" },
    { tag: tags.content, color: "#d4d4d4" },
    { tag: tags.meta, color: "#808080" },
    { tag: tags.processingInstruction, color: "#569cd6" },
    { tag: tags.heading, color: "#569cd6", fontWeight: "bold" },
    { tag: tags.url, color: "#4fc1ff", textDecoration: "underline" },
  ]);

  function currentHighlight() {
    return getEffectiveTheme() === "dark" ? darkHighlightStyle : defaultHighlightStyle;
  }

  // Public-ish imperative methods. The parent gets a reference via bind:this.
  export function setDoc(next: string) {
    if (!view) return;
    if (view.state.doc.toString() === next) return;
    view.dispatch({
      changes: { from: 0, to: view.state.doc.length, insert: next },
    });
  }
  export function focus() {
    view?.focus();
  }

  onMount(() => {
    if (!host) return;

    const state = EditorState.create({
      doc: initial,
      extensions: [
        lineNumbers(),
        highlightActiveLine(),
        history(),
        bracketMatching(),
        indentOnInput(),
        highlightCompartment.of(syntaxHighlighting(currentHighlight())),
        sql({ dialect: PostgreSQL }),
        autocompletion({
          override: [makeCompletionSource(getContext)],
          closeOnBlur: true,
          activateOnTyping: true,
        }),
        keymap.of([
          ...defaultKeymap.filter((k) => k.key !== "Mod-Enter"),
          ...historyKeymap,
          ...searchKeymap,
          indentWithTab,
          {
            key: "Mod-Enter",
            preventDefault: true,
            run: (v) => {
              const doc = v.state.doc.toString();
              const cursor = v.state.selection.main.head;
              const sel = v.state.selection.main;
              const picked = statementAtCursor(doc, cursor, {
                from: sel.from,
                to: sel.to,
              });
              if (picked && picked.text.length > 0) {
                onRun(picked);
              }
              return true;
            },
          },
        ]),
        EditorView.updateListener.of((u) => {
          if (u.docChanged) onChange(u.state.doc.toString());
        }),
        EditorView.theme({
          "&": {
            height: "100%",
            fontSize: "13px",
            fontFamily:
              "ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace",
          },
          ".cm-content": { padding: "8px 0" },
          ".cm-scroller": { fontFamily: "inherit" },
          ".cm-gutters": {
            backgroundColor: "var(--cm-bg)",
            color: "var(--text-faint)",
            borderRight: "1px solid var(--border-light)",
          },
          ".cm-activeLineGutter": {
            backgroundColor: "var(--bg-hover)",
          },
          ".cm-activeLine": {
            backgroundColor: "var(--bg-hover)",
          },
          ".cm-cursor": {
            borderLeftColor: "var(--text-accent)",
          },
          ".cm-selectionMatch": {
            backgroundColor: "var(--bg-accent-light)",
          },
          "&.cm-focused .cm-selectionBackground, .cm-selectionBackground": {
            backgroundColor: "var(--bg-selected-cell) !important",
          },
          ".cm-matchingBracket": {
            backgroundColor: "var(--bg-hover)",
            outline: "1px solid var(--text-accent)",
          },
          ".cm-tooltip": {
            backgroundColor: "var(--bg-surface)",
            color: "var(--text-primary)",
            border: "1px solid var(--border-secondary)",
          },
          ".cm-tooltip-autocomplete .cm-completionLabel": {
            color: "var(--text-primary)",
          },
          ".cm-tooltip-autocomplete .cm-completionDetail": {
            color: "var(--text-muted)",
          },
        }),
      ],
    });

    view = new EditorView({ state, parent: host });

    return () => {
      view?.destroy();
      view = null;
    };
  });

  $effect(() => {
    if (!view) return;
    view.dispatch({
      effects: highlightCompartment.reconfigure(
        syntaxHighlighting(currentHighlight()),
      ),
    });
  });
</script>

<div class="editor-host" style="height: {height}px" bind:this={host}></div>

<style>
  .editor-host {
    border: 1px solid var(--cm-border);
    border-radius: 4px;
    overflow: hidden;
    background: var(--cm-bg);
  }
  :global(.cm-editor) {
    height: 100%;
  }
  :global(.cm-editor.cm-focused) {
    outline: 2px solid var(--cm-focus-ring);
    outline-offset: -2px;
  }
</style>
