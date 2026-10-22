# cosmic-ws-isolate

Makes each COSMIC workspace its own set of apps:

- **Dock** shows only windows on the current workspace (and monitor). Clicking an app that only
  has windows on another workspace opens a new instance here instead of jumping away.
- **Alt+Tab / Super+Tab** (and the Shift variants) cycle only the current workspace's windows.
- Apps on another workspace that ask for focus mark that workspace **urgent** instead of yanking
  you over to it.

It's a small headless daemon plus three COSMIC settings. Every setting is backed up before it's changed,
and `restore` puts it back.

Built and verified against cosmic-comp `2f7c34f`, cosmic-applets `8981b0b` and cosmic-launcher `0adec45`.

## Install

```sh
make install          # binary -> ~/.local/bin, systemd user unit (not enabled yet)
make enable           # start the daemon now and at login
```

## Turn features on (one at a time is fine)

```sh
cosmic-ws-isolate apply --dock         # dock filter
cosmic-ws-isolate apply --activation   # no workspace jumping on focus requests
cosmic-ws-isolate apply --switcher     # Alt+Tab -> this daemon (run from ~/.local/bin)
cosmic-ws-isolate apply --all
```

All three take effect immediately, with no logout needed. Turn them off the same way with `restore`.
`restore` leaves a value alone if you changed it yourself after `apply`.

## Other commands

```sh
cosmic-ws-isolate watch    # read-only live view of workspaces/windows (sends nothing)
cosmic-ws-isolate status   # the daemon's view, as JSON
```

## How it works

| Feature | Mechanism |
|---|---|
| Dock | `com.system76.CosmicAppList/v1/filter_top_levels = Some(ConfiguredOutput)`, a built-in dock option |
| No jumping | `com.system76.CosmicComp/v1/activation_policy = FocusIfActiveWorkspace` |
| Alt+Tab | `com.system76.CosmicSettings.Shortcuts/v1/system_actions`: only the `WindowSwitcher` and `WindowSwitcherPrevious` entries are set (COSMIC merges this file over its defaults). Your `custom` shortcuts are never touched. |

The daemon follows windows and workspaces through the `ext-foreign-toplevel-list`, `zcosmic-toplevel-info`
and `ext-workspace` protocols, and keeps a most-recently-used list. Each Alt+Tab press focuses the next
window on the current workspace. Presses less than 700 ms apart (`daemon --burst-ms`) walk the same
list, so repeated Tab presses go further back instead of toggling between two windows.

Safety:

- The daemon only ever sends `activate`, and only to a window the compositor still lists.
  cosmic-comp crashes on requests for windows it has never described.
- If the daemon isn't running, the Alt+Tab command hands over to the stock `cosmic-launcher` switcher.
- If COSMIC Settings rewrites `system_actions` (for example, when you change the default terminal), the
  daemon re-adds its two entries within 5 seconds.

## Limitations

- There's no visual switcher overlay. A headless program can't see Alt being released, so each press
  switches immediately.
- Apps that can only raise their one existing window (rather than open a new one) won't get a second
  instance. With `--activation` you stay where you are and the other workspace is marked urgent.

## Emergency rollback (without the program)

```sh
rm ~/.config/cosmic/com.system76.CosmicComp/v1/activation_policy
rm ~/.config/cosmic/com.system76.CosmicSettings.Shortcuts/v1/system_actions
echo None > ~/.config/cosmic/com.system76.CosmicAppList/v1/filter_top_levels
systemctl --user disable --now cosmic-ws-isolate
```

This assumes the two removed files didn't exist before `apply`, which was the case on the machine this
was written on. The originals are kept in `~/.local/state/cosmic-ws-isolate/backup/`.

## Development

```sh
cargo test     # switching logic + config backup/restore round-trips
```
