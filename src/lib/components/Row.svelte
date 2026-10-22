<script lang="ts">
  import type { Snippet } from 'svelte';

  /** A settings row: label and help on the left, the control on the right. */
  let {
    id,
    label,
    help,
    children,
    below,
  }: {
    /** Used to link the control via `aria-labelledby={id}`. */
    id?: string;
    label: string;
    help?: string;
    children?: Snippet;
    /** Extra content under the row (warnings, lists). */
    below?: Snippet;
  } = $props();
</script>

<div class="row setting-row">
  <div class="text">
    <div class="label" {id}>{label}</div>
    {#if help}<p class="help">{help}</p>{/if}
  </div>
  {#if children}<div class="control">{@render children()}</div>{/if}
  {#if below}<div class="below">{@render below()}</div>{/if}
</div>

<style>
  .row {
    display: grid;
    grid-template-columns: minmax(0, 1fr) auto;
    align-items: center;
    column-gap: 24px;
    padding: 14px 0;
  }
  .label {
    font-size: 13px;
    line-height: 18px;
    font-weight: 500;
  }
  .help {
    margin-top: 2px;
    max-width: 46ch;
    font-size: 12px;
    line-height: 17px;
    color: var(--ink-2);
  }
  .control {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .below {
    grid-column: 1 / -1;
    margin-top: 10px;
  }
</style>
