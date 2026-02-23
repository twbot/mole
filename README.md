<p align="center">
  <img src="assets/mole.png" width="200" />
  <br><br>
  <strong>SSH tunnel manager CLI</strong>
  <br>
  Discovers tunnels from your <code>~/.ssh/config</code> and lets you start, stop, and monitor them with simple commands.
  <br><br>
  <a href="https://github.com/twbot/mole/releases/latest"><img src="https://img.shields.io/github/v/release/twbot/mole?style=flat-square" alt="Release"></a>
  <a href="LICENSE"><img src="https://img.shields.io/github/license/twbot/mole?style=flat-square" alt="License"></a>
  <a href="https://github.com/twbot/mole/actions/workflows/release.yml"><img src="https://img.shields.io/github/actions/workflow/status/twbot/mole/release.yml?style=flat-square&label=build" alt="Build"></a>
</p>

## Install

### Homebrew (macOS)

```bash
brew install twbot/tap/mole
```

### From source

Requires [autossh](https://www.harding.motd.ca/autossh/).

```bash
cargo install --path .
```

## Quick start

Mole reads your `~/.ssh/config` and finds Host blocks with `LocalForward`, `RemoteForward`, or `DynamicForward` — those are your tunnels. No separate config needed.

```
$ mole list
  ○ my-tunnel        inactive     16443:localhost:6443
  ○ reverse-tunnel   inactive     R:9090→localhost:3000

$ mole up my-tunnel
● my-tunnel started (pid 12345) — ✓ healthy

$ mole list
  ● my-tunnel        up 3m    ✓  16443:localhost:6443
  ○ reverse-tunnel   inactive     R:9090→localhost:3000

$ mole down my-tunnel
○ my-tunnel stopped
```

## Usage

```bash
mole list                 # show all tunnels with status, uptime, and health
mole up [name]            # start a tunnel (fuzzy picker if no name given)
mole down [name]          # stop a tunnel
mole restart [name]       # restart a tunnel
mole check                # health-check all active tunnels
mole logs [name]          # show tunnel logs (-f to follow)
mole add                  # interactive wizard to add a new tunnel
mole remove [name]        # remove a tunnel from SSH config
mole rename [old] <new>   # rename a tunnel
mole edit                 # open ~/.ssh/config in $EDITOR
mole config               # edit ~/.mole/config.toml
```

Bulk operations:

```bash
mole up --all             # start all inactive tunnels
mole down --all           # stop all active tunnels
mole up --group prod      # start all tunnels in group "prod"
mole down --group prod    # stop all tunnels in group "prod"
```

Persistence (macOS — via launchd):

```bash
mole enable [name]        # auto-start tunnel on login
mole disable [name]       # remove auto-start
mole up --persist [name]  # start + enable in one step
```

`mole ls` and `mole status` are aliases for `mole list`.

## Features

- **Fuzzy picker** — omit the tunnel name and get an interactive selector
- **Health check** — TCP-probes forwarded ports after starting to verify end-to-end connectivity
- **Port conflict detection** — refuses to start if a local port is already bound
- **Process adoption** — detects autossh tunnels started outside of mole and adopts them
- **Logging** — autossh stderr captured to `~/.mole/logs/`, viewable with `mole logs`
- **Groups** — tag tunnels with `# mole:group=<tag>` and operate on them together
- **LocalForward, RemoteForward, DynamicForward** — all three tunnel types supported

## Groups

Tag tunnels by adding a comment inside the Host block:

```
Host db-prod
  # mole:group=prod
  HostName 10.0.0.1
  User ubuntu
  LocalForward 5432 localhost:5432
```

Then `mole up --group prod` starts all tunnels in the group. The `--group`/`-g` flag works with `up`, `down`, `restart`, `list`, `enable`, and `disable`.

## SSH config example

```
Host my-tunnel
  HostName 10.0.0.1
  User me
  ProxyJump bastion
  LocalForward 16443 localhost:6443
  ExitOnForwardFailure yes

Host reverse-tunnel
  HostName bastion.example.com
  User me
  RemoteForward 9090 localhost:3000

Host socks-proxy
  HostName bastion.example.com
  User me
  DynamicForward 1080
```

## Configuration

Optional config at `~/.mole/config.toml` — run `mole config` to create/edit.

```toml
shell = "zsh"              # for completions
editor = "nvim"            # overrides $VISUAL/$EDITOR
health_timeout = 5         # seconds
max_log_size = 1048576     # bytes, before rotation
```

## Shell completions

Completions include subcommands, flags, tunnel names, and group names.

```bash
# zsh (add to ~/.zshrc)
source <(mole completions zsh)

# bash (add to ~/.bashrc)
source <(mole completions bash)

# fish (run once)
mole completions fish > ~/.config/fish/completions/mole.fish
```

## Platform notes

| Feature               | macOS         | Linux             |
| --------------------- | ------------- | ----------------- |
| `mole enable/disable` | launchd plist | not yet supported |
| `mole up --persist`   | launchd plist | not yet supported |

<details>
<summary>Linux persistence with systemd</summary>

```ini
# ~/.config/systemd/user/mole-my-tunnel.service
[Unit]
Description=mole tunnel: my-tunnel
After=network-online.target

[Service]
Type=simple
Environment=AUTOSSH_PORT=0
ExecStart=/usr/bin/autossh -N my-tunnel
Restart=always
RestartSec=5

[Install]
WantedBy=default.target
```

```bash
systemctl --user enable --now mole-my-tunnel.service
```

</details>
