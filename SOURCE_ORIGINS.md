# Source origins

This repository began as a clean source snapshot rather than a copy of the
`rustdesk4ohos` Git history.

Initial inputs:

- `crates/rd-engine`: enhanced runtime snapshot from Core commit
  `01e176672` on `refactor/protocol-compatible-runtime`.
- `libs/hbb_common`: RustDesk common/wire source at submodule commit `cd80a99`.
- `native/ohos_har`: active modern OHRS bridge and package assets from
  RustDesk-Har commit `69775e9`; the uncompiled historical `src/lib.rs` was not
  imported.

The original RustDesk project is licensed under the GNU Affero General Public
License version 3. RustDesk project and protocol copyrights remain with their
respective authors. New repository structure does not erase upstream
attribution or license obligations.
