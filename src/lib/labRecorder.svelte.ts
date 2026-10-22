// Voice Lab recorder: press-and-hold with a click-to-toggle fallback.
//
//   press()   pointerdown / Space down → lab_start (or stops a latched recording)
//   release() pointerup / pointerleave / Space up →
//               held ≥ TAP_MS: lab_stop
//               held <  TAP_MS: latch (toggle mode) — the next press stops
//   cancel()  Esc → lab_cancel
import { api, on, type Unlisten } from './api';
import { SILENT_LEVELS } from './levels';
import type { LabStopResult } from './types';

/** Releases quicker than this are treated as a click (toggle) rather than a hold. */
export const TAP_MS = 250;

export type LabPhase = 'idle' | 'recording' | 'processing';

export interface LabRecorderHooks {
  onresult: (r: LabStopResult) => void;
  /** Recording ended without a result: `message` is null when the user cancelled. */
  onfail: (message: string | null, stage: 'start' | 'stop') => void;
}

function message(e: unknown): string {
  return e instanceof Error ? e.message : String(e ?? '');
}

export class LabRecorder {
  phase = $state<LabPhase>('idle');
  /** True once a quick tap turned the recording into toggle mode. */
  latched = $state(false);
  levels = $state<number[]>(SILENT_LEVELS);
  elapsed = $state(0);

  #hooks: LabRecorderHooks;
  #holding = false;
  #pressedAt = 0;
  #started: Promise<boolean> = Promise.resolve(false);
  #clock: ReturnType<typeof setInterval> | null = null;
  #unlisten: Unlisten | null = null;
  #disposed = false;

  constructor(hooks: LabRecorderHooks) {
    this.#hooks = hooks;
  }

  async attach() {
    const un = await on('lab-levels', ({ levels }) => {
      if (this.phase === 'recording') this.levels = levels;
    });
    if (this.#disposed) un();
    else this.#unlisten = un;
  }

  /** Cancels an in-flight clip and stops listening (component teardown). */
  dispose() {
    this.#disposed = true;
    if (this.phase === 'recording') void this.cancel();
    this.#unlisten?.();
    this.#unlisten = null;
    this.#stopClock();
  }

  press(promptId: string) {
    if (this.phase === 'recording') {
      if (this.latched) void this.stop();
      return;
    }
    if (this.phase !== 'idle') return;
    this.#holding = true;
    this.#pressedAt = performance.now();
    this.latched = false;
    this.phase = 'recording';
    this.#startClock();
    this.#started = api.labStart(promptId).then(
      () => true,
      (e) => {
        this.#reset();
        this.#hooks.onfail(message(e) || 'Could not start recording.', 'start');
        return false;
      },
    );
  }

  release() {
    if (!this.#holding) return;
    this.#holding = false;
    if (this.phase !== 'recording') return;
    if (performance.now() - this.#pressedAt < TAP_MS) this.latched = true;
    else void this.stop();
  }

  /** Keyboard-activated click on the record button: start or stop without holding. */
  toggle(promptId: string) {
    if (this.phase === 'recording') {
      void this.stop();
    } else if (this.phase === 'idle') {
      this.press(promptId);
      this.#holding = false;
      this.latched = true;
    }
  }

  async stop() {
    if (this.phase !== 'recording') return;
    this.#holding = false;
    this.phase = 'processing';
    this.#stopClock();
    this.levels = SILENT_LEVELS;
    if (!(await this.#started)) return; // start failed and already reported
    try {
      const r = await api.labStop();
      this.#reset();
      this.#hooks.onresult(r);
    } catch (e) {
      this.#reset();
      this.#hooks.onfail(message(e) || 'Could not save the clip.', 'stop');
    }
  }

  async cancel() {
    if (this.phase !== 'recording') return;
    this.#reset();
    const started = await this.#started;
    if (started) await api.labCancel().catch(() => {});
    this.#hooks.onfail(null, 'stop');
  }

  #reset() {
    this.phase = 'idle';
    this.latched = false;
    this.#holding = false;
    this.levels = SILENT_LEVELS;
    this.#stopClock();
  }

  #startClock() {
    this.elapsed = 0;
    this.#stopClock();
    const t0 = performance.now();
    this.#clock = setInterval(() => (this.elapsed = (performance.now() - t0) / 1000), 100);
  }

  #stopClock() {
    if (this.#clock) clearInterval(this.#clock);
    this.#clock = null;
  }
}
