import { commands } from "$shared/ipc/go-bindings";
import type { GoStateV1, GoViewV1 } from "$shared/ipc/go-bindings";
import { events } from "$shared/ipc/native-events";
import { observe } from "$shared/lib/observe";
import { goKeyValid } from "./validation";
import { quotaAt, quotaProjection } from "./quota";

const STATES: readonly GoStateV1[] = ["unconfigured", "configured", "catalogued", "unavailable"];
const READ_TIMEOUT = 5000;
const ACTION_TIMEOUT = 20000;
type Action = "save" | "check" | "disconnect" | "quota";

/** Observes one profile; only native owns the saved OpenCode Go connection and its key. */
export class GoSession {
  view = $state.raw<GoViewV1 | null>(null);
  busy = $state<Action | null>(null);
  checking = $state(false);
  private quotaNow = $state(Date.now());
  readonly quota = $derived.by(() => {
    const quota = quotaAt(quotaProjection(this.view?.quota), this.quotaNow);
    return this.observationFailed && quota.state === "current"
      ? { ...quota, state: "stale" as const }
      : quota;
  });
  private quotaTimer: ReturnType<typeof setTimeout> | null = null;
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

  private adopt(view: GoViewV1) {
    if (
      view.version !== 1 ||
      view.profile !== this.profile ||
      !STATES.includes(view.state) ||
      !Number.isSafeInteger(view.models) ||
      view.models < 0 ||
      ((view.state === "configured" || view.state === "catalogued") &&
        (!view.connection_id || !/^[a-f0-9]{32}$/u.test(view.connection_id)))
    ) {
      this.observationFailed = true;
      return false;
    }
    this.view = { ...view, quota: quotaProjection(view.quota) };
    this.ageQuota();
    this.observationFailed = false;
    return true;
  }

  private ageQuota() {
    if (this.quotaTimer !== null) clearTimeout(this.quotaTimer);
    this.quotaTimer = null;
    this.quotaNow = Date.now();
    const quota = this.view?.quota;
    if (!this.active || !quota || quota.state !== "current") return;
    if (!Number.isFinite(this.quotaNow) || this.quotaNow < Number(quota.checked_at_ms)) return;
    const expires = Math.min(
      Number(quota.stale_at_ms),
      ...quota.windows.map((window) => Number(window.resets_at_ms)),
    );
    if (expires <= this.quotaNow) return;
    // Local clock observation only; no provider calls, including after a reset.
    this.quotaTimer = setTimeout(() => this.ageQuota(), Math.min(30000, expires - this.quotaNow));
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
      Promise.resolve().then(() => commands.goStatus(this.profile)),
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

  refresh() {
    return this.read();
  }

  private async act(action: Action, request: () => Promise<GoViewV1>) {
    if (
      !this.active ||
      this.busy ||
      this.observationFailed ||
      !this.view ||
      this.view.state === "unavailable" ||
      this.view.checking ||
      this.view.quota.checking
    )
      return false;
    const sequence = ++this.sequence;
    const generation = this.generation;
    const quotaSource = action === "quota" ? this.view.connection_id : null;
    this.busy = action;
    this.observationFailed = false;
    let result = await observe(
      Promise.resolve().then(request),
      ACTION_TIMEOUT,
      this.lifetime.signal,
    );
    if (generation !== this.generation || sequence !== this.sequence) return false;
    const quotaChanged =
      action === "quota" &&
      (this.readAgain ||
        this.view?.connection_id !== quotaSource ||
        (result.state === "received" && result.value.connection_id !== quotaSource));
    if (quotaChanged) {
      // Reservation/progress and source changes share an event. Confirm local state
      // while keeping the action pending; never project a mismatched request's quota.
      this.readAgain = false;
      result = await observe(
        Promise.resolve().then(() => commands.goStatus(this.profile)),
        READ_TIMEOUT,
        this.lifetime.signal,
      );
      if (generation !== this.generation || sequence !== this.sequence) return false;
    }
    this.busy = null;
    const adopted = result.state === "received" && this.adopt(result.value);
    if (!adopted) this.observationFailed = true;
    if (this.readAgain) {
      this.readAgain = false;
      void this.read();
    }
    return (
      adopted &&
      (action === "quota"
        ? this.view?.connection_id === quotaSource && this.view.quota.error === null
        : this.view?.error === null)
    );
  }

  save(expectedConnectionId: string | null, key: string, billingAcknowledged: boolean) {
    if (!this.view || expectedConnectionId !== this.view.connection_id)
      return Promise.resolve(false);
    if (!billingAcknowledged || !goKeyValid(key)) return Promise.resolve(false);
    return this.act("save", () =>
      commands.goSave(this.profile, expectedConnectionId, key, billingAcknowledged),
    );
  }

  check() {
    const expectedConnectionId = this.view?.connection_id;
    if (!expectedConnectionId) return Promise.resolve(false);
    return this.act("check", () => commands.goCheck(this.profile, expectedConnectionId));
  }

  refreshQuota() {
    const expectedConnectionId = this.view?.connection_id;
    if (!expectedConnectionId) return Promise.resolve(false);
    return this.act("quota", () => commands.goQuota(this.profile, expectedConnectionId));
  }

  disconnect() {
    const expectedConnectionId = this.view?.connection_id;
    if (!expectedConnectionId) return Promise.resolve(false);
    return this.act("disconnect", () => commands.goDisconnect(this.profile, expectedConnectionId));
  }

  dispose() {
    this.active = false;
    if (this.quotaTimer !== null) clearTimeout(this.quotaTimer);
    this.quotaTimer = null;
    this.generation++;
    this.sequence++;
    this.lifetime.abort();
    this.stop?.();
    this.stop = null;
    this.view = null;
    this.busy = null;
    this.checking = false;
    this.readFlight = null;
    this.readAgain = false;
    this.observationFailed = false;
    // Observation disposal cannot roll back a save/check/disconnect already admitted by native.
  }
}
