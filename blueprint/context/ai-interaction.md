# Interaction and ownership

Work autonomously on the already authorized Windows feature while the user is
away; do not infer new scope, account consent or external-action authority.

The active packet is a retrofit of existing implementation. User feature scope
is authorized; this new document has not independently been approved as a separate
proposal. Keep future behavior within that scope and surface material choices.

Multiple workers share one feature: implementation lead owns app code and Git;
skills curator owns `.agents/skills`, `blueprint/` and `docs/skills-*`; other
workers use explicit path ownership. Never commit all dirty files or create a
shared stash to reorganize another worker's work.

No PR, merge, auto-merge, release or deployment. The user's public own-fork Windows
branch can be pushed only by the designated lead. GitHub write access was restored
by the user; the remote Windows branch exists, but this document does not attest
a future commit has been pushed. Verify remote SHA before claiming publication.

No real account login/logout, OAuth consent or paid inference in tests. Keep
private bookmark exports, Library identifiers and user-specific data out of Git.
Only selected public source repository URLs and license notices belong here.

Summaries should distinguish implemented, portable-tested, native-unverified and
blocked. Write exact evidence rather than optimistic completion language.
