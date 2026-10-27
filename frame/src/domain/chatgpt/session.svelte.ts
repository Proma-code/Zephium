import { commands } from "$shared/ipc/chatgpt-bindings";
import type { ChatGptStateV1, ChatGptViewV1 } from "$shared/ipc/chatgpt-bindings";
import { observe } from "$shared/lib/observe";
import { events } from "$shared/ipc/native-events";

const ACCOUNT_STATES: readonly ChatGptStateV1[] = ["signed_out", "connected", "unavailable"];
const READ_TIMEOUT = 5000;
const ACTION_TIMEOUT = 20000;

/** Read-only legacy registration observation plus explicit disconnect, never login. */
export class ChatGptSession {
  view = $state.raw<ChatGptViewV1 | null>(null);
  busy = $state<"sign_out" | null>(null);
  checking = $state(false);
  observationFailed = $state(false);
  private active = false;
  private generation = 0;
  private sequence = 0;
  private readFlight: number | null = null;
  private readAgain = false;
  private stop: (() => void) | null = null;
  private lifetime = new AbortController();

  constructor(readonly profile: string) {}

  async start() {
    if (this.active) return;
    this.active = true;
    const generation = ++this.generation;
    this.lifetime = new AbortController();
    const stop = await events.workModelsChanged.listen(() => {
      if (!this.busy) this.sequence++;
      void this.read();
    });
    if (!this.active || generation !== this.generation) {
      stop();
      return;
    }
    this.stop = stop;
    await this.read();
  }

  private adopt(view: ChatGptViewV1): boolean {
    if (
      view.version !== 1 ||
      view.profile !== this.profile ||
      !ACCOUNT_STATES.includes(view.state) ||
      !["missing", "stored", "unavailable"].includes(view.presence) ||
      (view.presence === "missing" && view.state !== "signed_out")
    ) {
      this.observationFailed = true;
      return false;
    }
    this.view = view;
    this.observationFailed = false;
    return true;
  }

  private async read() {
    if (!this.active) return;
    if (this.busy || this.readFlight !== null) {
      this.readAgain = true;
      return;
    }
    const sequence = ++this.sequence;
    const generation = this.generation;
    this.readFlight = sequence;
    this.checking = true;
    const result = await observe(
      Promise.resolve().then(() => commands.chatgptAccount(this.profile)),
      READ_TIMEOUT,
      this.lifetime.signal,
    );
    if (generation !== this.generation) return;
    if (this.readFlight === sequence) this.readFlight = null;
    this.checking = false;
    if (sequence === this.sequence) {
      if (result.state === "received") this.adopt(result.value);
      else this.observationFailed = true;
    }
    if (this.readAgain && !this.busy) {
      this.readAgain = false;
      void this.read();
    }
  }

  /** Retry inconclusive storage observation without creating a new registration. */
  refresh() {
    return this.read();
  }

  async signOut() {
    if (!this.active || this.busy || this.view?.presence !== "stored") return false;
    const sequence = ++this.sequence;
    const generation = this.generation;
    this.busy = "sign_out";
    this.observationFailed = false;
    const result = await observe(
      Promise.resolve().then(() => commands.chatgptSignOut(this.profile)),
      ACTION_TIMEOUT,
      this.lifetime.signal,
    );
    if (generation !== this.generation || sequence !== this.sequence) return false;
    this.busy = null;
    const adopted = result.state === "received" && this.adopt(result.value);
    if (!adopted) this.observationFailed = true;
    if (this.readAgain) {
      this.readAgain = false;
      void this.read();
    }
    return adopted;
  }

  dispose() {
    this.active = false;
    this.generation++;
    this.sequence++;
    this.lifetime.abort();
    this.stop?.();
    this.stop = null;
    this.readFlight = null;
    this.readAgain = false;
    this.checking = false;
    this.busy = null;
    this.view = null;
    this.observationFailed = false;
  }
}
