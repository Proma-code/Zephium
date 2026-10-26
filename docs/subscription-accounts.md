# Windows subscription accounts

This experimental personal fork adds direct ChatGPT and OpenCode Go connections and an
optional connection to an existing local model gateway. It is not an upstream release.
Use only the separate [Windows trial](windows-subscription-trial.md); keep your
ordinary Zephium installation and data.

## ChatGPT

1. Open Settings → AI and choose **Sign in** in the ChatGPT card.
2. Review the prompt and continue in your system browser. You can cancel.
3. In the model picker, choose OpenAI → ChatGPT, then select a model.

Your existing OpenAI API key remains a separate option. Signing in does not
change your chosen model. Model names stay unchanged; the surrounding source
selector identifies the connection. Supported models offer the existing effort
control. Disconnecting one connection does not remove the other.

The account provides its model list. A listed model is not a guarantee that a
request will be accepted. The current adapter supports text and function tools,
not image input or provider-native search. Zephium does not silently switch from
this connection to an API key when a request fails or a usage limit is reached.

## Existing local gateway

Under Custom, choose **Local gateway**. In Settings, enter its label, local address
and its own access key, then save. Supported addresses have the form
`http://127.0.0.1:PORT/v1` or `http://[::1]:PORT/v1`.

Choose **Check models** when you want to read its catalog. Saving, listing models
and successfully sending a request are separate steps. Zephium does not install
or start a gateway for you. Use only a gateway you already trust and control.

This option is not Google account sign-in. Google does not support third-party
consumer Antigravity wrapping; check the [official FAQ](https://antigravity.google/docs/faq).
A compatible gateway's availability does not establish any upstream entitlement.

## OpenCode Go

The branch also includes an OpenCode Go connection using the subscription access
key issued by the [OpenCode Console](https://opencode.ai/console), not an OpenCode
CLI login. In Settings → AI, review the coding-agent-use and billing disclosure,
enter that key, acknowledge the controls, and save. Choose **Check models** to
read its public catalog, then explicitly select OpenCode Go in the model picker.
A public catalog does not verify the key or a paid plan.

OpenCode Go is intended for coding-agent work. Its Console **Extra Usage** setting
can charge your balance after included usage runs out. Zephium does not enable
that setting or choose another billing route, but cannot override the provider's
setting. Review it before sending requests. The latest completed request's
provider-reported billing source is shown separately from quota.

Only the currently implemented model/protocol subset is offered. Other listed
models are not silently routed through a different API. Remaining numeric quota
is unavailable here; check the provider Console. See the [official Go guide](https://opencode.ai/v2/docs/console/go/)
for current limits, model availability and privacy settings.

## Current limits

- Claude account chat remains in development. OpenCode CLI setup guidance is
  separate from the Go access-key connection and does not establish a CLI login.
- Numeric remaining subscription quota is not available through the current
  connected routes. Unknown quota is not shown as empty or full. Use the relevant
  provider's official account page to check usage.
- Portable tests have run, but native Windows installation and real-account
  behavior are not yet fully verified. See the [test status](../blueprint/verification.md).
- No real provider account was signed in or used for paid inference during these tests.

See the official [Sign in with ChatGPT documentation](https://developers.openai.com/siwc/token-sharing-open-source)
for current eligibility and service limits. A subscription is not unlimited access.
