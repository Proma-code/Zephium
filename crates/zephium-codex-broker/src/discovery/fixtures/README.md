# Published npm metadata fixtures

The two unmodified package metadata files are from official `@openai/codex`
0.164.0 and its 0.164.0-win32-x64 optional package (Apache-2.0).

- Root: https://registry.npmjs.org/@openai/codex/-/codex-0.164.0.tgz
- Windows: https://registry.npmjs.org/@openai/codex/-/codex-0.164.0-win32-x64.tgz

Both archive SHA-512 integrity values were checked against their exact registry
version records before reading members. The Windows archive's
`package/vendor/x86_64-pc-windows-msvc/bin/codex.exe` is 354,558,768 bytes with
SHA-256 `6f267830b029ee25d9339864aa867cd45de9f8fd4c5ea007d2e63c13a0c66d36`,
identical to the qualified original GitHub release asset. No package was installed
and no archive code was executed. The regression checks real metadata/layout;
it does not replace complete held-image verification at runtime or claim all
future npm packaging is identical.

Archive SHA-256:
- root: `afb117ec0d382744583dd13154ba0ca6b0f1980f8ed6a23bffb3aaadb6226e30`
- Windows: `4fbd23473a2fc223396cdd8490d3f658d368438134c12a3f1dac9d1e2041a65b`
