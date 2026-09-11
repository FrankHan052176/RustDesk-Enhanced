# `rustdesk-ohrs`

HarmonyOS native package produced directly by `RustDesk-Enhanced`.

The package contains the N-API bridge and native enhanced runtime. Its stable
consumer identity is:

- package: `rustdesk-ohrs`
- entry: `index.ets`
- native library: `librustdesk_native_har.so`

The existing ArkTS compatibility facade is under active migration. Package
declarations must not be treated as implemented unless the corresponding native
operation is present and passes the repository contract check.

Build from the repository root:

```bash
bash scripts/ohos/build-har.sh
```

The canonical artifact is `dist/ohos-har/package.har`.
