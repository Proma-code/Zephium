import { loadCodexSession, type CodexSession } from "$domain/codex";
import { CodexEffortIntent } from "./codex-effort.svelte";

/** One picker/profile lifetime, not a document-wide or cross-account catalog cache. */
export class CodexPickerObservation {
  private pending: Promise<CodexSession | null> | null = null;
  private owner: CodexSession | null = null;
  private disposed = false;
  private effortIntent: CodexEffortIntent | null = null;

  get effort() {
    return this.effortIntent;
  }

  cancelEffort() {
    this.effortIntent?.cancelPending();
  }

  constructor(private readonly profile: string) {}

  acquire() {
    if (this.disposed) return Promise.resolve(null);
    return (this.pending ??= loadCodexSession()
      .then(({ CodexSession }) => {
        if (this.disposed) return null;
        this.owner = new CodexSession(this.profile);
        this.effortIntent = new CodexEffortIntent(this.owner);
        return this.owner;
      })
      .catch((error: unknown) => {
        this.pending = null;
        throw error;
      }));
  }

  dispose() {
    this.disposed = true;
    this.effortIntent?.cancel();
    this.effortIntent = null;
    this.owner?.dispose();
    this.owner = null;
  }
}
