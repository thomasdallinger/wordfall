<script lang="ts">
  import { XIcon } from '@lucide/svelte';
  import { app } from '../state.svelte';

  /** `full`: centre across the whole window (no sidebar, e.g. during setup) and sit above its footer. */
  let { full = false }: { full?: boolean } = $props();
</script>

<div class="toaster" class:full aria-live="polite">
  {#each app.toasts as t (t.id)}
    <div class="toast {t.kind}" role={t.kind === 'error' ? 'alert' : 'status'}>
      <span class="mark" aria-hidden="true"></span>
      <div class="body">
        <p class="title">{t.title}</p>
        {#if t.message}<p class="message selectable">{t.message}</p>{/if}
      </div>
      <button class="icon-btn close" aria-label="Dismiss" onclick={() => app.dismissToast(t.id)}>
        <XIcon size={13} strokeWidth={1.5} />
      </button>
    </div>
  {/each}
</div>

<style>
  .toaster {
    position: fixed;
    bottom: 20px;
    left: calc(50% + var(--sidebar-width) / 2);
    z-index: 70;
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 8px;
    transform: translateX(-50%);
    pointer-events: none;
  }
  .toaster.full {
    left: 50%;
    bottom: 84px;
  }
  .toast {
    display: flex;
    align-items: flex-start;
    gap: 10px;
    width: max-content;
    max-width: 440px;
    padding: 9px 6px 9px 14px;
    border-radius: var(--radius-sheet);
    background: var(--ink);
    color: var(--surface);
    box-shadow: var(--shadow-float);
    pointer-events: auto;
    animation: appear var(--dur) var(--ease);
  }
  .mark {
    width: 6px;
    height: 6px;
    margin-top: 6px;
    flex-shrink: 0;
    border-radius: 999px;
    background: color-mix(in srgb, var(--surface) 55%, transparent);
  }
  /* On the inverted toast, status colours come from the opposite theme. */
  .error .mark {
    background: #f06a5a;
  }
  .warning .mark {
    background: #e3a43b;
  }
  .success .mark {
    background: #4cc38a;
  }
  @media (prefers-color-scheme: dark) {
    .error .mark {
      background: #c2362b;
    }
    .warning .mark {
      background: #946010;
    }
    .success .mark {
      background: #1a7d44;
    }
  }
  .body {
    min-width: 0;
    padding-right: 4px;
  }
  .title {
    font-size: 13px;
    line-height: 18px;
    font-weight: 500;
  }
  .message {
    margin-top: 1px;
    font-size: 12px;
    line-height: 17px;
    color: color-mix(in srgb, var(--surface) 72%, transparent);
    overflow-wrap: anywhere;
  }
  .close {
    width: 22px;
    height: 22px;
    color: color-mix(in srgb, var(--surface) 70%, transparent);
  }
  .close:hover {
    color: var(--surface);
    background: color-mix(in srgb, var(--surface) 12%, transparent);
  }
</style>
