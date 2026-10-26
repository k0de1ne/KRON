# Third-party code

## OpenAI Codex CLI — Apache License 2.0

https://github.com/openai/codex, commit 970111f6cb62cc02fe36a82ea7d71b37a2889c97

- `src/tools/apply_patch/parser.rs`, `streaming_parser.rs`, `seek_sequence.rs`,
  `text_file.rs`: copied from `codex-rs/apply-patch/src/` (imports adapted).
- `src/tools/apply_patch/mod.rs`: `compute_replacements` ported from
  `codex-rs/apply-patch/src/file_update.rs`.
- `src/prompts/codex.md`: `codex-rs/core/gpt_5_codex_prompt.md`, with the
  product name and tool hints changed.
- `src/prompts/apply_patch.lark`: `codex-rs/core/assets/tools/apply_patch.lark`.
- Tool names, parameters and descriptions in `src/tools/mod.rs` follow
  `codex-rs/core/src/tools/handlers/*_spec.rs`.

Copyright OpenAI. Licensed under the Apache License, Version 2.0
(http://www.apache.org/licenses/LICENSE-2.0). Distributed on an "AS IS" basis,
without warranties or conditions of any kind.

## Bundled in kron.apk

`android/proot.sh` puts these Termux builds into the APK unmodified, except
that the name `libtalloc.so.2` is rewritten to `libtalloc.so`:

- PRoot — GNU GPL v2, https://github.com/termux/proot (`libproot.so`, `libproot-loader.so`)
- talloc — GNU LGPL v3, https://talloc.samba.org (`libtalloc.so`)
- libandroid-shmem — BSD-3-Clause, https://github.com/termux/libandroid-shmem (`libandroid-shmem.so`)

Sources and build recipes: https://github.com/termux/termux-packages.
The Ubuntu rootfs is downloaded at run time from cdimage.ubuntu.com.

## References

Request details of the subscription adapters (endpoints, OAuth parameters,
client identity headers) were checked against:

- CLIProxyAPI (MIT), https://github.com/router-for-me/CLIProxyAPI
- Gemini CLI (Apache-2.0), https://github.com/google-gemini/gemini-cli
