# agentssh

`agentssh` is an open-source terminal interface for managing agent sessions (`codex`, `claude`, etc.) over SSH.

It is intentionally simple:
- `sshd` handles SSH.
- `agentssh` renders an interactive TUI list and summary view.
- `tmux` is the session backend.

When a user runs `ssh`, `agentssh` can be the forced command, giving a `terminal.shop`-style experience for agent sessions.

## Why tmux

Using tmux keeps implementation complexity low while providing:
- durable sessions
- safe detach/reattach
- PTY handling
- session lifecycle commands users already know

## Features

- List tmux sessions with state and command summary
- Preview recent output from each session
- Attach directly into selected session
- Refresh sessions manually or by interval
- Optional name filter

## Quick start

1. Build:

```bash
cargo build --release
```

2. Create agent sessions in tmux:

```bash
tmux new-session -d -s codex "codex"
tmux new-session -d -s claude "claude"
```

3. Run locally:

```bash
./target/release/agentssh
```

## Use as SSH interface

Configure a dedicated user with `ForceCommand`.

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

Now `ssh agentops@your-vps` opens the agent manager UI.

## Controls

- `j` / `k` or arrow keys: move selection
- `enter`: attach to selected session
- `r`: refresh now
- `q`: quit

## CLI flags

- `--filter <text>`: show only matching session names
- `--refresh-seconds <n>`: auto-refresh interval (default: `5`)

