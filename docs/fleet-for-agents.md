# Setting up a fleet for someone (agents)

These are the exact commands an agent runs to put a person's machines into one Local Studio fleet, share their GPUs within a power budget, and check the result. Every command talks to the Local controller on the same machine, with that machine's fleet key, so run them on each machine you set up.

## 1. Install

Install Local Studio on each machine with the install script in the README (`scripts/install.sh --channel <channel>`). Open it once: it starts the Local controller on `127.0.0.1:18091`. The commands below use the `local-studio` command; in a checkout, build it with `cargo build -p local-studio-core --bin local-studio`.

Every machine in a fleet needs the same fleet key and must be on the same tailnet. The key is `fleetKey` in Local Studio's Settings, Local (or the controller's `config.json`).

## 2. Check that the controller answers

```
local-studio fleet status --json
```

`{"state":"error","message":"No Local controller answers at ..."}` means the controller is not running: open Local Studio and run it again. A message about the fleet key means the key in Settings, Local does not match the controller's.

## 3. Join the fleet

```
local-studio fleet join
```

This turns on discovery. Within about 15 seconds the controller finds the other controllers on the tailnet that share the fleet key and links them. They show up as `peers` in `fleet status`.

## 4. Share this machine's GPU

```
local-studio fleet share --active 60 --away max
```

- `--active <watts|off>`: how many watts fleet work may draw while the person is using the machine. `off` means it does not lend while they are present.
- `--away <watts|max>`: the cap while they are away. `max` means no cap.

Both take whole watts up to 2000 (`--active` from 0, `--away` from 1) and either may be left out to keep its value. `share` sets lending to `idle`: the machine lends only while no other program uses its GPU, it is on AC power, and (on the media host) parish-media says the GPU is free. Any of those taking the GPU back reclaims it at once.

Ask the person for the two numbers. If they don't care, `--active off --away max` lends only while they are away, with no cap.

On a Linux machine with an NVIDIA GPU, a real power cap needs one root rule:

```
local-studio fleet power-helper          # prints the sudoers rule and the commands, changes nothing
local-studio fleet power-helper --yes    # checks it with visudo, installs it with sudo
```

It allows only `nvidia-smi -i <GPU index> -pl <watts>` for the user who ran it. `sudo` asks for that person's password, so they run the `--yes` step themselves. It refuses on machines without an NVIDIA GPU, which need no helper, and when the `nvidia-smi` on `PATH` sits somewhere a non-root user can change (a conda env, `~/.local/bin`): put the driver's `/usr/bin` first in `PATH` and run it again. The controller notices the new rule within a minute; there is no need to run `share` again.

### Presence on Linux desktops that don't report idle

`power.presenceSource` saying "the desktop does not report idle time" means the desktop never sets logind's idle hint (Hyprland and many window managers), so the controller can't tell whether someone is at the screen and applies the away cap. On Hyprland, have hypridle set the hint (add to `~/.config/hypr/hypridle.conf`, then restart hypridle; untested live):

```
listener {
    timeout = 60
    on-timeout = busctl call org.freedesktop.login1 /org/freedesktop/login1/session/auto org.freedesktop.login1.Session SetIdleHint b true
    on-resume = busctl call org.freedesktop.login1 /org/freedesktop/login1/session/auto org.freedesktop.login1.Session SetIdleHint b false
}
```

`fleet status` should then show `owner: present` or `away` with a `logind session` source.

To stop sharing:

```
local-studio fleet stop-sharing
```

## 5. Check

```
local-studio fleet status --json
```

| Field              | Meaning                                                                                       |
| ------------------ | --------------------------------------------------------------------------------------------- |
| `state`            | `lent` (the fleet may use the GPU), `reclaimed` (a check failed; see `reason`) or `off`       |
| `lend`             | `never`, `idle` or `always`                                                                    |
| `discover`         | Whether this machine looks for other machines                                                 |
| `reason`           | Plain words for why it is or isn't lending, such as "In use, so it lends at up to 60 W."      |
| `checks`           | Each lending check with `ok` and a detail                                                     |
| `power.activeWatts`, `power.awayWatts` | The caps set by `share` (`activeWatts` 0 = no lending while present, `awayWatts` `null` = no cap) |
| `power.present`    | Whether someone is using the machine now (`null` when it can't tell or lending is off)        |
| `power.presenceSource` | How that was read (macOS input idle time, a logind session, or why it can't tell)         |
| `power.capW`       | The cap in force now, or `null`                                                               |
| `power.lever`      | `nvidia-limit` (real NVIDIA power limit), `duty-cycle` (fleet work paused in short bursts) or `none` |
| `power.measured`, `power.measuredW` | Whether fleet draw is measured, and the draw now in watts                    |
| `power.helper`     | `installed`, `missing` (run `fleet power-helper`) or `unsupported` (no NVIDIA, or not Linux)  |
| `peers`            | Each linked machine with its `state`, `lend` and `reason`                                     |

The setup is done when every machine shows the peers you expect, and each sharing machine shows `lend: "idle"` with `state` `lent`, or `reclaimed` with a `reason` the person agrees with (on battery, a GPU program running, and so on).

Without `--json`, each command prints the same facts as text: the state and reason on the first line, then sharing, the caps, the owner, the cap now and its lever, fleet draw, the helper, each check and each peer.
