# hypr-workspace-history

Hyprland plugin for per-monitor MRU workspace history cycling.

It records workspace transitions with Hyprland workspace hooks, freezes the
history while cycling, and commits the previewed workspace when the Super/Meta
modifier is actually released via Hyprland's keyboard event bus.

## Build

```sh
nix build
```

or:

```sh
cmake -S . -B build
cmake --build build
```

## Lua Config

Load the plugin:

```lua
hl.plugin.load("/path/to/libhypr-workspace-history.so")
```

Use the Lua functions from keybindings:

```lua
bind("SUPER + backslash", function()
  hl.plugin.workspacehistory.cycle(1)
end)

bind("SUPER + slash", function()
  hl.plugin.workspacehistory.cycle(-1)
end)

bind("SUPER + Escape", function()
  hl.plugin.workspacehistory.cancel()
end)
```

No release bind is needed. The plugin listens for Super/Meta release directly.

## Debug State

The plugin writes a versioned JSON snapshot for status widgets and a compact
text snapshot for debugging:

```text
$XDG_RUNTIME_DIR/hyprland-workspace-history.json
$XDG_RUNTIME_DIR/hyprland-workspace-history-state
$XDG_RUNTIME_DIR/hyprland-workspace-history.log
```

The JSON file is written atomically and is intended to be watched with inotify:

```json
{
  "version": 1,
  "revision": 42,
  "active_monitor": "DP-1",
  "active_workspace": 3,
  "monitors": {
    "DP-1": { "history": [3, 1, 8] }
  },
  "cycle": null
}
```
