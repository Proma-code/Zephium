import type { ProfileRemovalViewV1 } from "$shared/ipc/bindings";

/** Presentation only: neither a retry permit nor a deletion-complete receipt. */
export function removalPresentation(view: ProfileRemovalViewV1) {
  const fact =
    view.cleanup === "verified"
      ? "managed_data_removed"
      : view.native === "signed_out_settled"
        ? "signed_out_cleanup_unconfirmed"
        : view.native === "not_configured"
          ? "not_configured"
          : "unconfirmed";
  return { phase: view.phase, fact, blocked: view.blocked } as const;
}
