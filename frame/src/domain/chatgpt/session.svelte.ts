import { commands } from "$shared/ipc/chatgpt-bindings";
import type { ChatGptStateV1, ChatGptViewV1 } from "$shared/ipc/chatgpt-bindings";
import { observe } from "$shared/lib/observe";
import { events } from "$shared/ipc/native-events";

const ACCOUNT_STATES: readonly ChatGptStateV1[] = [
  "signed_out",
  "signing_in",
  "connected",
  "unavailable",
];
const READ_TIMEOUT = 5000;
const ACTION_TIMEOUT = 20000;
const POLL_INTERVAL = 1000;
/** Native owns the five-minute deadline; this adds one final bounded readback. */
const POLL_WINDOW = 305000;
type Action = "sign_in" | "cancel" | "sign_out" | "models";

/** Profile-scoped observation only. Rust owns OAuth, credentials and account state. */
export class ChatGptSession {
  view = $state.raw<ChatGptViewV1 | null>(null);
  busy = $state<Action | null>(null);
  checking = $state(false);
  observationFailed = $state(false);
  pollingPaused = $state(false);
  private pendingLogin = $state(false);
  private ownedAttempt: string | null = null;
  private active = false;
  private generation = 0;
  private sequence = 0;
  private readFlight: number | null = null;
  private readAgain = false;
  private stop: (() => void) | null = null;
  private lifetime = new AbortController();
  private timer: ReturnType<typeof setTimeout> | null = null;
  private pollDeadline: number | null = null;

  constructor(readonly profile: string) {}

  get signingIn() {
    return this.pendingLogin || this.view?.state === "signing_in";
  }

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
      !Number.isSafeInteger(view.models) ||
      view.models < 0
    ) {
      this.observationFailed = true;
      return false;
    }
    this.view = view;
    this.observationFailed = false;
    this.pendingLogin = view.state === "signing_in";
    if (!this.pendingLogin) {
      this.ownedAttempt = null;
      this.pollDeadline = null;
      this.pollingPaused = false;
      this.clearTimer();
    } else if (view.login_attempt !== this.ownedAttempt) {
      // Observing a different window's flow does not make this session its owner.
      this.ownedAttempt = null;
    }
    return true;
  }

  private clearTimer() {
    if (this.timer !== null) clearTimeout(this.timer);
    this.timer = null;
  }

  private schedulePoll() {
    this.clearTimer();
    if (!this.active || !this.signingIn || this.pollingPaused) return;
    this.pollDeadline ??= Date.now() + POLL_WINDOW;
    if (Date.now() >= this.pollDeadline) {
      this.pollingPaused = true;
      this.observationFailed = true;
      return;
    }
    this.timer = setTimeout(
      () => {
        this.timer = null;
        void this.read();
      },
      Math.min(POLL_INTERVAL, this.pollDeadline - Date.now()),
    );
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
    } else this.schedulePoll();
  }

  /** An explicit read can resume observation, never silently restart OAuth. */
  refresh() {
    if (this.signingIn) this.pollDeadline = Date.now() + POLL_WINDOW;
    this.pollingPaused = false;
    return this.read();
  }

  private async act(action: Action, request: () => Promise<ChatGptViewV1>) {
    if (!this.active || (this.busy && !(action === "cancel" && this.busy === "sign_in")))
      return false;
    const sequence = ++this.sequence;
    const generation = this.generation;
    this.busy = action;
    this.observationFailed = false;
    this.clearTimer();
    const result = await observe(
      Promise.resolve().then(request),
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
    } else this.schedulePoll();
    return adopted;
  }

  signIn() {
    if (!this.active || this.busy || this.signingIn || this.view?.state !== "signed_out")
      return Promise.resolve(false);
    const attempt = crypto.randomUUID();
    this.pendingLogin = true;
    this.ownedAttempt = attempt;
    this.pollDeadline = Date.now() + POLL_WINDOW;
    this.pollingPaused = false;
    return this.act("sign_in", () => commands.chatgptSignIn(this.profile, attempt));
  }

  cancel() {
    const attempt = this.ownedAttempt ?? this.view?.login_attempt;
    if (!this.signingIn || !attempt) return Promise.resolve(false);
    return this.act("cancel", () => commands.chatgptCancel(this.profile, attempt));
  }

  signOut() {
    if (this.signingIn || this.view?.state !== "connected") return Promise.resolve(false);
    return this.act("sign_out", () => commands.chatgptSignOut(this.profile));
  }

  refreshModels() {
    if (this.signingIn || this.view?.state !== "connected") return Promise.resolve(false);
    return this.act("models", () => commands.chatgptRefreshModels(this.profile));
  }

  dispose() {
    const cancelOwned = this.signingIn ? this.ownedAttempt : null;
    this.active = false;
    this.generation++;
    this.sequence++;
    this.clearTimer();
    this.lifetime.abort();
    this.stop?.();
    this.stop = null;
    this.readFlight = null;
    this.readAgain = false;
    this.checking = false;
    this.busy = null;
    this.view = null;
    this.pendingLogin = false;
    this.ownedAttempt = null;
    this.pollDeadline = null;
    this.pollingPaused = false;
    this.observationFailed = false;
    if (cancelOwned) {
      // Native cancellation only stops an outstanding flow, never a completed account.
      void observe(
        Promise.resolve().then(() => commands.chatgptCancel(this.profile, cancelOwned)),
        READ_TIMEOUT,
      );
    }
  }
}
