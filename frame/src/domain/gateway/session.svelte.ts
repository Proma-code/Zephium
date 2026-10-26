import { commands } from "$shared/ipc/gateway-bindings";
import type { GatewayStateV1, GatewayViewV1 } from "$shared/ipc/gateway-bindings";
import { events } from "$shared/ipc/native-events";
import { observe } from "$shared/lib/observe";
import { gatewayBaseValid, gatewayKeyValid, gatewayLabelValid } from "./validation";

const STATES: readonly GatewayStateV1[] = ["unconfigured", "configured", "checked", "unavailable"];
const READ_TIMEOUT = 5000;
const ACTION_TIMEOUT = 20000;
type Action = "save" | "check" | "disconnect";

/** Observes one profile; only native owns the saved gateway and its key. */
export class GatewaySession {
  view = $state.raw<GatewayViewV1 | null>(null);
  busy = $state<Action | null>(null);
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

  private adopt(view: GatewayViewV1) {
    if (
      view.version !== 1 ||
      view.profile !== this.profile ||
      !STATES.includes(view.state) ||
      !Number.isSafeInteger(view.models) ||
      view.models < 0 ||
      ((view.state === "configured" || view.state === "checked") &&
        (!view.connection_id ||
          !/^[a-f0-9]{32}$/u.test(view.connection_id) ||
          !view.base ||
          !gatewayBaseValid(view.base)))
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
      Promise.resolve().then(() => commands.gatewayStatus(this.profile)),
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

  private async act(action: Action, request: () => Promise<GatewayViewV1>) {
    if (
      !this.active ||
      this.busy ||
      this.observationFailed ||
      !this.view ||
      this.view.state === "unavailable" ||
      this.view.checking
    )
      return false;
    const sequence = ++this.sequence;
    const generation = this.generation;
    this.busy = action;
    this.observationFailed = false;
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
    }
    return adopted && this.view?.error === null;
  }

  save(expectedConnectionId: string | null, label: string, base: string, key: string) {
    if (!this.view || expectedConnectionId !== this.view.connection_id)
      return Promise.resolve(false);
    if (!gatewayLabelValid(label) || !gatewayBaseValid(base) || !gatewayKeyValid(key))
      return Promise.resolve(false);
    return this.act("save", () =>
      commands.gatewaySave(this.profile, expectedConnectionId, label, base, key),
    );
  }

  check() {
    const expectedConnectionId = this.view?.connection_id;
    if (!expectedConnectionId) return Promise.resolve(false);
    return this.act("check", () => commands.gatewayCheck(this.profile, expectedConnectionId));
  }

  disconnect() {
    const expectedConnectionId = this.view?.connection_id;
    if (!expectedConnectionId) return Promise.resolve(false);
    return this.act("disconnect", () =>
      commands.gatewayDisconnect(this.profile, expectedConnectionId),
    );
  }

  dispose() {
    this.active = false;
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
