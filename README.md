# hypr-workspace-history

Per-monitor MRU workspace history for Hyprland, modeled after the old XMonad
`CycleWorkspaceByScreen` setup.

The daemon watches Hyprland's event socket, records the active workspace for
each monitor, and exposes small commands for keybindings:

- `cycle next` previews the next less-recent workspace for the active monitor.
- `cycle prev` previews in the opposite direction.
- `commit` ends the preview session and updates history once.
- `cancel` returns to the workspace where the preview started.

During a cycle session the daemon freezes the original history. That keeps
previewed workspaces from becoming "recent" until the modifier key is released,
matching the important XMonad behavior.

## Build

```sh
cargo build --release
```

## Hyprland Bindings

Start the daemon once in your Hyprland session:

```hyprlang
exec-once = hypr-workspace-history daemon
```

Use the command client from bindings:

```hyprlang
bind = SUPER, backslash, exec, hypr-workspace-history cycle next
bind = SUPER, slash, exec, hypr-workspace-history cycle prev
bindr = SUPER, Super_L, exec, hypr-workspace-history commit
bind = SUPER SHIFT, backslash, exec, hypr-workspace-history cancel
```

`bindr` is the key input trick: Hyprland handles the physical key release, and
the daemon handles the frozen MRU session. If your Super key is not `Super_L`,
bind the release command to the physical modifier key you actually hold.

You can print the same snippet with:

```sh
hypr-workspace-history bindings
```

## State

History is persisted at:

```text
$XDG_STATE_HOME/hypr-workspace-history/state.json
```

or `~/.local/state/hypr-workspace-history/state.json` when `XDG_STATE_HOME` is
unset.
