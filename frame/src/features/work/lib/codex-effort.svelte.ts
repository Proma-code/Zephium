import type { CodexSession } from "$domain/codex";
import type { CodexSelectionV1 } from "$shared/ipc/codex-bindings";

/** Transient interaction only. The committed selection always remains in the native view. */
export class CodexEffortIntent {
  desired = $state.raw<CodexSelectionV1 | null>(null);
  private sending = false;
  private live = true;
  private generation: string | null = null;

  constructor(
    private readonly owner: CodexSession,
    private confirmed?: () => void,
  ) {}

  /** Only the currently mounted pane may react to a confirmed preference. */
  attach(confirmed: () => void) {
    this.confirmed = confirmed;
    return () => {
      if (this.confirmed === confirmed) this.confirmed = undefined;
    };
  }

  request(selection: CodexSelectionV1) {
    const view = this.owner.view;
    if (
      !this.live ||
      !view?.catalog_generation ||
      view.state !== "connected" ||
      (this.owner.busy && this.owner.busy !== "selection")
    )
      return;
    if (this.sending && view.catalog_generation !== this.generation) {
      this.desired = null;
      return;
    }
    this.generation = view.catalog_generation;
    this.desired = selection;
    if (!this.sending) void this.drain();
  }

  private async drain() {
    this.sending = true;
    while (this.live && this.desired) {
      const selected = this.desired;
      const generation = this.generation;
      const success = await this.owner.select(selected, generation);
      if (!this.live) break;
      const view = this.owner.view;
      if (!success || view?.state !== "connected" || view.catalog_generation !== generation) {
        // A rejected or unobserved write is not safe to replay. Native readback
        // may still confirm it later, but never revives this gesture's queue.
        this.desired = null;
        break;
      }
      if (this.desired === selected) {
        this.desired = null;
        this.confirmed?.();
      }
    }
    this.sending = false;
  }

  cancelPending() {
    this.desired = null;
  }

  cancel() {
    // This drops only unsent intent; an admitted native commit is not canceled.
    this.live = false;
    this.confirmed = undefined;
    this.desired = null;
  }
}
