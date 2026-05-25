<script lang="ts">
  let {
    orientation = "horizontal",
    onResize,
  }: {
    orientation?: "horizontal" | "vertical";
    onResize: (delta: number) => void;
  } = $props();

  function onPointerDown(e: PointerEvent) {
    e.preventDefault();
    (e.target as HTMLElement).setPointerCapture(e.pointerId);
    const start = orientation === "horizontal" ? e.clientX : e.clientY;
    let prev = start;

    function move(ev: PointerEvent) {
      const current = orientation === "horizontal" ? ev.clientX : ev.clientY;
      const delta = current - prev;
      prev = current;
      onResize(delta);
    }

    function up() {
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", up);
    }

    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", up);
  }
</script>

<div
  class="resizer"
  class:horizontal={orientation === "horizontal"}
  class:vertical={orientation === "vertical"}
  onpointerdown={onPointerDown}
  role="separator"
  aria-orientation={orientation}
></div>

<style>
  .resizer {
    flex-shrink: 0;
    background: transparent;
    transition: background 0.15s;
    z-index: 10;
  }
  .resizer:hover,
  .resizer:active {
    background: var(--resizer-hover);
  }
  .horizontal {
    width: 5px;
    cursor: col-resize;
    align-self: stretch;
  }
  .vertical {
    height: 5px;
    cursor: row-resize;
    align-self: stretch;
  }
</style>
