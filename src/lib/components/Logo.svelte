<script lang="ts">
  /**
   * The app mark: a microphone whose grille is lines of text. Voice in, words out.
   * The stand is drawn in `currentColor`; `accent` colours the head (e.g. the signal colour).
   * The grille is cut out of the head, so it shows whatever is behind the mark.
   */
  let { size = 20, accent = 'currentColor' }: { size?: number; accent?: string } = $props();
  const uid = $props.id();
  const mask = `logo-grille-${uid}`;
</script>

<svg width={size} height={size} viewBox="0 0 32 32" fill="none" aria-hidden="true">
  <mask id={mask}>
    <rect x="10" y="1.5" width="12" height="19" rx="6" fill="#fff" />
    <g stroke="#000" stroke-width="1.3" stroke-linecap="round">
      <path d="M13.2 6.3h5.6" />
      <path d="M13.2 9.2h4" />
      <path d="M13.2 12.1h5.6" />
      <path d="M13.2 15h2.9" />
    </g>
  </mask>
  <rect x="10" y="1.5" width="12" height="19" rx="6" fill={accent} mask="url(#{mask})" />
  <g stroke="currentColor" stroke-width="2" stroke-linecap="round">
    <path d="M6.2 14a9.8 9.8 0 0 0 19.6 0" />
    <path d="M16 23.8v4.8" />
    <path d="M10.8 28.6h10.4" />
  </g>
</svg>
