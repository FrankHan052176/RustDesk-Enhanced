# Source origins

This branch uses `rustdesk4ohos` commit `f42906f55` as its base. That commit
contains the full RustDesk application and its upstream history, so the
existing Flutter clients continue to build from the original `src/` and
`flutter/` trees.

The enhanced runtime was imported from `RustDesk-Enhanced` commit `5627058` as
an additive source-to-artifact chain:

- `crates/rd-engine`: enhanced protocol and media runtime, originally split
  from Core commit `01e176672` on `refactor/protocol-compatible-runtime`;
- `libs/hbb_common`: the `rustdesk4ohos` submodule at `2b54c6a`, shared by the
  upstream application and the enhanced runtime;
- `native/ohos_har`: active OHRS bridge and package assets descended from
  RustDesk-Har commit `69775e9`;
- `scripts/ohos`: reproducible HAR build tooling.

The original RustDesk project is licensed under the GNU Affero General Public
License version 3. RustDesk project and protocol copyrights remain with their
respective authors. The integration retains the upstream Git history and does
not alter those attribution or license obligations.
