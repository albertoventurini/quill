<script lang="ts">
  //! Entity-relationship diagram for one schema (or a subset of its tables).
  //!
  //! Reads the cached `SchemaPayload` (no extra DB round-trip when the schema
  //! has already been introspected), lays the visible relations out with
  //! dagre, and hand-draws table boxes + foreign-key edges in SVG.  Drag a table
  //! to move it, drag the background to pan, zoom with the wheel.  A side
  //! list toggles which tables are shown and whether 1-hop FK
  //! neighbours are pulled in automatically.

  import dagre from "@dagrejs/dagre";
  import { getSchemaPayload } from "./schemaStore";
  import type { RelationInfo, SchemaPayload } from "./tauri";

  let {
    serverId,
    database,
    schema,
    seed,
    includeNeighbors: includeNeighborsInit,
  }: {
    serverId: number;
    database: string;
    schema: string;
    seed: string[] | "all";
    includeNeighbors: boolean;
  } = $props();

  // ── Data load ──────────────────────────────────────────────────────────
  let payload = $state<SchemaPayload | null>(null);
  let loadError = $state<string | null>(null);
  let loading = $state(true);

  // Reactive scope/options the user can change in the side panel.
  let selected = $state<string[]>([]);
  let includeNeighbors = $state(includeNeighborsInit);
  let initialized = false;

  $effect(() => {
    // Re-run if the target changes; payload is promise-cached so reopening is
    // cheap.  `schema`/`seed` are captured once for initialisation below.
    loading = true;
    loadError = null;
    getSchemaPayload(serverId, database)
      .then((p) => {
        payload = p;
        loading = false;
        if (!initialized) {
          const rels = relationsIn(p, schema);
          selected = seed === "all" ? rels.map((r) => r.name) : [...seed];
          initialized = true;
        }
      })
      .catch((e: unknown) => {
        loadError =
          typeof e === "object" && e !== null && "message" in e
            ? String((e as { message: unknown }).message)
            : String(e);
        loading = false;
      });
  });

  function relationsIn(p: SchemaPayload, name: string): RelationInfo[] {
    return p.schemas.find((s) => s.name === name)?.relations ?? [];
  }

  // ── Derived model ──────────────────────────────────────────────────────
  let relations = $derived(payload ? relationsIn(payload, schema) : []);
  let relMap = $derived(new Map(relations.map((r) => [r.name, r])));
  let selectedSet = $derived(new Set(selected));

  /** Names actually drawn: the selected set, plus 1-hop FK neighbours (in
   *  this schema) when the toggle is on.  Filtered to relations that exist. */
  let visibleNames = $derived.by(() => {
    const vis = new Set(selectedSet);
    if (includeNeighbors) {
      for (const name of selectedSet) {
        const rel = relMap.get(name);
        if (rel) {
          for (const fk of rel.foreign_keys) {
            if (fk.referenced_schema === schema && relMap.has(fk.referenced_table)) {
              vis.add(fk.referenced_table);
            }
          }
        }
        for (const r of relations) {
          if (
            r.foreign_keys.some(
              (fk) => fk.referenced_schema === schema && fk.referenced_table === name,
            )
          ) {
            vis.add(r.name);
          }
        }
      }
    }
    return new Set([...vis].filter((n) => relMap.has(n)));
  });

  type Edge = { from: string; to: string; label: string };

  /** FK edges with both endpoints visible (and in this schema). */
  let edges = $derived.by(() => {
    const out: Edge[] = [];
    for (const name of visibleNames) {
      const rel = relMap.get(name);
      if (!rel) continue;
      for (const fk of rel.foreign_keys) {
        if (
          fk.referenced_schema === schema &&
          visibleNames.has(fk.referenced_table)
        ) {
          out.push({ from: name, to: fk.referenced_table, label: fk.columns.join(", ") });
        }
      }
    }
    return out;
  });

  // ── Node sizing (dagre needs measured boxes) ───────────────────────────
  const HEADER_H = 26;
  const ROW_H = 18;
  const CHAR_W = 7; // approx advance of the monospace font at 11px
  const PAD_X = 20;

  function nodeSize(rel: RelationInfo): { width: number; height: number } {
    const longest = Math.max(
      rel.name.length + 4,
      ...rel.columns.map((c) => c.name.length + c.type_name.length + 4),
      8,
    );
    const width = Math.min(360, Math.max(150, longest * CHAR_W + PAD_X));
    const height = HEADER_H + rel.columns.length * ROW_H + 6;
    return { width, height };
  }

  type Placed = { rel: RelationInfo; x: number; y: number; width: number; height: number };

  // ── Layout (dagre) ─────────────────────────────────────────────────────
  /** Automatic top-left positions from dagre, before any manual moves. */
  let autoLayout = $derived.by(() => {
    const visRels = relations.filter((r) => visibleNames.has(r.name));
    const g = new dagre.graphlib.Graph();
    g.setGraph({ rankdir: "LR", nodesep: 40, ranksep: 90, marginx: 24, marginy: 24 });
    g.setDefaultEdgeLabel(() => ({}));
    for (const rel of visRels) {
      const { width, height } = nodeSize(rel);
      g.setNode(rel.name, { width, height });
    }
    for (const e of edges) {
      if (e.from !== e.to) g.setEdge(e.from, e.to);
    }
    dagre.layout(g);

    const nodes: Placed[] = [];
    for (const rel of visRels) {
      const n = g.node(rel.name);
      if (!n) continue;
      // dagre reports box centres; convert to top-left for SVG.
      nodes.push({ rel, x: n.x - n.width / 2, y: n.y - n.height / 2, width: n.width, height: n.height });
    }
    return nodes;
  });

  /** Manual positions for tables the user has dragged, keyed by table name.
   *  Kept across selection changes so a moved table stays put. */
  let manualPos = $state<Record<string, { x: number; y: number }>>({});

  let nodes = $derived(
    autoLayout.map((n) => {
      const m = manualPos[n.rel.name];
      return m ? { ...n, x: m.x, y: m.y } : n;
    }),
  );

  /** FK edges drawn as cubic curves between the nearest vertical sides of the
   *  two boxes.  dagre's routed points are ignored because they go stale as
   *  soon as a table is moved by hand. */
  let paths = $derived.by(() => {
    const placed = new Map(nodes.map((n) => [n.rel.name, n]));
    const out: { d: string; label: string }[] = [];
    for (const e of edges) {
      const a = placed.get(e.from);
      const b = placed.get(e.to);
      if (!a || !b) continue;
      const y1 = a.y + a.height / 2;
      const y2 = b.y + b.height / 2;
      if (a === b) {
        const x = a.x + a.width;
        out.push({
          d: `M ${x} ${y1 - 8} C ${x + 40} ${y1 - 24}, ${x + 40} ${y1 + 24}, ${x} ${y1 + 8}`,
          label: e.label,
        });
        continue;
      }
      let d: string;
      if (b.x >= a.x + a.width) {
        const x1 = a.x + a.width;
        const x2 = b.x;
        const dx = Math.max(40, (x2 - x1) / 2);
        d = `M ${x1} ${y1} C ${x1 + dx} ${y1}, ${x2 - dx} ${y2}, ${x2} ${y2}`;
      } else if (b.x + b.width <= a.x) {
        const x1 = a.x;
        const x2 = b.x + b.width;
        const dx = Math.max(40, (x1 - x2) / 2);
        d = `M ${x1} ${y1} C ${x1 - dx} ${y1}, ${x2 + dx} ${y2}, ${x2} ${y2}`;
      } else {
        // Horizontally overlapping (stacked): loop out from both right sides.
        const x1 = a.x + a.width;
        const x2 = b.x + b.width;
        const xc = Math.max(x1, x2) + 50;
        d = `M ${x1} ${y1} C ${xc} ${y1}, ${xc} ${y2}, ${x2} ${y2}`;
      }
      out.push({ d, label: e.label });
    }
    return out;
  });

  /** FK column names per relation, for the column-row marker. */
  function fkColumns(rel: RelationInfo): Set<string> {
    return new Set(rel.foreign_keys.flatMap((fk) => fk.columns));
  }

  // ── Pan / zoom ─────────────────────────────────────────────────────────
  let zoom = $state(1);
  let panX = $state(24);
  let panY = $state(24);
  let svgEl: SVGSVGElement | null = $state(null);
  let dragging = $state(false);
  let dragStart = { x: 0, y: 0, panX: 0, panY: 0 };

  function clamp(v: number, lo: number, hi: number) {
    return Math.max(lo, Math.min(hi, v));
  }

  function onWheel(e: WheelEvent) {
    e.preventDefault();
    if (!svgEl) return;
    const rect = svgEl.getBoundingClientRect();
    const cx = e.clientX - rect.left;
    const cy = e.clientY - rect.top;
    const next = clamp(zoom * (e.deltaY < 0 ? 1.1 : 1 / 1.1), 0.2, 3);
    panX = cx - ((cx - panX) / zoom) * next;
    panY = cy - ((cy - panY) / zoom) * next;
    zoom = next;
  }

  function onPointerDown(e: PointerEvent) {
    dragging = true;
    dragStart = { x: e.clientX, y: e.clientY, panX, panY };
    (e.currentTarget as SVGElement).setPointerCapture(e.pointerId);
  }
  function onPointerMove(e: PointerEvent) {
    if (!dragging) return;
    panX = dragStart.panX + (e.clientX - dragStart.x);
    panY = dragStart.panY + (e.clientY - dragStart.y);
  }
  function onPointerUp(e: PointerEvent) {
    dragging = false;
    (e.currentTarget as SVGElement).releasePointerCapture?.(e.pointerId);
  }

  function resetView() {
    zoom = 1;
    panX = 24;
    panY = 24;
  }

  // ── Table drag ─────────────────────────────────────────────────────────
  let draggedNode = $state<string | null>(null);
  let nodeDragStart = { x: 0, y: 0, nodeX: 0, nodeY: 0 };

  function onNodePointerDown(e: PointerEvent, n: Placed) {
    if (e.button !== 0) return;
    // Keep the background pan handler from also starting.
    e.stopPropagation();
    draggedNode = n.rel.name;
    nodeDragStart = { x: e.clientX, y: e.clientY, nodeX: n.x, nodeY: n.y };
    (e.currentTarget as SVGElement).setPointerCapture(e.pointerId);
  }
  function onNodePointerMove(e: PointerEvent) {
    if (draggedNode === null) return;
    e.stopPropagation();
    manualPos[draggedNode] = {
      x: nodeDragStart.nodeX + (e.clientX - nodeDragStart.x) / zoom,
      y: nodeDragStart.nodeY + (e.clientY - nodeDragStart.y) / zoom,
    };
  }
  function onNodePointerUp(e: PointerEvent) {
    if (draggedNode === null) return;
    e.stopPropagation();
    draggedNode = null;
    (e.currentTarget as SVGElement).releasePointerCapture?.(e.pointerId);
  }

  function relayout() {
    manualPos = {};
  }

  /** The dragged table is drawn last so it stays above the others. */
  let drawOrder = $derived(
    draggedNode === null
      ? nodes
      : [...nodes.filter((n) => n.rel.name !== draggedNode), ...nodes.filter((n) => n.rel.name === draggedNode)],
  );

  function toggleTable(name: string) {
    selected = selectedSet.has(name)
      ? selected.filter((n) => n !== name)
      : [...selected, name];
  }
  function selectAll() {
    selected = relations.map((r) => r.name);
  }
  function clearAll() {
    selected = [];
  }
</script>

<div class="erd">
  <aside class="panel">
    <div class="panel-head">
      <strong>{schema}</strong>
      <span class="muted">{visibleNames.size} shown</span>
    </div>
    <label class="opt">
      <input type="checkbox" bind:checked={includeNeighbors} />
      FK neighbours
    </label>
    <div class="panel-actions">
      <button class="btn-sm" onclick={selectAll}>All</button>
      <button class="btn-sm" onclick={clearAll}>None</button>
      <button class="btn-sm" onclick={resetView}>Reset view</button>
      <button
        class="btn-sm"
        onclick={relayout}
        disabled={Object.keys(manualPos).length === 0}
        title="Discard manual table positions">Re-layout</button
      >
    </div>
    <div class="table-list">
      {#each relations as rel (rel.name)}
        {@const auto = visibleNames.has(rel.name) && !selectedSet.has(rel.name)}
        <label class="row" class:auto>
          <input
            type="checkbox"
            checked={selectedSet.has(rel.name)}
            onchange={() => toggleTable(rel.name)}
          />
          <span class="tname">{rel.name}</span>
          {#if auto}<span class="auto-tag" title="pulled in as an FK neighbour">fk</span>{/if}
        </label>
      {/each}
      {#if relations.length === 0 && !loading}
        <p class="muted">No tables in this schema.</p>
      {/if}
    </div>
  </aside>

  <div class="canvas">
    {#if loading}
      <p class="status">Loading schema…</p>
    {:else if loadError}
      <p class="status error">{loadError}</p>
    {:else if visibleNames.size === 0}
      <p class="status">Select one or more tables to diagram.</p>
    {:else}
      <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
      <svg
        bind:this={svgEl}
        class:dragging
        onwheel={onWheel}
        onpointerdown={onPointerDown}
        onpointermove={onPointerMove}
        onpointerup={onPointerUp}
        role="application"
        aria-label="Entity relationship diagram"
      >
        <defs>
          <marker
            id="erd-arrow"
            viewBox="0 0 10 10"
            refX="9"
            refY="5"
            markerWidth="7"
            markerHeight="7"
            orient="auto-start-reverse"
          >
            <path d="M 0 0 L 10 5 L 0 10 z" fill="var(--text-muted, #888)" />
          </marker>
        </defs>
        <g transform="translate({panX} {panY}) scale({zoom})">
          {#each paths as p}
            <path class="edge" d={p.d} marker-end="url(#erd-arrow)" />
          {/each}
          {#each drawOrder as n (n.rel.name)}
            {@const fkCols = fkColumns(n.rel)}
            <!-- svelte-ignore a11y_no_static_element_interactions -->
            <g
              class="node"
              class:moving={draggedNode === n.rel.name}
              transform="translate({n.x} {n.y})"
              onpointerdown={(e) => onNodePointerDown(e, n)}
              onpointermove={onNodePointerMove}
              onpointerup={onNodePointerUp}
              onpointercancel={onNodePointerUp}
            >
              <rect class="box" width={n.width} height={n.height} rx="5" />
              <rect class="header" width={n.width} height={HEADER_H} rx="5" />
              <rect class="header-foot" y={HEADER_H - 6} width={n.width} height="6" />
              <text class="title" x={n.width / 2} y={HEADER_H / 2} text-anchor="middle">
                {n.rel.name}
              </text>
              {#each n.rel.columns as col, i (col.name)}
                {@const cy = HEADER_H + i * ROW_H + ROW_H / 2 + 2}
                <text
                  class="col"
                  class:pk={col.is_primary_key}
                  x="8"
                  y={cy}
                >{col.is_primary_key ? "🔑 " : fkCols.has(col.name) ? "→ " : ""}{col.name}</text>
                <text class="type" x={n.width - 8} y={cy} text-anchor="end">{col.type_name}</text>
              {/each}
            </g>
          {/each}
        </g>
      </svg>
    {/if}
  </div>
</div>

<style>
  .erd {
    display: flex;
    flex: 1;
    min-height: 0;
    border: 1px solid var(--border-secondary);
    border-radius: 6px;
    overflow: hidden;
    background: var(--bg-base, var(--bg-surface));
  }
  .panel {
    flex: none;
    width: 200px;
    display: flex;
    flex-direction: column;
    gap: 0.4rem;
    padding: 0.5rem;
    border-right: 1px solid var(--border-secondary);
    background: var(--bg-tertiary);
    overflow: hidden;
  }
  .panel-head { display: flex; justify-content: space-between; align-items: baseline; }
  .muted { color: var(--text-muted); font-size: 0.75rem; }
  .opt { display: flex; align-items: center; gap: 0.35rem; font-size: 0.8rem; }
  .panel-actions { display: flex; flex-wrap: wrap; gap: 0.25rem; }
  .btn-sm:disabled { opacity: 0.5; cursor: default; }
  .node { cursor: move; user-select: none; }
  .node.moving .box { stroke: var(--text-accent); stroke-width: 1.5; }
  .btn-sm {
    flex: 1;
    padding: 0.15rem 0.3rem;
    font-size: 0.72rem;
    background: var(--bg-surface);
    color: var(--text-primary);
    border: 1px solid var(--border-secondary);
    border-radius: 3px;
    cursor: pointer;
  }
  .btn-sm:hover { background: var(--bg-hover); }
  .table-list { flex: 1; overflow: auto; display: flex; flex-direction: column; gap: 1px; }
  .row {
    display: flex;
    align-items: center;
    gap: 0.3rem;
    padding: 0.1rem 0.15rem;
    font-size: 0.8rem;
    cursor: pointer;
    border-radius: 3px;
  }
  .row:hover { background: var(--bg-hover); }
  .tname { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .row.auto .tname { color: var(--text-muted); font-style: italic; }
  .auto-tag {
    margin-left: auto;
    font-size: 0.6rem;
    color: var(--text-accent);
    border: 1px solid var(--border-secondary);
    border-radius: 2px;
    padding: 0 0.15rem;
  }
  .canvas { flex: 1; min-width: 0; position: relative; }
  .status { padding: 1rem; color: var(--text-muted); }
  .status.error { color: var(--text-error); white-space: pre-wrap; }
  svg { width: 100%; height: 100%; display: block; cursor: grab; touch-action: none; }
  svg.dragging { cursor: grabbing; }

  .box {
    fill: var(--bg-surface);
    stroke: var(--border-secondary);
    stroke-width: 1;
  }
  .header { fill: var(--bg-tertiary); }
  .header-foot { fill: var(--bg-tertiary); }
  .title {
    fill: var(--text-primary);
    font-size: 12px;
    font-weight: 600;
    dominant-baseline: middle;
  }
  .col {
    fill: var(--text-primary);
    font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
    font-size: 11px;
    dominant-baseline: middle;
  }
  .col.pk { font-weight: 700; }
  .type {
    fill: var(--text-muted);
    font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
    font-size: 11px;
    dominant-baseline: middle;
  }
  .edge {
    fill: none;
    stroke: var(--text-muted, #888);
    stroke-width: 1.5;
  }
</style>
