# spot-defy

A privacy-first **Spotify player TUI** for the terminal, with embedded streaming
and a tmux now-playing integration. It plays audio itself through librespot, so
you do not need the Spotify desktop app running.

- **Embedded playback** via [librespot](https://github.com/librespot-org/librespot)
- **Search, playlists, albums, and library discovery** for top tracks/artists,
  recently played, and saved tracks
- **Privacy-first auth** with PKCE, no client secret, Keychain-only refresh
  tokens, and no telemetry
- **tmux status bar** support through a fast `now-playing` subcommand

> Requires macOS and a Spotify Premium account. librespot cannot stream on free
> Spotify accounts.

## Install

With Homebrew, after the first public release is published:

```fish
brew tap blacktop/tap
brew install --cask spot-defy
spot-defy auth login
spot-defy
```

From source:

```fish
cargo build --release --locked
./target/release/spot-defy auth login
./target/release/spot-defy
```

Running `spot-defy` without logging in first also works; it opens the browser
login flow automatically.

## Requirements

- macOS on Apple Silicon or Intel
- A Spotify Premium account
- Rust 1.85 or newer when building from source
- Optional: tmux for the status-bar integration

## Why Two Logins?

Spotify rate-limits Web API calls made with librespot's streaming client id.
spot-defy follows the same approach as spotify-player and ncspot: one PKCE flow
uses the librespot client id only for streaming, and a separate Web API client id
handles search, playlists, albums, and library reads. Both are one-time browser
authorizations; the rotating refresh tokens are cached in the macOS Keychain.

To use your own Spotify Developer app for Web API quota, create an app at the
[Spotify Developer Dashboard](https://developer.spotify.com/dashboard), add
`http://127.0.0.1:8899/login` as a redirect URI, and set the app id in
`~/Library/Application Support/spot-defy/config.toml`:

```toml
client_id = "your-app-client-id"
# redirect_port = 8899
```

## Keybindings

**Screens & Global**

| Key | Action |
| --- | --- |
| `1` / `2` / `3` | Search / Playlists / Library |
| `/` | Start a search |
| `Tab` / `Shift-Tab` | Cycle Library tabs or Search lanes |
| `q` / `Ctrl-C` | Quit |

**Navigation**

| Key | Action |
| --- | --- |
| `j` / `↓`, `k` / `↑` | Move down / up |
| `g` / `Home`, `G` / `End` | Jump to first / last |
| `Enter` | Play a track or open an album/artist/playlist |
| `Esc` | Back |

**Playback**

| Key | Action |
| --- | --- |
| `Space` | Play / pause |
| `n` / `p` | Next / previous track |
| `→` / `l`, `←` / `h` | Seek +5s / -5s |
| `+` / `=`, `-` / `_` | Volume +5 / -5 |

**Search Input**

| Key | Action |
| --- | --- |
| _type_ | Search automatically after a short pause |
| `Enter` | Search immediately |
| `Esc` | Stop editing |

## tmux Now-Playing

With the TUI running, show the current track in your tmux status bar:

```tmux
set -g status-interval 5
set -g status-right-length 100
set -g status-right "#(/path/to/spot-defy now-playing)"
```

tmux runs `#(...)` commands with its own `PATH`, so use the absolute binary path
from:

```fish
command -v spot-defy
```

`spot-defy now-playing` reads the running app's Unix socket and prints one line
with a hard 400 ms timeout. When nothing is playing, auth is missing, or the TUI
is not running, it prints an empty line and exits successfully. Full details are
in [docs/tmux.md](docs/tmux.md).

## Commands

| Command | Description |
| --- | --- |
| `spot-defy` | Launch the interactive TUI |
| `spot-defy auth login` | Run the streaming and Web API OAuth login flows |
| `spot-defy auth logout` | Remove stored refresh tokens from the Keychain |
| `spot-defy now-playing` | Print a one-line now-playing status for tmux |

## Privacy & Security

- **PKCE OAuth**: no Spotify client secret is stored anywhere.
- **Keychain-only persistence**: only rotating refresh tokens are persisted, in
  the macOS Keychain service `spot-defy`.
- **Memory-only access tokens**: access tokens stay in memory and are never
  written to disk.
- **Log filtering**: known token-bearing `TRACE` targets from librespot OAuth
  code are dropped before logs reach disk.
- **No telemetry**: the app talks only to Spotify.

Run `spot-defy auth logout` to remove stored refresh tokens.

## Release

Maintainer release flow:

```fish
just check
just tag 0.1.0
# wait for the GitHub Actions release to finish
just tap-update 0.1.0
```

`just tag` pushes an annotated `vX.Y.Z` tag. GitHub Actions builds Apple Silicon
and Intel macOS release tarballs, publishes them to the GitHub release, and
uploads `checksums.txt`.

`just tap-update` downloads `checksums.txt`, generates
`../homebrew-tap/Casks/spot-defy.rb`, and commits the tap change locally. Use
`just tap-update-push 0.1.0` when you want the tap commit pushed as part of the
same step.

## Troubleshooting

- **First source build is slow.** librespot pulls a large dependency tree;
  subsequent builds are much faster.
- **Track unavailable.** Spotify Premium is required for librespot streaming.
- **Login problems or stale token.** Run `spot-defy auth logout`, then
  `spot-defy auth login`.
- **tmux shows nothing.** The full TUI must be running; it serves the
  now-playing socket.

## License

MIT
