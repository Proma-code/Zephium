# Windows subscription accounts

This experimental personal fork adds a direct ChatGPT connection and an optional
connection to an existing local model gateway. It is not an upstream release.
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

## Current limits

- Claude and OpenCode subscription chat are still in development; setup guidance
  alone is not a working connection.
- Numeric remaining subscription quota is not available through the current
  connected routes. Unknown quota is not shown as empty or full. Use the relevant
  provider's official account page to check usage.
- Portable tests have run, but native Windows installation and real-account
  behavior are not yet fully verified. See the [test status](../blueprint/verification.md).
- No real provider account was signed in or used for paid inference during these tests.

See the official [Sign in with ChatGPT documentation](https://developers.openai.com/siwc/token-sharing-open-source)
for current eligibility and service limits. A subscription is not unlimited access.
