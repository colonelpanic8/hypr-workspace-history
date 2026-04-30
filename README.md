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

The plugin writes a compact snapshot for status widgets/debugging:

```text
$XDG_RUNTIME_DIR/hyprland-workspace-history-state
$XDG_RUNTIME_DIR/hyprland-workspace-history.log
```
