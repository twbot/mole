<p align="center">
  <img src="assets/mole.png" width="200" />
</p>

# mole

SSH tunnel manager CLI. Discovers tunnels from your `~/.ssh/config` and lets you start, stop, and monitor them with simple commands.

## Install

### Homebrew (macOS)

```bash
brew install twbot/tap/mole
```

### From source

```bash
cargo install --path .
```

Requires [autossh](https://www.harding.motd.ca/autossh/):

```bash
# macOS
brew install autossh

# Debian/Ubuntu
sudo apt install autossh

# Fedora
sudo dnf install autossh

# Arch
sudo pacman -S autossh
```

## Usage

```bash
mole list                # show all tunnels with status, uptime, and health
mole up [name]           # start a tunnel (fuzzy picker if no name given)
mole down [name]         # stop a tunnel
mole restart [name]      # stop + start a tunnel
mole up --all            # start all inactive tunnels
mole down --all          # stop all active tunnels
mole restart --all       # restart all active tunnels
mole up --group prod     # start all inactive tunnels in group "prod"
mole down --group prod   # stop all active tunnels in group "prod"
mole restart --group prod # restart all tunnels in group "prod"
mole list --group prod   # list only tunnels in group "prod"
mole enable --group prod # enable auto-start for all tunnels in group "prod"
mole disable --group prod # disable auto-start for all tunnels in group "prod"
mole check               # health-check all active tunnels
mole logs [name]         # show tunnel logs (autossh stderr)
mole logs -f [name]      # follow log output
mole add                 # interactive wizard to add a new tunnel
mole remove [name]       # remove a tunnel from SSH config (stops + cleans up)
mole rename [old] <new>  # rename a tunnel (updates SSH config, logs, PID, launchd)
mole edit                # open ~/.ssh/config in $EDITOR
mole enable [name]       # auto-start tunnel on login (macOS only, via launchd)
mole disable [name]      # remove auto-start
mole up --persist [name] # start + enable auto-start in one step (macOS only)
mole config              # initialize or edit ~/.mole/config.toml
mole --version           # print version
```

`mole ls` and `mole status` are aliases for `mole list`.

## How it works

Mole parses your `~/.ssh/config` (including `Include` directives) and finds Host blocks with `LocalForward`, `RemoteForward`, or `DynamicForward` — those are your tunnels. No separate config file needed.

Example SSH config entries:

```
Host my-tunnel
  # mole:group=prod
  HostName 10.0.0.1
  User me
  ProxyJump bastion
  LocalForward 16443 localhost:6443
  RequestTTY no
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

Running `mole up my-tunnel` spawns `autossh -N my-tunnel` in the background and tracks the PID in `~/.mole/pids/`. Running `mole down my-tunnel` sends SIGTERM. Stale PID files are cleaned up automatically.

## Features

- **Fuzzy picker** — omit the tunnel name and get an interactive selector (powered by `dialoguer`, no fzf needed)
- **RemoteForward support** — discovers and manages reverse tunnels (`RemoteForward`) alongside local forwards
- **DynamicForward support** — discovers and manages SOCKS proxy tunnels (`DynamicForward` / `ssh -D`), with health checking on the local listen port
- **Health check** — after starting a tunnel, mole TCP-probes the forwarded port to verify it's working end-to-end (local forwards only; remote forwards show `—`)
- **Port conflict detection** — refuses to start if a local port is already bound
- **Uptime tracking** — `mole list` shows how long each tunnel has been running
- **Process adoption** — detects autossh tunnels started outside of mole and adopts them
- **Logging** — autossh stderr is captured to `~/.mole/logs/`, viewable with `mole logs`
- **Persistence** (macOS) — `mole enable` generates a launchd plist so tunnels auto-start on login
- **Groups** — tag tunnels with `# mole:group=<tag>` in your SSH config and operate on them together (`mole up --group prod`)

## Groups

Tag tunnels by adding a `# mole:group=<tag>` comment inside the Host block:

```
Host db-prod
  # mole:group=prod
  HostName 10.0.0.1
  User ubuntu
  LocalForward 5432 localhost:5432

Host api-prod
  # mole:group=prod
  HostName 10.0.0.2
  User ubuntu
  LocalForward 8080 localhost:80

Host db-staging
  # mole:group=staging
  HostName 10.1.0.1
  User ubuntu
  LocalForward 5433 localhost:5432
```

Then operate on all tunnels in a group at once:

```bash
mole up --group prod      # start all prod tunnels
mole down --group prod    # stop all prod tunnels
mole list --group staging # list only staging tunnels
```

The `--group`/`-g` flag works with `up`, `down`, `restart`, `list`, `enable`, and `disable`. The `mole add` wizard includes an optional Group tab to set the tag when creating a tunnel. Shell completions include group names.

`mole list` shows group tags as dimmed badges next to tunnel names.

## Platform notes

Most commands work on macOS and Linux. The exceptions:

| Feature               | macOS         | Linux             |
| --------------------- | ------------- | ----------------- |
| `mole enable/disable` | launchd plist | not yet supported |
| `mole up --persist`   | launchd plist | not yet supported |

On Linux, you can achieve the same persistence with a systemd user service:

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

## Configuration

Mole uses an optional config file at `~/.mole/config.toml`. Run `mole config` to create and edit it.

```toml
# Shell for completions (so `mole completions` works without an argument)
shell = "zsh"

# Editor for `mole edit` and `mole config` (overrides $VISUAL/$EDITOR)
editor = "nvim"

# Health check timeout in seconds (default: 5)
health_timeout = 5

# Max log file size in bytes before rotation (default: 1048576 = 1 MB)
max_log_size = 1048576
```

All fields are optional — if the file doesn't exist or a field is missing, defaults are used.

## Shell completions

Completions include subcommands, flags, and tunnel names from your SSH config. One command to set up:

**zsh** (add to `~/.zshrc`):

```bash
source <(mole completions zsh)
```

**bash** (add to `~/.bashrc`):

```bash
source <(mole completions bash)
```

**fish** (run once):

```fish
mole completions fish > ~/.config/fish/completions/mole.fish
```

If you set `shell` in your config, `mole completions` works without the shell argument.

Then `mole up <TAB>` will complete tunnel names, `mole <TAB>` will complete subcommands, and `--<TAB>` will complete flags.

## Example

```
$ mole list
  ○ my-tunnel        inactive     16443:localhost:6443
  ○ reverse-tunnel   inactive     R:9090→localhost:3000

$ mole up my-tunnel
● my-tunnel started (pid 12345) — ✓ healthy

$ mole list
  ● my-tunnel        up 3m    ✓  16443:localhost:6443
  ○ reverse-tunnel   inactive     R:9090→localhost:3000

$ mole check
  ● my-tunnel                ✓ :16443

  ✓ All 1 port(s) healthy across 1 tunnel(s)

$ mole down my-tunnel
○ my-tunnel stopped
```

Remote forwards display with an `R:` prefix. Dynamic (SOCKS) forwards display with a `D:` prefix. Health checks apply to local and dynamic forwards — remote forwards show `—` since the bound port lives on the remote server.
