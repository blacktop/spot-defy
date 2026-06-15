# tmux now-playing status bar

spot-defy ships a `now-playing` subcommand that prints a single line describing
what the running app is playing. It is meant for the tmux status bar.

```
▶ Daft Punk — Aerodynamic
```

## How it works

`spot-defy now-playing` connects to the running app's Unix socket, reads one
cached snapshot, and prints a single line, then exits. It **never** starts
librespot, opens the browser, or launches the TUI, so it is cheap enough to
poll every few seconds.

- The full TUI (run `spot-defy` with no subcommand) must already be running; it
  is what serves the socket.
- When nothing is playing, the app is not running, or auth is missing, the
  command prints an empty line and exits `0`. The status bar then shows nothing
  instead of an error or a hang.
- There is a hard 400 ms timeout on the whole query, so the status bar can
  never block waiting for a slow or dead socket.

## State symbols

| State    | Symbol | Example                       |
| -------- | ------ | ----------------------------- |
| Playing  | `▶`    | `▶ Daft Punk — Aerodynamic`   |
| Paused   | `⏸`    | `⏸ Air — La Femme d'Argent`   |
| Loading  | `…`    | `… Boards of Canada — Roygbiv`|
| Stopped  | —      | *(empty line)*                |

## Configuration

Add the following to `~/.tmux.conf`:

```tmux
set -g status-interval 5
set -g status-right-length 100
set -g status-right "#(/path/to/spot-defy now-playing)"
```

Then reload tmux: `tmux source-file ~/.tmux.conf`.

### Use an absolute path

tmux runs `#(...)` commands with its own `PATH`, which often does not include
your shell's `PATH`. Use the absolute path to the binary. Find it with:

```fish
command -v spot-defy
```

and substitute that path into `status-right`.

## Safety

Track and artist names come from Spotify and are treated as untrusted text:

- Control characters (newlines, tabs, carriage returns) are replaced with
  spaces so the output is always exactly one line.
- Each name is truncated to 40 characters (with an `…`) so a pathological title
  cannot dominate the bar; tmux additionally clamps the whole status to
  `status-right-length`.
- `#` is escaped as `##` so a song title can never inject a tmux format
  directive (e.g. a title like `#(rm -rf …)` is rendered literally, not run).

The subcommand only ever reads the socket and writes to stdout; it issues no
network calls and holds no credentials.
