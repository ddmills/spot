# spot

A standalone Spotify player for the terminal, built with [ratatui](https://ratatui.rs) and
[librespot](https://github.com/librespot-org/librespot). It streams and plays audio itself —
no Spotify desktop app required.

Spotify is an add-on rather than a requirement. With no account spot is an internet radio
player, which is the half that needs nobody's permission; connect an account from Home and
your library, your search and your records arrive with it. Playing a Spotify track needs
**Premium** — that is librespot's floor, not spot's.

## Download

Grab `spot.exe` from the [latest release](https://github.com/ddmills/spot/releases/latest)
and double-click it. That is the whole install: one self-contained file, no runtime to add,
and it writes only to your own `%APPDATA%` and `%LOCALAPPDATA%`. Delete the exe and those two
folders and it is gone. Right-click it and *Pin to Start* if you want it somewhere findable.

You need:

- **Windows 10 or 11**
- **Spotify Premium**, for the Spotify half only — librespot can only stream for Premium
  accounts. Radio asks for no account at all, and a free account still gets the record a
  station is playing named for it.
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

spot opens straight onto Home, with no login and no browser. Home lists **Radio**, which plays
at once, and **Spotify**, which connects an account.

Open the Spotify row and two browser windows follow, one after the other: the first authorizes
the Web API, the second authorizes the playback engine. Both are Spotify's own login page, both
are one-time, and after them spot refreshes its own tokens forever — later runs connect in the
background while the first frame is already up.

Windows may also ask to allow a network connection — spot briefly listens on `127.0.0.1:8989`
to catch Spotify's login redirect. Local only; nothing else can reach it. If some other program
already holds that port, spot says so and the sign-in fails, because the port is registered
with Spotify and can't be changed. Radio goes on playing either way.

## How it works

- **librespot** provides the audio engine: the app logs in with your Spotify account and
  streams tracks directly. spot owns the queue, the shuffle and the transport itself — it
  is not a Spotify Connect device, so it does not appear in the device picker of your
  phone/web player and cannot be remote-controlled from there. The list on the player
  screen *is* the play order, because spot wrote it.
- Transport controls (play/pause/seek/volume/shuffle) act on the local player
  for instant response; browsing and search use the Spotify Web API.
- Album art is drawn as half-block pixels, so a truecolor terminal is required — as the
  spectrum analyzer already needed. It appears in three places: full size in the player
  view (`v`), as a thumbnail in the bottom bar, and beside the header of an album page.
  Art comes from Spotify's image CDN; for the playing track the URL arrives with the
  track's own metadata, so it costs no extra API call, and that sleeve's dominant color becomes
  the UI accent while it plays. Records with no colour to speak of fall back to the
  built-in gold. Browsing an album fetches its own sleeve without disturbing the accent —
  the record you are looking at and the one you are hearing are not always the same.
- The visualizer has three modes, cycled with `V` in the player view. **bars** is the
  spectrum analyzer. **waveform** is what an audio editor draws — one bar per slice of
  time, as tall as the record was loud over it, standing either side of a centerline
  and walking left as new slices arrive, so the field is the last few seconds rather
  than this instant and a chorus is visible as a shape. **scope** drops the analysis
  entirely and draws the waveform itself, triggered on a rising zero-crossing so it
  holds still, in braille for eight times the vertical resolution the block characters
  have. All three take their colours from the playing sleeve, and all three work for a
  radio station as well as a track. The choice lasts the session; it starts on **bars**
  each launch.
- Auth uses two OAuth browser flows with pre-registered client IDs — **no developer app
  registration needed**. The Web API uses ncspot's client ID (registered in extended quota
  mode; Spotify's own keymaster ID gets 429-rate-limited on `api.spotify.com`), while the
  streaming session uses the keymaster ID. The Web API refresh token is stored in
  `%APPDATA%\spot\auth.json`; session credentials are cached by librespot in
  `%LOCALAPPDATA%\spot\creds` after the first login, so both flows are one-time.
- Neither flow runs at startup. The saved refresh token, when there is one, is spent in the
  background while the first frame is already drawn; the browser only opens for the Spotify
  row on Home. The sign-in hands the terminal back to the console for the length of it,
  because the OAuth flow prints the authorization URL. To sign out, delete `auth.json` and
  the `creds` file — spot is then the radio player it starts as.
- Spotify is asked first whether the account can stream, because librespot 0.8 does not
  merely refuse a login for an account it will not stream for — `Session::check_catalogue`
  ends the process, and nothing above it can intervene. `/v1/me` reports the level for
  spot's client ID, so a free account never reaches that call. Should it ever stop
  reporting, the login is attempted only where an exit costs nothing: from the Home row,
  which has already given the console back, or on cached credentials, which exist only
  because this account has streamed here before.
- Either way a free account keeps the Web API, so a station's announcement still gets
  looked up, named and given its sleeve — the library and the transport are what Premium
  buys.

## Updating

spot asks GitHub for the newest release once at startup. When there is one, an **Update
available** row appears at the top of Home with the version beside it; `Enter` downloads that
release's `spot.exe`, writes it over the running one, and the row then offers a restart.
Press `?` at any time to see which version is running.

The old executable is kept beside the new one as `spot.exe.old` until the next start, because
Windows will not delete a running program. Nothing is downloaded until you press `Enter`, and
a failed check is silent — spot works offline apart from the music.

## Where it keeps things

- `%APPDATA%\spot\auth.json` — the Web API refresh token
- `%APPDATA%\spot\radio.json` — your saved radio stations
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

Home lists **Liked Songs**, **Discover Weekly** (only when you follow it),
**Playlists** — everything you've saved or followed — and **Radio**. A Home row opens
on one click, anywhere on it. Until Spotify is connected the first three are not there and
a **Spotify** row is: opening it signs you in, and its right-hand end says where that got
to — `not connected`, `connecting…`, or `no Premium` for an account that cannot stream.
An **Update available** row joins them when a newer release exists — see
[Updating](#updating). From a playlist, a track row leads to its album or artist. Every
page spells the path that reached it across its top row — `HOME  ›  MUSE  ›  BLACK
HOLES` — with the page you're on at the head and every step before it clickable. A crumb
is a jump rather than a run of single steps: clicking `HOME` from three pages deep
restores Home as you left it. `Esc` and `Backspace` take one step back along the same
path.

An artist page carries their top tracks, then their records as cards under an **Albums**
heading. Where the catalogue runs to more than one kind, the heading gets a strip of tabs
(`←` / `→`, or click them) — **Albums**, **Singles**, **Compilations** and **Appears On** —
and it names only the ones that artist has records in. The whole catalogue arrives in one
fetch, so a tab changes what you're looking at without asking Spotify again, and without
adding a step to the path.

The path stays a path rather than a log of everything you clicked. Opening a page that is
already on it walks *back* to that page instead of adding a second copy, so bouncing
between an album and its artist never lengthens the trail, and opening the page you are
already standing on does nothing. Search is one slot: a new query takes the old one's
place wherever it sat. `/` asks both catalogues at once, wherever you press it, and the
answers come back on one page — **Tracks**, **Albums**, **Artists**, **Playlists** and
**Stations**. The Spotify tabs land first; stations follow when the directory answers. Without
an account the strip is **Stations** and nothing else, because there is nothing else to ask. A path too long for the row loses its middle — both `HOME` and the
page you're on stay put, with an `…` standing for what was dropped.

The player view draws the same path over the page waiting underneath, and clicking any of
it closes the player and lands you there.

## Radio

**Radio** is internet stations, which have nothing to do with Spotify — it is the one row
Home always has, and the whole app when there is no account.
Stations come from [Radio Browser](https://api.radio-browser.info), the community station
directory — no account, no API key, around 57,000 stations across 241 countries. The page
has four tabs (`←` / `→`, or click them): **Popular**, the directory's own most-voted chart;
**Countries** and **Genres**, which you drill into; and **Saved**, the ones you kept.
This page is for browsing; to find a station by name just use the search box, like
anything else — `/` asks Spotify and the directory together, and the stations come back
under a **Stations** tab beside the Spotify ones.

`Enter` plays the selected station, and `Enter` on the station already playing stops it.
`L` saves a station, or unsaves it; saved stations live in `%APPDATA%\spot\radio.json`,
because the directory has no accounts to keep them in. Playing a station reports a click
back to the directory — that is the only ranking signal the chart has, and spot reads that
chart on every screen of this feature.

The deck's `◂◂ previous` and `next ▸▸` walk what you have listened to. Previous steps back
a station at a time, and past the first station it returns to the Spotify queue that
station interrupted, where it was paused; next walks the same path forward again. Once
there is nothing left to go forward to, the right-hand control reads `seek ▸▸` and moves
down the playing station's own country instead, in the directory's chart order. `n` and
`p` do the same from the keyboard. Either way the station you are leaving goes quiet on
the press, not when the next one has finished connecting.

Plenty of stations in a directory of ten thousand no longer answer. One that will not play
stays named on the deck reading `OFF AIR` with the reason beside it, so the controls out of
it are still there and `▶ play` tries it again; a station that connects and then stops
sending is caught the same way. A seek that lands on a dead station walks on to the next
one in the country rather than stopping there, giving up after three.

Radio and Spotify never play at once: starting a station pauses Spotify, and starting a
Spotify track stops the station. The bottom bar and the player view (`v`) both switch to
the station, and the spectrum analyzer keeps working, because both engines feed the same
PCM tap. About six popular stations in ten announce their current track over ICY metadata.

Where one does, spot looks it up on Spotify — with any account, Premium or not; with none
the deck simply shows what the station wrote. On a confident match the deck stops showing
the station's raw text and shows the record instead — its own name, artist, album and year,
with the artist and album clickable exactly as they are for a Spotify track, `★` and `L`
saving it to your library, and `b` / `B` opening its album or artist page. Those two pages
need an account that can play, because nothing on them can be started without one; the name,
the sleeve and `L` do not. The stream keeps
playing throughout; none of those touch the audio device. The station's name moves to the
bottom row of the deck, where the queue is named for a Spotify track.

spot will not guess. The rules come from a sweep of 384 stations, because announcements are
spelled every which way: `Artist - Title` mostly, but also `Title by Artist`, with the
station's branding glued on after a dash or a pipe, or its name put in front of the record.
Plenty of what arrives in that field isn't music at all — station idents, adverts with the
advertiser billed as the artist, jingles, ad-break messages, `offline` markers.

What can't be read as a record is shown exactly as the station wrote it, and no search is
spent on it. What can be read but not confidently matched is also shown as-is: it simply has
nothing behind it. Only an announcement that agrees with a Spotify record on *both* the
title and the artist becomes a link you can act on, because the cost of getting that wrong
is a stranger's song saved to your library.

A couple of stations use formats spot doesn't read — a tilde-delimited record, a
plus-encoded one. Those fall through to "shown as the station wrote it", which is the same
place an unmatched announcement lands.

Stations that stream over **HLS** are listed and marked, but spot can't play them yet —
that is around 6% of the directory, and it does include most BBC and national-broadcaster
streams. They're shown rather than hidden because a directory that quietly omits the BBC
is a worse answer than a row that says why it won't play.

## Keys

| Key | Action |
| --- | --- |
| `Space` | play / pause |
| `n / p` | next / previous track or station |
| `h / l` | seek -5s / +5s |
| `- / =` | volume down / up |
| `s` | toggle shuffle |
| `j / k`, `↓` / `↑` | move selection |
| `g / G` | top / bottom |
| `Ctrl-d/u` | half page down / up |
| `H` | go home |
| `v` | toggle player view (Esc closes) |
| `V` | cycle the visualizer — bars, waveform, scope |
| `← / →` | switch tab (search results, artist pages, radio pages) |
| `Backspace` | back to the previous view |
| `Esc` | back to the previous view · closes an overlay |
| `Enter` | drill into the selected row, or play it |
| `x` | play without opening (a playlist row, or the current view) |
| `a` | play the selected track next |
| `L` | like / unlike the track — the selected row, or the playing one in the player view; on a station row, save it |
| `F` | save / unsave the playlist you are on — not shown on your own, where unsaving is how Spotify spells deleting |
| `E` | edit the name and blurb of a playlist you own |
| `b / B` | open the selected track's album / artist |
| `o / O` | cycle sort column / flip sort direction (every list, or click a column header) |
| `/` | search Spotify and the radio directory at once |
| `R` | refresh the current view and your playlists |
| `?` | help overlay, which also names the running version |
| `q`, `Ctrl-c` | quit |

Every browse list sorts: tracks, albums, playlists, artists, stations and the
radio directory's countries and genres. Click a column header to sort by it —
again to turn it round, a third time to clear it — or press `o` to step through
the columns the pane is showing and `O` to flip the arrow. The
player view's queue is the one list that does not sort — row 1 plays first, and
a sorted queue would lie about that.

Sorting reorders the visible list only. Playing a sorted list plays it in the
order you see — the queue is what is on screen, and the player view (`v`)
lists exactly the order that will play. Playlists longer than 500 tracks load
fully, streaming in page by page; a play started while pages are still
arriving grows as they land. Reopening a playlist is instant until it changes
on Spotify's side (or you press `R`).

## License

MIT — see [LICENSE](LICENSE). spot builds on librespot, ratatui and rspotify, all MIT licensed;
it is not affiliated with or endorsed by Spotify.
