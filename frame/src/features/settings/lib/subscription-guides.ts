import * as m from "$shared/i18n/messages";

/** Display-only setup documentation, never account state or runtime admission. */
export type SubscriptionGuide = Readonly<
  {
    id: "claude" | "antigravity" | "opencode";
    name: () => string;
    description: () => string;
    instructions: () => string;
    documentation: string;
  } & ({ support: "native_runner_required"; command: string } | { support: "unsupported" })
>;

export const subscriptionGuides: readonly SubscriptionGuide[] = [
  {
    id: "claude",
    name: m.ai_subscriptions_claude,
    description: m.ai_subscriptions_claude_description,
    instructions: m.ai_subscriptions_claude_steps,
    documentation: "https://code.claude.com/docs/en/setup",
    support: "native_runner_required",
    command: "claude auth login",
  },
  {
    id: "antigravity",
    name: m.ai_subscriptions_antigravity,
    description: m.ai_subscriptions_antigravity_description,
    instructions: m.ai_subscriptions_antigravity_steps,
    documentation: "https://antigravity.google/docs/faq",
    support: "unsupported",
  },
  {
    id: "opencode",
    name: m.ai_subscriptions_opencode,
    description: m.ai_subscriptions_opencode_description,
    instructions: m.ai_subscriptions_opencode_steps,
    documentation: "https://opencode.ai/docs/providers/",
    support: "native_runner_required",
    command: "opencode auth login",
  },
];
