import { loadCodexSession, type CodexSession } from "$domain/codex";

/** One picker/profile lifetime, not a document-wide or cross-account catalog cache. */
export class CodexPickerObservation {
  private pending: Promise<CodexSession | null> | null = null;
  private owner: CodexSession | null = null;
  private disposed = false;

  constructor(private readonly profile: string) {}

  acquire() {
    if (this.disposed) return Promise.resolve(null);
    return (this.pending ??= loadCodexSession()
      .then(({ CodexSession }) => {
        if (this.disposed) return null;
        this.owner = new CodexSession(this.profile);
        return this.owner;
      })
      .catch((error: unknown) => {
        this.pending = null;
        throw error;
      }));
  }

  dispose() {
    this.disposed = true;
    this.owner?.dispose();
    this.owner = null;
  }
}
