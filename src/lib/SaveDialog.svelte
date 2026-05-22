<script lang="ts">
  import { api, type CommandError, type NewSavedQuery, type SavedScope } from "./tauri";
  import { errorMessage } from "./tree";

  let {
    initialSql,
    serverIdHint,
    onSaved,
    onError,
  }: {
    initialSql: string;
    /** The tab's serverId — used for the "Server only" scope option. `null`
     *  means there's no active tab; the scope defaults to global. */
    serverIdHint: number | null;
    onSaved: () => void;
    onError: (msg: string) => void;
  } = $props();

  let dialog = $state<HTMLDialogElement | null>(null);
  let name = $state("");
  let scope = $state<SavedScope>(serverIdHint === null ? "global" : "server");
  let formError = $state("");

  export function open(): void {
    name = "";
    formError = "";
    scope = serverIdHint === null ? "global" : "server";
    dialog?.showModal();
  }

  async function submit(e: Event) {
    e.preventDefault();
    formError = "";
    if (!name.trim()) {
      formError = "Name is required.";
      return;
    }
    const payload: NewSavedQuery = {
      name: name.trim(),
      scope,
      server_id: scope === "server" ? serverIdHint : null,
      sql: initialSql,
    };
    if (scope === "server" && payload.server_id === null) {
      formError = "No active server — pick a tab first, or choose Global.";
      return;
    }
    try {
      await api.saveQuery(payload);
      dialog?.close();
      onSaved();
    } catch (err) {
      const ce = err as CommandError;
      if (ce.kind === "Saved" && ce.message.includes("already exists")) {
        formError = ce.message;
      } else {
        onError(errorMessage(err));
        dialog?.close();
      }
    }
  }
</script>

<dialog bind:this={dialog} class="modal">
  <h2>Save query</h2>
  <form onsubmit={submit} class="save-form">
    <label class="field">
      Name
      <!-- svelte-ignore a11y_autofocus -->
      <input class="input" bind:value={name} autofocus required />
    </label>
    <fieldset class="scope">
      <legend>Scope</legend>
      <label>
        <input type="radio" name="scope" value="global" bind:group={scope} />
        Global (visible to every server)
      </label>
      <label>
        <input
          type="radio"
          name="scope"
          value="server"
          bind:group={scope}
          disabled={serverIdHint === null}
        />
        This server only
      </label>
    </fieldset>
    {#if formError}<p class="error">{formError}</p>{/if}
    <div class="modal-actions">
      <button type="button" class="btn" onclick={() => dialog?.close()}>Cancel</button>
      <button type="submit" class="btn btn-primary">Save</button>
    </div>
  </form>
</dialog>

<style>
  .modal { border: 1px solid #888; border-radius: 8px; padding: 1.25rem; max-width: 400px; width: 90%; }
  .modal::backdrop { background: rgba(0,0,0,0.3); }
  .save-form { display: flex; flex-direction: column; gap: 0.75rem; }
  .field { display: flex; flex-direction: column; gap: 0.15rem; font-size: 0.9rem; }
  .input { padding: 0.35rem; border: 1px solid #aaa; border-radius: 4px; font: inherit; }
  .scope { display: flex; flex-direction: column; gap: 0.25rem; font-size: 0.85rem; border: 1px solid #ddd; padding: 0.5rem; border-radius: 4px; }
  .scope legend { padding: 0 0.25rem; color: #555; }
  .modal-actions { display: flex; justify-content: flex-end; gap: 0.5rem; margin-top: 0.5rem; }
  .btn { padding: 0.3rem 0.6rem; border: 1px solid #888; border-radius: 4px; background: #f0f0f0; cursor: pointer; font: inherit; font-size: 0.9rem; }
  .btn:hover { background: #e0e0e0; }
  .btn-primary { background: #3366cc; color: white; border-color: #2255aa; }
  .btn-primary:hover { background: #2255aa; }
  .error { color: #b00020; font-size: 0.85rem; margin: 0; }
</style>
