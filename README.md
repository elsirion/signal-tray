# signal-tray

A small Signal client that sits in a corner of your screen and hides in the
tray. It's built to pop up, answer a message, and get out of the way without
touching the mouse.

- Chat list → chat, like on mobile. Compact rows with small profile
  pictures (initials when there's none), also next to senders in groups
  and in notifications.
- Keyboard first: search, move, open, reply, and switch chats from the
  keyboard.
- Floats bottom-right via `wlr-layer-shell` (sway, Hyprland, river, KDE, …).
  Other environments fall back to a normal window.
- Tray icon (StatusNotifierItem) with an unread badge. Left click toggles.
- Desktop notifications when a message arrives and you're not looking.
  Clicking one opens that chat.
- Images: received ones are shown inline (click to open in your image
  viewer); send them by pasting, dropping or picking files.
- Voice messages play in the chat, at 1×, 1.5× or 2×; consecutive ones
  play one after another. A dot marks those you haven't heard.
- Links in messages open in your browser, and message text can be
  selected and copied.
- Read receipts, delivery ticks (✓ sent, ✓✓ delivered, bold ✓✓ read), edits,
  remote deletes, and sync with messages you send from your phone.
- Several accounts side by side, their chats in one list. Each account has
  a colour that marks its chats and your messages.
- Disappearing messages are honoured: they're deleted from the local store
  when their timer runs out. The timer counts from sending, not from
  reading, so they may vanish a little early.

Signal is spoken natively through [presage](https://github.com/whisperfish/presage),
using the [fifteenlabs fork](https://github.com/fifteenlabs/presage), which can
import your message history when linking (Signal's "Link & Sync"). No Java,
no helper process.

## Install / run

With Nix:

```sh
nix run .              # from a checkout
nix profile install .  # install
```

As a flake input:

```nix
environment.systemPackages = [ inputs.signal-tray.packages.${pkgs.system}.default ];
```

Without Nix you need GTK 4.14+, gtk4-layer-shell, OpenSSL, protoc, a
Rust toolchain, and GStreamer with its base, good, bad and libav plugins
(for voice messages): `cargo install --path .`

### First start: linking

On first start the window shows a QR code. On your phone go to
**Signal → Settings → Linked devices → Link new device** and scan it.
Your phone then asks whether to **transfer message history**. If you accept,
it uploads an encrypted archive that signal-tray downloads and imports (your
chats, and pointers to media from the last 45 days, whose images are
downloaded when you open their chat). If you quit during the import, it resumes on the next start. If you
skip it, chats start from the moment of linking.
Until the first message arrives, the list shows all contacts and groups
alphabetically; after that it shows chats only, and contacts are found by
typing.

### More accounts

Settings (`Ctrl+,` or the tray menu) → **Add account…** shows a new QR code;
link it like the first. All accounts' chats are shown in one list, most
recent first. The same person in two accounts is two chats, and replies
always go out from the account a chat belongs to.

With more than one account, a stripe in the account's colour marks each chat
in the list, and the chat header shows the account's name. Your own messages
are always drawn in the account's colour. Name and colour are changed in the
settings, next to each account; the name starts out as the phone number.
Removing an account there deletes its messages from this computer; remove
the linked device on your phone as well.

### Try it without an account

```sh
signal-tray --demo
```

This shows sample chats of two accounts from an in-memory store. It runs as a separate app
instance, so it never touches your real data.

## Global shortcut (sway)

The app is single-instance, so running `signal-tray toggle` while it's
running shows or hides the existing window. Bind that to a key:

```
# ~/.config/sway/config
exec signal-tray --hidden
bindsym $mod+m exec signal-tray toggle
```

`toggle` hides the window if it's focused and otherwise brings it up with
keyboard focus, so one key both summons and dismisses it. The other
commands are `show`, `hide` and `quit`. The tray icon needs a tray host, e.g.
swaybar's `tray` or waybar's `tray` module.

## Keys

| Where     | Key                              | Action                              |
|-----------|----------------------------------|-------------------------------------|
| anywhere  | `Esc`                            | back → clear search → hide          |
|           | `Ctrl+Q`                         | quit                                |
|           | `F1`                             | shortcut help                       |
|           | `Ctrl+,`                         | settings                            |
|           | `Super`/`Alt`+drag               | move the window (sway)              |
| chat list | type                             | search chats *and* contacts/groups  |
|           | `↑` `↓` / `Ctrl+J` `Ctrl+K` / `Ctrl+N` `Ctrl+P` | move selection       |
|           | `PgUp` `PgDn`                    | move selection by a page            |
|           | `Enter`                          | open chat                           |
|           | `Alt+1…9`                        | open the n-th chat                  |
| chat      | `Enter` / `Shift+Enter`          | send / new line                     |
|           | `Alt+↑` `Alt+↓` (`Ctrl+PgUp/PgDn`) | previous / next chat without going back |
|           | `PgUp` `PgDn`                    | scroll history                      |
|           | `Alt+←`                          | back to the list                    |
|           | `Ctrl+.`                         | emoji picker                        |
|           | `Ctrl+V`                         | paste text, or an image to send     |
|           | `Ctrl+O`                         | pick images to send                 |
|           | `Backspace` (empty message)      | remove the last image to send       |
|           | `Ctrl+Space`                     | play / pause a voice message¹       |

Search matches every contact and group, so it's also how you start a new
conversation.

¹ Pauses or resumes the one playing in the chat; otherwise plays the first
you haven't heard, or the newest. Input methods that use `Ctrl+Space`
(e.g. fcitx, IBus) take it first.

## Configuration

Optional, at `~/.config/signal-tray/config.toml`. All keys and their
defaults:

```toml
width = 340
height = 520
corner = "bottom-right"     # top-left | top-right | bottom-left | bottom-right
margin = 8                  # distance from the screen edges, or [horizontal, vertical]
opacity = 0.9               # window background, 0.2–1.0; text stays opaque
start_hidden = false        # same as --hidden
hide_on_focus_loss = false  # hide when another window gets focus
notifications = true
device_name = "signal-tray" # shown in the phone's linked devices list

# One per linked account, added when linking. Changed in the settings.
[[accounts]]
id = "1"                    # names its data directory, see below
name = "Personal"           # shown next to the chat title
color = "#2c6bed"
```

Unknown keys are rejected, so typos don't pass silently.

Opacity, hiding on focus loss and each account's name and colour can also
be set live in the settings
window (tray menu → Settings…, or `Ctrl+,`). On sway, Super+drag (or Alt+drag) moves the window; on release it snaps
to the nearest corner, keeping its distance from the edges, and `corner` and
`margin` are updated. Both rewrite the file, dropping any comments.

## Files

Each account has a directory `~/.local/share/signal-tray/accounts/<id>/`
with:

- `messages.sqlite`: your messages, as shown in the window
- `presage.sqlite`: Signal keys, sessions, contacts and groups (presage's
  store). presage also writes each message there as it arrives; those
  copies are moved into `messages.sqlite` and deleted on the next start, so
  there's one long-lived copy of each message.
- `attachments/`: received and sent images, and received audio. They're
  deleted along with their message (remote deletes, disappearing messages).
- `avatars/`: pictures of contacts and groups, checked for changes at most
  once a day.

Versions before multi-account support kept both files directly in
`~/.local/share/signal-tray/`; they're moved into `accounts/1/` on the first
start. An account directory that isn't listed in the config is left over
from linking that never finished, and is deleted on start.

None of these are encrypted yet; the directory is only accessible to your
user (mode 0700).

Logs go to stderr. Use `RUST_LOG=signal_tray=debug` for RPC-level detail.
Message contents are not logged.

## Not in v1

Attachments other than images and audio (received ones show as
`📎 name`), recording voice messages, view-once media, reactions, typing
indicators, calls, stickers (shown as `[sticker]`), changing a chat's
disappearing-message timer.

## Development

```sh
nix develop
just demo     # UI with sample data
just test
just clippy
```

Layout: `model.rs` holds the domain types, `accounts.rs` the per-account
file layout, `signal/convert.rs` turns decrypted Signal messages into domain
events (unit tested), `store.rs` is the SQLite history of one account (unit
tested), `signal/mod.rs` runs presage for one account on its own thread,
`app.rs` is the controller, `ui/` is the GTK window, and `tray.rs` is the
tray icon.

## License

AGPL-3.0-only, because presage is.
