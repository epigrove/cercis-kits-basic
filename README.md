# Basic kit (cercis-kits-basic)

**English** | [中文](README.zh-CN.md)

Everyday functionality for everyone. **It is not a program — it is a set of independent plugins**:
each has its own `plugin.yaml`, its own permissions and its own UI extension points, and can be
enabled, authorised or upgraded on its own.

> This repository only holds features **every domain needs**; research-specific features live in
> `cercis-kits-research`.

```
basic/
├── crates/
│   ├── cercis-kit-core/        shared library: kernel bridge, row checks, reference checks
│   ├── plugin-project-wizard/  plugin 1 · project wizard
│   ├── plugin-data-entry/      plugin 2 · data entry
│   ├── plugin-transfer/        plugin 3 · import & export
│   └── plugin-health/          plugin 4 · health check
└── plugins/                    plugin manifests shipped with the kit (one directory each)
    ├── project_wizard/plugin.yaml
    ├── data_entry/plugin.yaml
    ├── import_export/plugin.yaml
    └── health_check/plugin.yaml
```

## The first four plugins

| Plugin | Capability | Permissions | Entry point |
|--------|-----------|-------------|-------------|
| `project_wizard` Project wizard | create a project, install a template, scan an existing folder (read-only) | read + write | `cercis-plugin-wizard` |
| `data_entry` Data entry | batch paste, single-row entry, attach files | read + write | `cercis-plugin-entry` |
| `import_export` Import & export | CSV/TSV import (mapping / dry run / report), long and wide export, data notes | read + write | `cercis-plugin-transfer` |
| `health_check` Health check | directory skeleton, broken links, encoding, name clashes, duplicate keys, orphan attachments | **read-only** | `cercis-plugin-health` |

> Later batches (pack & merge, backup/restore, plugin manager, a generic query UI) join the same
> directory as milestones land.

## Shared rules

1. **Writes only go through the kernel**: plugins work through the `cercis` command with `--json`
   and therefore cannot bypass atomic writes, per-change snapshots or the change log.
2. **Read-only by default**: writing requires declaring `write` in the manifest, and the app
   injects `--allow-write` only when the user has authorised it; without it a plugin refuses
   outright instead of writing quietly.
3. **Check before writing**: column names, required fields, types, duplicate keys and
   **referential integrity** are validated before anything is written; a problem means the whole
   batch is not written — no half-finished imports.
4. **Core capability is platform-neutral**: UI adaptation is declared through extension points in
   the app, so one implementation serves desktop, tablet and phone.

## Build and test

```bash
cd cercis-kits-basic       # in the development workspace: kits/basic
cargo build          # produces the four plugin binaries
cargo test           # 34 tests: unit + end-to-end (cross-process, against the real kernel)
                     # including "every command declared in a manifest is a real subcommand"
```

The end-to-end tests really call kernel commands, so the kernel binary must exist:

```bash
cd ../kernel && cargo build     # in the development workspace
```

Install them into your user plugin directory (that is where the app and the CLI look for plugins;
this records the install in the ledger and marks them as "bundled" so they cannot be deleted):

```bash
bash ../apps/desktop/packaging/install-basic-kits.sh   # needs the apps repository next to it
```

If a plugin binary is older than its source (`cargo test` does not rebuild the plugin binaries in
`target/debug/`), the tests **skip it and say so** rather than passing on a stale binary.

## Try it by hand

```bash
# 1. create a project (project wizard)
cercis-plugin-wizard --project /tmp/demo --name demo --template example-study --allow-write

# 2. record a row (data entry)
cercis-plugin-entry append --project /tmp/demo --table studies \
  --set study_id=ST-001 --set name="demo study" --allow-write

# 3. import CSV (import & export)
cercis-plugin-transfer import --project /tmp/demo --table studies \
  --file more-studies.csv --report /tmp/demo/import-report.md --allow-write

# 4. export + data notes
cercis-plugin-transfer export --project /tmp/demo --table studies \
  --out /tmp/demo/exports --readme --allow-write

# 5. health check (read-only plugin)
cercis-plugin-health --project /tmp/demo
```

Add `--json` to any command for machine-readable output — that is the channel the app and AI use.
