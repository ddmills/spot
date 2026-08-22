# spot

A standalone Spotify player for the terminal, built with [ratatui](https://ratatui.rs) and
[librespot](https://github.com/librespot-org/librespot). It streams and plays audio itself —
no Spotify desktop app required. Requires **Spotify Premium**.

## Download

Grab `spot.exe` from the [latest release](https://github.com/ddmills/spot/releases/latest)
and double-click it. That is the whole install: one self-contained file, no runtime to add,
and it writes only to your own `%APPDATA%` and `%LOCALAPPDATA%`. Delete the exe and those two
folders and it is gone. Right-click it and *Pin to Start* if you want it somewhere findable.

You need:

- **Windows 10 or 11**
- **Spotify Premium** — librespot can only stream for Premium accounts
- **[Windows Terminal](https://aka.ms/terminal)**, or another terminal with 24-bit color —
  preinstalled on Windows 11, a free Store install on Windows 10. This is a real requirement
  rather than a preference: the whole palette is truecolor and album art is drawn as per-cell
  RGB half-blocks, so a 16-color console renders it as garbage.

Double-clicking is enough because spot notices when it was started from Explorer rather than
from a shell, and hands itself to Windows Terminal — the console Windows gives a
double-clicked exe is otherwise whatever the machine's default happens to be. If Windows
Terminal isn't installed, spot runs in that default console anyway and says at startup if it
can't tell the terminal has the color it needs.

Running it yourself still behaves exactly as it always did: `.\spot.exe` from a shell stays in
that shell. Set `SPOT_NO_RELAUNCH=1` to make a double-click stay put too.

The exe is not code-signed, so the first launch shows **"Windows protected your PC"**. Choose
*More info* → *Run anyway*; it is asked once per download. (If that bothers you, build it
yourself — see below.)

### First run

Two browser windows open, one after the other: the first authorizes the Web API, the second
authorizes the playback engine. Both are Spotify's own login page, both are one-time, and after
them spot refreshes its own tokens forever.

Windows may also ask to allow a network connection — spot briefly listens on `127.0.0.1:8989`
to catch Spotify's login redirect. Local only; nothing else can reach it. If some other program
already holds that port, spot says so and stops, because the port is registered with Spotify
and can't be changed.

## How it works

- **librespot** provides the playback engine: the app logs in with your Spotify account and
  registers itself as a Spotify Connect device named `spot` (it also shows up in the device
  picker of your phone/web player).
- Transport controls (play/pause/seek/volume/shuffle) go directly to the local player
  for instant response; browsing, search, and starting playback use the Spotify Web API.
- Album art is drawn as half-block pixels, so a truecolor terminal is required — as the
  spectrum analyzer already needed. It appears in three places: full size in the player
  view (`v`), as a thumbnail in the bottom bar, and beside the header of an album page.
  Art comes from Spotify's image CDN; for the playing track the URL rides along with the
  playback poll, so it costs no extra API call, and that sleeve's dominant color becomes
  the UI accent while it plays. Records with no colour to speak of fall back to the
  built-in gold. Browsing an album fetches its own sleeve without disturbing the accent —
  the record you are looking at and the one you are hearing are not always the same.
- Auth uses two OAuth browser flows with pre-registered client IDs — **no developer app
  registration needed**. The Web API uses ncspot's client ID (registered in extended quota
  mode; Spotify's own keymaster ID gets 429-rate-limited on `api.spotify.com`), while the
  streaming session uses the keymaster ID. The Web API refresh token is stored in
  `%APPDATA%\spot\auth.json`; session credentials are cached by librespot in
  `%LOCALAPPDATA%\spot\creds` after the first login, so both flows are one-time.
- Because the Web API client ID's quota is shared with all ncspot/spotify-player users,
  polling backs off automatically when Spotify returns 429.

## Where it keeps things

- `%APPDATA%\spot\auth.json` — the Web API refresh token
- `%LOCALAPPDATA%\spot\creds` — the playback session credentials
- `%LOCALAPPDATA%\spot\audio` — streamed audio cache, capped at 2 GiB
- `%LOCALAPPDATA%\spot\spot.log` — the log, rewritten each run

There is no config file; nothing needs setting up.

## Building from source

```
cargo build --locked --release
```

`--locked` matters: `Cargo.lock` pins `vergen` to 9.0.6, and librespot 0.8.0's build script
breaks with 9.1.0. If you regenerate the lockfile and the build fails inside `librespot-core`,
run `cargo update -p vergen@9.1.0 --precise 9.0.6`.

The build needs Rust 1.88 or newer. `.cargo/config.toml` links the MSVC runtime statically so
the binary doesn't need the VC++ redistributable; `build.rs` embeds the icon and version
resource, which wants `rc.exe` from the Windows SDK — without it you get a warning and an
unbranded but working exe.

## Getting around

One pane, full width. `♫ spot` sits at the top left of both the browse screen and the
player view: clicking it — or pressing `H` — goes Home, which is where the app opens and
where the back stack bottoms out.

Home lists **Liked Songs**, **Discover Weekly** (only when you follow it) and
**Playlists**, which holds everything you've saved or followed. A Home row opens on one
click, anywhere on it. From a playlist, a track row leads to its album or artist. Every
page spells the path that reached it across its top row — `HOME  ›  MUSE  ›  BLACK
HOLES` — with the page you're on at the head and every step before it clickable. A crumb
is a jump rather than a run of single steps: clicking `HOME` from three pages deep
restores Home as you left it. `Esc` and `Backspace` take one step back along the same
path.

The path stays a path rather than a log of everything you clicked. Opening a page that is
already on it walks *back* to that page instead of adding a second copy, so bouncing
between an album and its artist never lengthens the trail, and opening the page you are
already standing on does nothing. Search is one slot: a new query takes the old one's
place wherever it sat. A path too long for the row loses its middle — both `HOME` and the
page you're on stay put, with an `…` standing for what was dropped.

The player view draws the same path over the page waiting underneath, and clicking any of
it closes the player and lands you there.

## Keys

| Key | Action |
| --- | --- |
| `Space` | play / pause |
| `n / p` | next / previous track |
| `h / l` | seek -5s / +5s |
| `- / =` | volume down / up |
| `s` | toggle shuffle |
| `j / k`, `↓` / `↑` | move selection |
| `g / G` | top / bottom |
| `Ctrl-d/u` | half page down / up |
| `H` | go home |
| `v` | toggle player view (Esc closes) |
| `← / →` | switch search tab |
| `Backspace` | back to the previous view |
| `Esc` | back to the previous view · closes an overlay |
| `Enter` | drill into the selected row, or play it |
| `x` | play without opening (a playlist row, or the current view) |
| `a` | add selected track to queue |
| `L` | like / unlike the track — the selected row, or the playing one in the player view |
| `b / B` | open the selected track's album / artist |
| `o / O` | cycle sort column / flip sort direction |
| `/` | search Spotify |
| `R` | refresh the current view and your playlists |
| `?` | help overlay |
| `q`, `Ctrl-c` | quit |

Sorting reorders the visible list only; the playing-context order (and the
`→` next-up marker) follow Spotify's own order, so the marker hides while a
sort is active. Playlists longer than 500 tracks load fully, streaming in
page by page; reopening a playlist is instant until it changes on Spotify's
side (or you press `R`).

## License

MIT — see [LICENSE](LICENSE). spot builds on librespot, ratatui and rspotify, all MIT licensed;
it is not affiliated with or endorsed by Spotify.
