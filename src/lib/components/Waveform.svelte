<script lang="ts">
  import { waveHeights } from '../levels';

  /** Live input waveform in the signal colour, sized for in-page use. */
  let { levels, bars = 21, height = 28 }: { levels: number[]; bars?: number; height?: number } = $props();
  const heights = $derived(waveHeights(levels, bars));
</script>

<span class="wave" style:height="{height}px" aria-hidden="true">
  {#each heights as h, i (i)}
    <span style:transform="scaleY({h})"></span>
  {/each}
</span>

<style>
  .wave {
    display: inline-flex;
    align-items: center;
    gap: 3px;
  }
  .wave span {
    flex-shrink: 0;
    width: 3px;
    height: 100%;
    border-radius: 999px;
    background: var(--signal);
    transform-origin: center;
    transition: transform 70ms linear;
  }
</style>
