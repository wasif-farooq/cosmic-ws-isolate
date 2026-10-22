# cosmic-ws-isolate

[![CI](https://github.com/wasif-farooq/cosmic-ws-isolate/actions/workflows/ci.yml/badge.svg?branch=master)](https://github.com/wasif-farooq/cosmic-ws-isolate/actions/workflows/ci.yml)

Makes each COSMIC workspace its own set of apps:

- **Dock** shows only windows on the current workspace (and monitor). Clicking an app that only
  has windows on another workspace opens a new instance here instead of jumping away.
- **Alt+Tab / Super+Tab** (and the Shift variants) open COSMIC's normal switcher, listing only
  the current workspace's windows: hold Alt, press Tab to move, release Alt to switch.
- Apps on another workspace that ask for focus mark that workspace **urgent** instead of yanking
  you over to it.

It's a small headless daemon plus three COSMIC settings. Every setting is backed up before it's changed,
and `restore` puts it back.

Built and verified against cosmic-comp `2f7c34f`, cosmic-applets `8981b0b`, cosmic-launcher `0adec45`
and pop-launcher `5b86851`.

## Install

```sh
make install          # binary -> ~/.local/bin, systemd user unit (not enabled yet)
make enable           # start the daemon now and at login
```

## Turn features on (one at a time is fine)

```sh
cosmic-ws-isolate apply --dock         # dock filter
cosmic-ws-isolate apply --activation   # no workspace jumping on focus requests
cosmic-ws-isolate apply --switcher     # per-workspace Alt+Tab switcher (run from ~/.local/bin)
cosmic-ws-isolate apply --all
```

All three take effect immediately, with no logout needed. Turn them off the same way with `restore`.
`restore` leaves a value alone if you changed it yourself after `apply`.

## Other commands

```sh
cosmic-ws-isolate watch    # read-only live view of workspaces/windows (sends nothing)
cosmic-ws-isolate status   # the daemon's view, as JSON
cosmic-ws-isolate switch next|prev   # focus next/previous window instantly, no popup
```

If you'd rather have instant switching with no popup, bind `cosmic-ws-isolate switch next` to a
custom shortcut in COSMIC Settings.

## How it works

| Feature | Mechanism |
|---|---|
| Dock | `com.system76.CosmicAppList/v1/filter_top_levels = Some(ConfiguredOutput)`, a built-in dock option |
| No jumping | `com.system76.CosmicComp/v1/activation_policy = FocusIfActiveWorkspace` |
| Alt+Tab | A pop-launcher plugin in `~/.local/share/pop-launcher/plugins/cosmic-ws-isolate/` named `"COSMIC Windows"`. pop-launcher loads user plugins first and skips later plugins with the same name, so this one replaces the stock window list that cosmic-launcher's Alt+Tab view shows. An empty query (the Alt+Tab view) lists the current workspace; a typed launcher search still finds windows on every workspace. |

The daemon follows windows and workspaces through the `ext-foreign-toplevel-list`, `zcosmic-toplevel-info`
and `ext-workspace` protocols and keeps a most-recently-used list. The plugin asks it for the window
list in the order cosmic-launcher expects, so the previous window is preselected as usual.

Safety:

- The daemon only sends `activate` (and `close`, when you close a window from the launcher), and
  only to a window the compositor still lists. cosmic-comp crashes on requests for windows it has
  never described.
- The plugin hands over to the stock window plugin if the daemon isn't running, so Alt+Tab keeps
  working (unfiltered).
- `apply`/`restore --switcher` only restart the `pop-launcher` helper process; cosmic-launcher starts
  a fresh one on the next search. No logout is needed.
- Alt+Tab's own key binding is left at COSMIC's default.

## Limitations

- Apps that can only raise their one existing window (rather than open a new one) won't get a second
  instance. With `--activation` you stay where you are and the other workspace is marked urgent.

## Emergency rollback (without the program)

```sh
rm ~/.config/cosmic/com.system76.CosmicComp/v1/activation_policy
rm -r ~/.local/share/pop-launcher/plugins/cosmic-ws-isolate && pkill -x pop-launcher
echo None > ~/.config/cosmic/com.system76.CosmicAppList/v1/filter_top_levels
systemctl --user disable --now cosmic-ws-isolate
```

This assumes `activation_policy` didn't exist before `apply`, which was the case on the machine this
was written on. The originals are kept in `~/.local/state/cosmic-ws-isolate/backup/`.

## Development

```sh
cargo test     # switching logic + config backup/restore round-trips
```

## License

GPL-3.0-only (see [LICENSE](LICENSE)), matching the `cosmic-client-toolkit` library it links against.
