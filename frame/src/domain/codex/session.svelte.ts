import { sameCodexEffort, supportsCodexEffort } from "./selection";
import { commands } from "$shared/ipc/codex-bindings";
import type { CodexStateV1, CodexViewV1 } from "$shared/ipc/codex-bindings";
import type { CodexSelectionV1 } from "$shared/ipc/codex-bindings";
import { validCodexView } from "./validation";
import { observe } from "$shared/lib/observe";
import { events } from "$shared/ipc/native-events";

const ACCOUNT_STATES: readonly CodexStateV1[] = [
  "signed_out",
  "signing_in",
  "connected",
  "unavailable",
  "preparing",
  "recovery_required",
];
const READ_TIMEOUT = 5000;
const ACTION_TIMEOUT = 20000;
const POLL_INTERVAL = 1000;
/** Native owns the five-minute deadline; this adds one final bounded readback. */
const POLL_WINDOW = 305000;
type Action = "check" | "sign_in" | "cancel" | "sign_out" | "models" | "quota" | "selection";

/** Profile-scoped observation only. Rust owns the original runtime, credentials and account state. */
export class CodexSession {
  view = $state.raw<CodexViewV1 | null>(null);
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

  private adopt(view: CodexViewV1): boolean {
    if (
      !validCodexView(view) ||
      view.profile !== this.profile ||
      !ACCOUNT_STATES.includes(view.state)
    ) {
      this.observationFailed = true;
      return false;
    }
    this.view = view;
    this.observationFailed = false;
    this.pendingLogin = view.state === "signing_in";
    if (!this.pendingLogin && view.state !== "preparing" && view.runtime !== "preparing") {
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
    if (
      !this.active ||
      (!this.signingIn && this.view?.state !== "preparing" && this.view?.runtime !== "preparing") ||
      this.pollingPaused
    )
      return;
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
      Promise.resolve().then(() => commands.codexAccount(this.profile)),
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

  /** Explicit recovery intent may recheck/restart the native owner; polling never does. */
  refresh() {
    if (this.signingIn) this.pollDeadline = Date.now() + POLL_WINDOW;
    this.pollingPaused = false;
    return this.act("check", () => commands.codexCheckConnection(this.profile));
  }

  private async act(action: Action, request: () => Promise<CodexViewV1>) {
    if (!this.active || (this.busy && !(action === "cancel" && this.busy === "sign_in")))
      return false;
    const sequence = ++this.sequence;
    const generation = this.generation;
    const observingWake = this.view?.runtime === "dormant" || this.view?.runtime === "preparing";
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
    // A slow explicit wake may outlive this renderer's observation. Reconcile
    // once even if its Preparing event was missed; Account never wakes/replays.
    if (this.readAgain || (observingWake && result.state === "timeout")) {
      this.readAgain = false;
      void this.read();
    } else this.schedulePoll();
    return (
      adopted && (!this.view?.error || (action === "cancel" && this.view.error === "cancelled"))
    );
  }

  signIn() {
    if (
      !this.active ||
      this.busy ||
      this.signingIn ||
      !this.view ||
      !["signed_out", "unavailable"].includes(this.view.state)
    )
      return Promise.resolve(false);
    const attempt = crypto.randomUUID();
    this.pendingLogin = true;
    this.ownedAttempt = attempt;
    this.pollDeadline = Date.now() + POLL_WINDOW;
    this.pollingPaused = false;
    return this.act("sign_in", () => commands.codexConnect(this.profile, attempt));
  }

  cancel() {
    const attempt = this.ownedAttempt ?? this.view?.login_attempt;
    if (!this.signingIn || !attempt) return Promise.resolve(false);
    return this.act("cancel", () => commands.codexCancelLogin(this.profile, attempt));
  }

  signOut() {
    if (this.signingIn || this.view?.state !== "connected") return Promise.resolve(false);
    return this.act("sign_out", () => commands.codexLogout(this.profile));
  }

  refreshModels() {
    if (this.signingIn || this.view?.state !== "connected") return Promise.resolve(false);
    return this.act("models", () => commands.codexRefreshModels(this.profile));
  }

  refreshQuota() {
    if (this.signingIn || this.view?.state !== "connected") return Promise.resolve(false);
    return this.act("quota", () => commands.codexRefreshQuota(this.profile));
  }

  /** Explicit selection only; never infer a wire model or an unsupported effort. */
  async select(selection: CodexSelectionV1 | null) {
    if (!this.view || this.signingIn) return false;
    if (
      selection &&
      (this.view.state !== "connected" ||
        !this.view.models.some(
          (model) => model.id === selection.model && supportsCodexEffort(model, selection.effort),
        ))
    )
      return false;
    if (!(await this.act("selection", () => commands.codexSelectModel(this.profile, selection))))
      return false;
    return selection
      ? this.view.page_selected &&
          this.view.selection?.model === selection.model &&
          sameCodexEffort(this.view.selection.effort, selection.effort)
      : !this.view.page_selected;
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
        Promise.resolve().then(() => commands.codexCancelLogin(this.profile, cancelOwned)),
        READ_TIMEOUT,
      );
    }
  }
}
