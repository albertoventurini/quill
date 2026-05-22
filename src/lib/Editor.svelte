<script lang="ts">
  //! CodeMirror 6 SQL editor.
  //!
  //! Owns the EditorView; mirrors document changes back to the parent via
  //! the `onChange` callback.  The parent retains the source of truth in
  //! a `$state<string>` and writes back into the editor when needed via
  //! the exposed `setDoc` method (e.g. M5's "open saved query" flow).

  import { onMount } from "svelte";
  import { EditorState } from "@codemirror/state";
  import { EditorView, keymap, lineNumbers, highlightActiveLine } from "@codemirror/view";
  import {
    defaultKeymap,
    history,
    historyKeymap,
    indentWithTab,
  } from "@codemirror/commands";
  import { sql, PostgreSQL } from "@codemirror/lang-sql";
  import { searchKeymap } from "@codemirror/search";
  import { bracketMatching, indentOnInput, syntaxHighlighting, defaultHighlightStyle } from "@codemirror/language";
  import { autocompletion } from "@codemirror/autocomplete";

  import { statementAtCursor } from "./statement";
  import { makeCompletionSource, type EditorContext } from "./completion";

  let {
    initial = "SELECT 1",
    onChange,
    onRun,
    getContext = () => null as EditorContext,
  }: {
    initial?: string;
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

  function makeRunKeymap() {
    return keymap.of([
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
    ]);
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
        syntaxHighlighting(defaultHighlightStyle),
        sql({ dialect: PostgreSQL }),
        autocompletion({
          override: [makeCompletionSource(getContext)],
          closeOnBlur: true,
          activateOnTyping: true,
        }),
        keymap.of([
          ...defaultKeymap,
          ...historyKeymap,
          ...searchKeymap,
          indentWithTab,
        ]),
        makeRunKeymap(),
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
        }),
      ],
    });

    view = new EditorView({ state, parent: host });

    return () => {
      view?.destroy();
      view = null;
    };
  });
</script>

<div class="editor-host" bind:this={host}></div>

<style>
  .editor-host {
    height: 220px;
    border: 1px solid #aaa;
    border-radius: 4px;
    overflow: hidden;
    background: white;
  }
  :global(.cm-editor) {
    height: 100%;
  }
  :global(.cm-editor.cm-focused) {
    outline: 2px solid #3366cc;
    outline-offset: -2px;
  }
</style>
