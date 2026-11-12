<script lang="ts">
  import { tick } from 'svelte';
  import { app } from '../state.svelte';
  import type { MapEdge, MapNode, MapNodeKind, QuestionStatus, SystemMap } from '../types';

  /**
   * The meeting's system map, drawn roughly: systems in a column per kind (ours, the customer's,
   * outside services), people and roles in a row below, a line for each flow labelled with what
   * flows and how (dashed when how is unknown), and what is still open. Click a box to see what is
   * known about it; while the meeting records, rename it or delete it if it's wrong. Names said but
   * not (yet) on the map are listed in one quiet line under it, so nothing is overlooked.
   */
  let {
    meetingId,
    map,
    status = 'off',
    editable = false,
    alsoMentioned = [],
  }: {
    meetingId: string;
    map: SystemMap;
    /** What the live model is doing (shared with the questions). */
    status?: QuestionStatus;
    editable?: boolean;
    /** Names said but not on the map, the latest first. */
    alsoMentioned?: string[];
  } = $props();

  const uid = $props.id();

  const LANES: { kind: MapNodeKind; label: string }[] = [
    { kind: 'ours', label: 'Ours' },
    { kind: 'customer', label: 'Customer' },
    { kind: 'outside', label: 'Outside services' },
  ];
  const KIND: Record<MapNodeKind, string> = {
    ours: 'Our system',
    customer: 'Customer system',
    outside: 'Outside service',
    person: 'Person or role',
  };
  /** Open points listed under the picture before "and N more". */
  const OPEN_SHOWN = 4;

  const lanes = $derived(LANES.map((l) => ({ ...l, nodes: map.nodes.filter((n) => n.kind === l.kind) })).filter((l) => l.nodes.length > 0));
  const people = $derived(map.nodes.filter((n) => n.kind === 'person'));
  const names = $derived(new Map(map.nodes.map((n) => [n.id, n.name])));
  const openCount = $derived.by(() => {
    const c = new Map<number, number>();
    for (const o of map.open) c.set(o.node, (c.get(o.node) ?? 0) + 1);
    return c;
  });

  let selectedId = $state<number | null>(null);
  const selected = $derived(map.nodes.find((n) => n.id === selectedId) ?? null);
  let draft = $state('');

  function select(n: MapNode) {
    if (selectedId === n.id) {
      selectedId = null;
      return;
    }
    selectedId = n.id;
    draft = n.name;
  }

  function rename() {
    const n = selected;
    if (!n || !draft.trim() || draft.trim() === n.name) return;
    void app.editMapNode(meetingId, n.id, draft.trim());
  }

  function remove() {
    const n = selected;
    if (!n) return;
    selectedId = null;
    void app.editMapNode(meetingId, n.id, null);
  }

  // ---- lines between the boxes, from where the boxes ended up ----

  let canvas: HTMLElement | undefined = $state();
  type Box = { x: number; y: number; w: number; h: number };
  let boxes = $state<Record<number, Box>>({});
  let size = $state({ w: 0, h: 0 });

  function measure() {
    if (!canvas) return;
    const base = canvas.getBoundingClientRect();
    const next: Record<number, Box> = {};
    for (const el of canvas.querySelectorAll<HTMLElement>('[data-node]')) {
      const r = el.getBoundingClientRect();
      next[Number(el.dataset.node)] = { x: r.left - base.left, y: r.top - base.top, w: r.width, h: r.height };
    }
    boxes = next;
    size = { w: base.width, h: base.height };
  }

  $effect(() => {
    void map.nodes.map((n) => n.name + n.kind).join();
    void tick().then(measure);
  });

  $effect(() => {
    if (!canvas) return;
    const ro = new ResizeObserver(() => measure());
    ro.observe(canvas);
    return () => ro.disconnect();
  });

  /** Where the line from a box's centre towards (dx, dy) leaves the box. */
  function exit(b: Box, dx: number, dy: number, pad: number) {
    const hw = b.w / 2 + pad;
    const hh = b.h / 2 + pad;
    const t = Math.min(dx === 0 ? Infinity : hw / Math.abs(dx), dy === 0 ? Infinity : hh / Math.abs(dy));
    return { x: b.x + b.w / 2 + dx * t, y: b.y + b.h / 2 + dy * t };
  }

  function short(text: string, max: number): string {
    return text.length > max ? `${text.slice(0, max - 1).trimEnd()}…` : text;
  }

  function unknownHow(how: string): boolean {
    return ['', '?', 'unknown', 'unbekannt', 'unklar'].includes(how.trim().toLowerCase());
  }

  function edgeLabel(e: MapEdge): string {
    // At a glance: what, and the gist of how ("by hand", not "by hand, every morning").
    return unknownHow(e.how) ? short(e.what, 24) : `${short(e.what, 18)} (${short(e.how.split(',')[0].trim(), 12)})`;
  }

  const lines = $derived.by(() => {
    const out: { key: string; d: string; lx: number; ly: number; label: string; title: string; unknown: boolean }[] = [];
    map.edges.forEach((e, i) => {
      const a = boxes[e.from];
      const b = boxes[e.to];
      if (!a || !b) return;
      const ax = a.x + a.w / 2, ay = a.y + a.h / 2, bx = b.x + b.w / 2, by = b.y + b.h / 2;
      const title = `${names.get(e.from)} sends ${e.what} to ${names.get(e.to)}${unknownHow(e.how) ? '' : `, ${e.how}`}`;
      if (Math.abs(ax - bx) < 24) {
        // Same column: bow out to the right so the line doesn't run under the boxes between.
        const sx = a.x + a.w, tx = b.x + b.w + 6, bow = Math.min(a.w, 56);
        const cx = Math.max(sx, tx) + bow;
        const d = `M ${sx} ${ay} C ${cx} ${ay}, ${cx} ${by}, ${tx} ${by}`;
        out.push({ key: `${i}`, d, lx: cx - bow / 4, ly: (ay + by) / 2, label: edgeLabel(e), title, unknown: unknownHow(e.how) });
        return;
      }
      const dx = bx - ax, dy = by - ay, len = Math.hypot(dx, dy) || 1;
      const s = exit(a, dx / len, dy / len, 0);
      const t = exit(b, -dx / len, -dy / len, 4);
      out.push({ key: `${i}`, d: `M ${s.x} ${s.y} L ${t.x} ${t.y}`, lx: (s.x + t.x) / 2, ly: (s.y + t.y) / 2, label: edgeLabel(e), title, unknown: unknownHow(e.how) });
    });
    // Labels that would sit on one another: move the later one down a line.
    const placed: { x: number; y: number; w: number }[] = [];
    for (const l of out) {
      const w = l.label.length * 5.6;
      while (placed.some((p) => Math.abs(p.y - l.ly) < 13 && Math.abs(p.x - l.lx) < (p.w + w) / 2)) l.ly += 13;
      placed.push({ x: l.lx, y: l.ly, w });
    }
    return out;
  });

  const flows = $derived(
    selected
      ? map.edges
          .filter((e) => e.from === selected.id || e.to === selected.id)
          .map((e) => {
            const how = unknownHow(e.how) ? '' : `, ${e.how}`;
            return e.from === selected.id
              ? `Sends ${e.what} to ${names.get(e.to)}${how}`
              : `Gets ${e.what} from ${names.get(e.from)}${how}`;
          })
      : [],
  );

  const empty = $derived.by(() => {
    switch (status) {
      case 'no-model':
        return 'Download a summary model in Settings › Meetings to get a map while you talk.';
      case 'starting':
        return 'Starting the model…';
      case 'failed':
        return 'The map stopped because the model failed. The transcript goes on.';
      case 'off':
        return editable ? 'No map is built for this meeting.' : 'No systems or people came up.';
      default:
        return 'Systems, people and how they connect appear here as they come up. Say in My notes what to focus on, for example “focus on the ERP export”.';
    }
  });

  function nodeLabel(n: MapNode): string {
    const open = openCount.get(n.id) ?? 0;
    return `${n.name}, ${KIND[n.kind].toLowerCase()}${open ? `, ${open} open` : ''}`;
  }
</script>

{#snippet box(n: MapNode)}
  <button
    class={['node', n.kind, { selected: selectedId === n.id }]}
    data-node={n.id}
    aria-pressed={selectedId === n.id}
    aria-label={nodeLabel(n)}
    title={n.known[0] ?? KIND[n.kind]}
    onclick={() => select(n)}
  >
    <span class="name">{n.name}</span>
    {#if openCount.get(n.id)}<span class="open-mark" aria-hidden="true">{openCount.get(n.id)} open</span>{/if}
  </button>
{/snippet}

<div class="system-map">
  {#if map.nodes.length === 0}
    <p class="empty">{empty}</p>
  {:else}
    <div class="canvas" bind:this={canvas}>
      <svg class="edges" width={size.w} height={size.h} aria-hidden="true">
        <defs>
          <marker id="arrow-{uid}" viewBox="0 0 8 8" refX="7" refY="4" markerWidth="7" markerHeight="7" orient="auto-start-reverse">
            <path d="M0,0.5 L7.5,4 L0,7.5" fill="none" stroke="context-stroke" stroke-width="1.4" />
          </marker>
        </defs>
        {#each lines as l (l.key)}
          <path d={l.d} class={['line', { unknown: l.unknown }]} marker-end="url(#arrow-{uid})"><title>{l.title}</title></path>
        {/each}
        {#each lines as l (l.key)}
          <text x={l.lx} y={l.ly} class="label" text-anchor="middle" dominant-baseline="middle">{l.label}</text>
        {/each}
      </svg>
      {#if lanes.length > 0}
        <div class="lanes" style:grid-template-columns="repeat({lanes.length}, minmax(0, 1fr))">
          {#each lanes as lane (lane.kind)}
            <div class="lane">
              <p class="lane-label">{lane.label}</p>
              {#each lane.nodes as n (n.id)}
                {@render box(n)}
              {/each}
            </div>
          {/each}
        </div>
      {/if}
      {#if people.length > 0}
        <div class="people">
          <p class="lane-label">People and roles</p>
          <div class="people-row">
            {#each people as n (n.id)}
              {@render box(n)}
            {/each}
          </div>
        </div>
      {/if}
    </div>

    {#if selected}
      <div class="detail" role="region" aria-label="About {selected.name}">
        <div class="detail-head">
          {#if editable}
            <input
              class="field name-field"
              bind:value={draft}
              aria-label="Name"
              spellcheck="false"
              onblur={rename}
              onkeydown={(e) => {
                if (e.key === 'Enter') (e.currentTarget as HTMLInputElement).blur();
                if (e.key === 'Escape') {
                  draft = selected.name;
                  selectedId = null;
                }
              }}
            />
            <button class="btn btn-quiet small" aria-label="Delete {selected.name}" title="Remove this box; it won't come back" onclick={remove}>Delete</button>
          {:else}
            <h3 class="detail-name">{selected.name}</h3>
          {/if}
        </div>
        <p class="detail-kind">{KIND[selected.kind]}</p>
        {#if selected.known.length || flows.length || openCount.get(selected.id)}
          <ul class="facts">
            {#each selected.known as k, i (i)}<li>{k}</li>{/each}
            {#each flows as f, i (i)}<li>{f}</li>{/each}
            {#each map.open.filter((o) => o.node === selected.id) as o (o.number)}<li class="open">Open: {o.text}</li>{/each}
          </ul>
        {:else}
          <p class="detail-kind">Nothing more was said about it yet.</p>
        {/if}
      </div>
    {/if}

    {#if map.open.length > 0}
      <div class="still-open">
        <h3>Still open</h3>
        <ul>
          {#each map.open.slice(-OPEN_SHOWN) as o (o.number)}
            <li><span class="on">{names.get(o.node)}:</span> {o.text}</li>
          {/each}
        </ul>
        {#if map.open.length > OPEN_SHOWN}
          <p class="more">and {map.open.length - OPEN_SHOWN} more (click a box to see its own)</p>
        {/if}
      </div>
    {/if}
  {/if}
  {#if alsoMentioned.length > 0}
    <p class="also">Also mentioned: {alsoMentioned.join(', ')}</p>
  {/if}
</div>

<style>
  .empty {
    max-width: 52ch;
    font-size: 13px;
    line-height: 20px;
    color: var(--ink-2);
  }
  .canvas {
    position: relative;
    padding: 4px 0 8px;
  }
  .edges {
    position: absolute;
    inset: 0;
    overflow: visible;
    pointer-events: none;
    color: var(--ink-3);
  }
  .line {
    fill: none;
    stroke: var(--ink-3);
    stroke-width: 1.2;
  }
  .line.unknown {
    stroke-dasharray: 4 3;
  }
  .label {
    font-size: 11px;
    fill: var(--ink-2);
    paint-order: stroke;
    stroke: var(--surface);
    stroke-width: 4px;
    stroke-linejoin: round;
  }
  .lanes {
    position: relative;
    display: grid;
    column-gap: 56px;
    align-items: start;
  }
  .lane {
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: 18px;
    min-width: 0;
  }
  .lane-label {
    font-size: 12px;
    line-height: 16px;
    color: var(--ink-3);
  }
  .people {
    position: relative;
    margin-top: 28px;
  }
  .people-row {
    display: flex;
    flex-wrap: wrap;
    gap: 12px 24px;
    margin-top: 8px;
  }
  .node {
    position: relative;
    display: inline-flex;
    align-items: baseline;
    gap: 6px;
    max-width: 100%;
    padding: 6px 10px;
    border-radius: var(--radius-control, 6px);
    border: 1px solid var(--line);
    background: var(--surface);
    font-size: 13px;
    line-height: 18px;
    font-weight: 500;
    color: var(--ink);
    text-align: left;
    overflow-wrap: anywhere;
    animation: arrive 160ms ease-out;
  }
  .node.ours {
    border: 1.5px solid var(--ink);
    font-weight: 700;
  }
  .node.outside {
    border-style: dashed;
    border-color: var(--ink-3);
  }
  .node.person {
    border-radius: 999px;
    font-weight: 400;
  }
  /* Opaque, so line labels underneath don't show through. */
  .node:hover {
    box-shadow: inset 0 0 0 999px var(--hover);
  }
  .node.selected {
    box-shadow: inset 0 0 0 999px var(--press);
    border-color: var(--ink-2);
  }
  .open-mark {
    flex-shrink: 0;
    font-size: 11px;
    font-weight: 400;
    color: var(--ink-3);
    white-space: nowrap;
  }
  .detail {
    margin-top: 16px;
    padding: 12px 14px;
    border-radius: var(--radius-sheet, 10px);
    background: var(--bg);
  }
  .detail-head {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .name-field {
    flex: 1;
    min-width: 0;
    height: 28px;
    font-size: 13px;
    font-weight: 700;
  }
  .detail-name {
    font-size: 13px;
    font-weight: 700;
  }
  .detail-kind {
    margin-top: 4px;
    font-size: 12px;
    color: var(--ink-3);
  }
  .facts {
    margin-top: 8px;
    padding-left: 18px;
    list-style: disc;
    font-size: 13px;
    line-height: 20px;
    color: var(--ink);
  }
  .facts .open {
    color: var(--ink-2);
  }
  .btn.small {
    height: 24px;
    padding: 0 8px;
    font-size: 12px;
  }
  .still-open {
    margin-top: 16px;
  }
  .still-open h3 {
    font-size: 12px;
    font-weight: 700;
    color: var(--ink-2);
  }
  .still-open ul {
    margin-top: 4px;
    font-size: 13px;
    line-height: 20px;
    color: var(--ink);
  }
  .still-open .on {
    font-weight: 700;
  }
  .more {
    font-size: 12px;
    color: var(--ink-3);
  }
  .also {
    margin-top: 12px;
    max-width: 72ch;
    font-size: 12px;
    line-height: 18px;
    color: var(--ink-2);
  }
  @keyframes arrive {
    from {
      opacity: 0;
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .node {
      animation: none;
    }
  }
</style>
