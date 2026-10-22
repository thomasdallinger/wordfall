<script lang="ts">
  import { tick } from 'svelte';

  let {
    title,
    message,
    confirmLabel,
    onconfirm,
  }: {
    title: string;
    message: string;
    confirmLabel: string;
    onconfirm: () => void;
  } = $props();

  let dialog: HTMLDialogElement | undefined = $state();
  let cancelBtn: HTMLButtonElement | undefined = $state();

  /** Opens as a native modal dialog, which traps focus and restores it on close. */
  export async function show() {
    dialog?.showModal();
    await tick();
    cancelBtn?.focus();
  }

  function close() {
    dialog?.close();
  }
</script>

<dialog
  bind:this={dialog}
  class="sheet"
  aria-labelledby="confirm-title"
  aria-describedby="confirm-message"
  onclick={(e) => e.target === dialog && close()}
>
  <h2 id="confirm-title" class="title">{title}</h2>
  <p id="confirm-message" class="message">{message}</p>
  <div class="buttons">
    <button class="btn btn-quiet" bind:this={cancelBtn} onclick={close}>Cancel</button>
    <button
      class="btn btn-bad"
      onclick={() => {
        close();
        onconfirm();
      }}>{confirmLabel}</button
    >
  </div>
</dialog>

<style>
  .sheet {
    width: 360px;
    margin: auto;
    padding: 20px;
    border: none;
    border-radius: var(--radius-sheet);
    color: var(--ink);
    background: var(--surface);
    box-shadow:
      0 0 0 1px var(--line),
      var(--shadow-float);
  }
  .sheet[open] {
    animation: appear var(--dur) var(--ease);
  }
  .sheet::backdrop {
    background: var(--scrim);
  }
  .title {
    font-size: 15px;
    line-height: 20px;
  }
  .message {
    margin-top: 6px;
    font-size: 13px;
    line-height: 19px;
    color: var(--ink-2);
  }
  .buttons {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
    margin-top: 20px;
  }
</style>
