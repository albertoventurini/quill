<script lang="ts">
  import { api, type Connection, type NewConnection, type SlotState, type QueryResult, type CommandError } from "$lib/tauri";

  // ── Connection list ──

  let connections = $state<Connection[]>([]);
  let selectedId = $state<number | null>(null);
  let connectedState = $state<Record<number, SlotState>>({});

  // ── Add-connection modal ──

  let showAddModal = $state(false);
  let addDialog = $state<HTMLDialogElement | null>(null);
  let addForm: NewConnection = $state({
    name: "",
    host: "localhost",
    port: 5432,
    default_db: "postgres",
    username: "postgres",
    ssl_mode: "disable",
    slot_budget: 2,
    password_ref: null,
  });
  let addError = $state("");

  // ── Connect form ──

  let connectPassword = $state("");

  // ── Query form ──

  let sql = $state("SELECT 1");
  let database = $state("");
  let runningQuery = $state(false);
  let result = $state<QueryResult | { error: CommandError } | null>(null);

  // ── Load connections on mount ──

  $effect(() => {
    api.listConnections().then((c) => (connections = c));
  });

  // Derive the currently-selected connection object.
  let selected = $derived(connections.find((c) => c.id === selectedId) ?? null);

  // ── Slot indicator label ──

  function slotLabel(state: SlotState | undefined): string {
    if (!state) return "";
    const busy = state.slots.filter((s) => s.busy).length;
    return `[${busy}/${state.budget}]`;
  }

  function isConnected(id: number): boolean {
    return id in connectedState;
  }

  // ── Add connection ──

  async function saveConnection(e: Event) {
    e.preventDefault();
    addError = "";
    try {
      const conn = await api.saveConnection({ ...addForm });
      connections = await api.listConnections();
      showAddModal = false;
      addDialog?.close();
    } catch (err) {
      addError = ((err as CommandError).message) ?? String(err);
    }
  }

  function openAddModal() {
    showAddModal = true;
    // Reset form to defaults.
    addForm = {
      name: "",
      host: "localhost",
      port: 5432,
      default_db: "postgres",
      username: "postgres",
      ssl_mode: "disable",
      slot_budget: 2,
      password_ref: null,
    };
    addError = "";
  }

  // Show the dialog after the DOM node is bound.
  $effect(() => {
    if (showAddModal && addDialog) {
      addDialog.showModal();
    }
  });

  function closeAddModal() {
    showAddModal = false;
    addDialog?.close();
  }

  // ── Connect / disconnect ──

  async function connect(id: number) {
    try {
      const state = await api.connectServer(id, connectPassword);
      connectedState = { ...connectedState, [id]: state };
      connectPassword = "";
    } catch (err) {
      result = { error: err as CommandError };
    }
  }

  async function disconnect(_id: number) {
    await api.disconnectServer(_id);
    // Remove from local state.
    const next = { ...connectedState };
    delete next[_id];
    connectedState = next;
  }

  // ── Run query ──

  async function run() {
    if (!selected || !isConnected(selected.id) || !sql.trim() || runningQuery) return;

    runningQuery = true;
    result = null;
    try {
      const db = database.trim() || selected.default_db;
      result = await api.runQuery(selected.id, db, sql);
    } catch (err) {
      result = { error: err as CommandError };
    } finally {
      runningQuery = false;
    }
  }

  let canRun = $derived(
    selected !== null &&
    isConnected(selected.id) &&
    sql.trim().length > 0 &&
    !runningQuery,
  );

  // ── Render a QueryResult for the <pre> block ──

  function renderResult(r: QueryResult): string {
    if (r.columns.length === 0)
      return `(no columns)\n${r.row_count} rows, ${r.duration_ms}ms`;
    const header = r.columns.map((c) => c.name).join("\t");
    const lines = r.rows.map((row) =>
      row
        .map((cell) => {
          if (cell === null) return "NULL";
          if (typeof cell === "object") return JSON.stringify(cell);
          return String(cell);
        })
        .join("\t"),
    );
    return [header, ...lines, "", `${r.row_count} rows in ${r.duration_ms}ms`].join("\n");
  }
</script>

<div class="shell">
  <!-- ═══════ LEFT PANE ═══════ -->
  <aside class="left-pane">
    <h2>Connections</h2>

    <!-- Connection list -->
    {#if connections.length === 0}
      <p class="muted">No saved connections.</p>
    {:else}
      <ul class="conn-list">
        {#each connections as conn (conn.id)}
          <!-- svelte-ignore a11y_no_noninteractive_element_to_interactive_role -->
          <li
            class="conn-item"
            class:selected={selectedId === conn.id}
            onclick={() => { selectedId = conn.id; }}
            onkeydown={(e) => { if (e.key === "Enter") selectedId = conn.id; }}
            role="button"
            tabindex="0"
          >
            <span class="conn-name">{conn.name}</span>
            <span class="slot-badge">{slotLabel(connectedState[conn.id])}</span>
          </li>
        {/each}
      </ul>
    {/if}

    <button class="btn" onclick={openAddModal}>+ Add Connection</button>

    <!-- Connect / disconnect controls -->
    {#if selected}
      {@const sel = selected}
      <div class="connect-area">
        {#if isConnected(sel.id)}
          <p>
            Connected.
            <button class="btn btn-danger" onclick={() => disconnect(sel.id)}>
              Disconnect
            </button>
          </p>
        {:else}
          <form
            onsubmit={(e) => { e.preventDefault(); connect(sel.id); }}
            class="connect-form"
          >
            <input
              type="password"
              placeholder="Password"
              bind:value={connectPassword}
              class="input"
            />
            <button type="submit" class="btn" disabled={!connectPassword}>
              Connect
            </button>
          </form>
        {/if}
      </div>
    {/if}
  </aside>

  <!-- ═══════ RIGHT PANE ═══════ -->
  <main class="right-pane">
    {#if selected}
      <h3>{selected.name}</h3>

      <!-- Database input -->
      <label class="field">
        Database
        <input
          type="text"
          bind:value={database}
          placeholder={selected.default_db}
          class="input"
        />
      </label>

      <!-- SQL textarea -->
      <textarea
        bind:value={sql}
        class="sql-input"
        rows={8}
        placeholder="SELECT 1"
      ></textarea>

      <!-- Run -->
      <button class="btn" onclick={run} disabled={!canRun}>
        {runningQuery ? "Running…" : "Run"}
      </button>

      <!-- Result / error -->
      {#if result}
        <div class="result-area">
          {#if "error" in result}
            <pre class="error">[{result.error.kind}] {result.error.message}</pre>
          {:else}
            <pre>{renderResult(result)}</pre>
          {/if}
        </div>
      {/if}
    {:else}
      <p class="muted">Select or add a connection to get started.</p>
    {/if}
  </main>
</div>

<!-- ═══════ ADD-CONNECTION DIALOG ═══════ -->
<!-- svelte-ignore a11y_no_static_element_interactions -->
<dialog bind:this={addDialog} class="modal" onclose={closeAddModal}>
  <h2>Add Connection</h2>
  <form onsubmit={saveConnection} class="add-form">
    <label class="field">
      Name
      <input type="text" class="input" bind:value={addForm.name} required />
    </label>
    <label class="field">
      Host
      <input type="text" class="input" bind:value={addForm.host} required />
    </label>
    <label class="field">
      Port
      <input type="number" class="input" bind:value={addForm.port} min={1} max={65535} />
    </label>
    <label class="field">
      Default database
      <input type="text" class="input" bind:value={addForm.default_db} required />
    </label>
    <label class="field">
      Username
      <input type="text" class="input" bind:value={addForm.username} required />
    </label>
    <label class="field">
      SSL mode
      <select class="input" bind:value={addForm.ssl_mode}>
        <option value="disable">disable</option>
        <option value="prefer">prefer</option>
        <option value="require">require</option>
        <option value="verify-ca">verify-ca</option>
        <option value="verify-full">verify-full</option>
      </select>
    </label>
    <label class="field">
      Slot budget
      <input type="number" class="input" bind:value={addForm.slot_budget} min={1} max={16} />
    </label>

    {#if addError}
      <p class="error">{addError}</p>
    {/if}

    <div class="modal-actions">
      <button type="button" class="btn" onclick={closeAddModal}>Cancel</button>
      <button type="submit" class="btn btn-primary">Save</button>
    </div>
  </form>
</dialog>

<!-- ═══════ STYLES ═══════ -->
<style>
  /* ── Layout ── */

  .shell {
    display: flex;
    height: 100vh;
  }

  .left-pane {
    width: 280px;
    min-width: 280px;
    border-right: 1px solid #ccc;
    padding: 1rem;
    overflow-y: auto;
    display: flex;
    flex-direction: column;
    gap: 0.75rem;
  }

  .right-pane {
    flex: 1;
    padding: 1rem;
    overflow-y: auto;
    display: flex;
    flex-direction: column;
    gap: 0.75rem;
  }

  /* ── Connection list ── */

  .conn-list {
    list-style: none;
    padding: 0;
    margin: 0;
  }

  .conn-item {
    display: flex;
    justify-content: space-between;
    align-items: center;
    padding: 0.4rem 0.5rem;
    cursor: pointer;
    border-radius: 4px;
    border: 1px solid transparent;
  }

  .conn-item:hover {
    background: #e8e8e8;
  }

  .conn-item.selected {
    background: #d0d0ff;
    border-color: #8888cc;
  }

  .slot-badge {
    font-size: 0.8rem;
    color: #666;
    font-variant-numeric: tabular-nums;
  }

  /* ── Common controls ── */

  .btn {
    padding: 0.4rem 0.8rem;
    border: 1px solid #888;
    border-radius: 4px;
    background: #f0f0f0;
    cursor: pointer;
    font: inherit;
  }

  .btn:hover {
    background: #e0e0e0;
  }

  .btn:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }

  .btn-primary {
    background: #3366cc;
    color: white;
    border-color: #2255aa;
  }

  .btn-primary:hover {
    background: #2255aa;
  }

  .btn-danger {
    background: #cc3333;
    color: white;
    border-color: #aa2222;
  }

  .btn-danger:hover {
    background: #aa2222;
  }

  .input {
    padding: 0.4rem;
    border: 1px solid #aaa;
    border-radius: 4px;
    font: inherit;
    box-sizing: border-box;
  }

  /* ── Connect area ── */

  .connect-area {
    border-top: 1px solid #ccc;
    padding-top: 0.75rem;
  }

  .connect-form {
    display: flex;
    gap: 0.5rem;
  }

  .connect-form .input {
    flex: 1;
  }

  /* ── SQL input ── */

  .sql-input {
    width: 100%;
    padding: 0.5rem;
    font: 14px monospace;
    border: 1px solid #aaa;
    border-radius: 4px;
    box-sizing: border-box;
    resize: vertical;
  }

  /* ── Result ── */

  .result-area {
    border-top: 1px solid #ccc;
    padding-top: 0.5rem;
  }

  .result-area pre {
    margin: 0;
    font: 13px monospace;
    white-space: pre-wrap;
  }

  .error {
    color: #cc0000;
  }

  /* ── Modal ── */

  .modal {
    border: 1px solid #888;
    border-radius: 8px;
    padding: 1.5rem;
    max-width: 400px;
    width: 90%;
  }

  .modal::backdrop {
    background: rgba(0, 0, 0, 0.3);
  }

  .add-form {
    display: flex;
    flex-direction: column;
    gap: 0.6rem;
  }

  .field {
    display: flex;
    flex-direction: column;
    gap: 0.2rem;
    font-size: 0.9rem;
  }

  .modal-actions {
    display: flex;
    justify-content: flex-end;
    gap: 0.5rem;
    margin-top: 0.5rem;
  }

  /* ── Misc ── */

  .muted {
    color: #888;
    font-style: italic;
  }

  h2, h3 {
    margin: 0 0 0.25rem 0;
    font-size: 1.1rem;
  }
</style>
