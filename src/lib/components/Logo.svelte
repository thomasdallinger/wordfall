<script lang="ts">
  /**
   * The app mark: words falling into place. Two word-bars tumble in, fading like a
   * motion trail, and the last one lands flat, closed by a full stop.
   * Drawn in `currentColor`; `dot` colours the full stop separately (e.g. the signal colour).
   */
  let { size = 20, dot = 'currentColor' }: { size?: number; dot?: string } = $props();
</script>

<svg width={size} height={size} viewBox="0 0 32 32" fill="none" aria-hidden="true">
  <g stroke="currentColor" stroke-width="3.4" stroke-linecap="round">
    <path d="M18.2 8.2l6.6-2.4" opacity="0.5" />
    <path d="M11 16.2l9.6-1.7" opacity="0.78" />
    <path d="M5.6 24.2h15.2" />
  </g>
  <circle cx="27.2" cy="24.2" r="2.6" fill={dot} />
</svg>
