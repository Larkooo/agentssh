# agentssh

`agentssh` is an SSH-first, tabbed interface for running and managing coding agents (`codex`, `claude`, etc.) on your own VPS.

The design is inspired by the `terminal.shop` terminal experience:
- top tab bar
- dashboard + per-instance tabs
- keyboard-first controls
- boxed terminal panels

## Architecture

`agentssh` is agent-first, but keeps runtime complexity low by using `tmux` for PTY/session durability.

- `sshd` handles remote access
- `agentssh` handles agent discovery, tabs, summaries, and controls
- `tmux` handles durable sessions and attach/detach behavior

## What it does

- Auto-detects installed agent CLIs in `PATH` (currently: `codex`, `claude`, `aider`, `gemini`, `opencode`)
- Detects running agent sessions from tmux
- Creates new agent instances from inside the list view (`[+ New Instance]`)
- Uses a two-step wizard for creation: choose agent, then choose working directory (preset or custom)
- Shows an agent dashboard list + summary panel
- Shows each running instance as its own top tab
- Attaches into an instance (`enter`)
- Stops an instance (`x`)

## Quick start

1. Build:

```bash
cargo build --release
```

2. Run:

```bash
./target/release/agentssh
```

3. Inside the app:

- go to the dashboard tab
- select `[+ New Instance]` in the list and press `enter`
- choose agent, then choose working directory, then press `enter` to create
- select an instance and press `enter` to jump in
- detach from tmux normally (`Ctrl-b d`) and return to the manager

## SSH ForceCommand setup

Use a dedicated user so SSH lands directly in the manager UI.

Example (`/etc/ssh/sshd_config.d/agentssh.conf`):

```text
Match User agentops
    ForceCommand /usr/local/bin/agentssh
    PermitTTY yes
    X11Forwarding no
    AllowTcpForwarding no
```

Reload SSH:

```bash
sudo systemctl reload sshd
```

Then connect:

```bash
ssh agentops@your-vps
```

## Controls

- `up/down` (or `j/k`): move selection in dashboard list
- `enter` on `[+ New Instance]`: start creation wizard
- `enter` on an instance: attach to selected/current instance
- `left/right` (or `h/l`, `tab`): switch tabs
- `x`: stop selected/current instance
- `d`: go to dashboard tab
- `r`: refresh
- `q`: quit
