# Links and copying

Open links in messages, and select and copy message text.

## Decisions

- Message bodies are selectable: drag to select, `Ctrl+C` to copy.
  Right-clicking gives GTK's label menu (Copy, Select All, and on a link
  Open Link and Copy Link Address). Sender names, times and the "deleted"
  placeholder stay unselectable.
- Links are `http://` and `https://` URLs, and `www.` addresses (opened
  as `https://`). Emails, phone numbers and other schemes aren't linked.
- A link ends at whitespace or `<>"`. Trailing `.,:;!?'` are left out, as
  are closing brackets without an opening one in the link, so
  `(see https://example.com/a_(b))` links `https://example.com/a_(b)`.
  Matches that don't parse as a URL with a host stay plain text.
- Clicking a link opens it with the default handler via `gtk::UriLauncher`.
  Like opening images, the layer surface isn't passed as parent, since
  compositors disconnect clients that try.
- Clicking a message focuses it, so typing a printable character while a
  message (or nothing) has focus moves focus to the compose box and types
  it there. Other keys keep their meaning.
- The chat is re-rendered when its messages change (new messages,
  receipts), which drops a selection in progress. Accepted for now.

## Implementation

`ui/links.rs` finds links in a body and builds Pango markup with
`<a href>` around them, escaping the rest. The body label uses that
markup; its `activate-link` handler launches the URI and stops GTK's
default handler. CSS keeps links and selections readable on outgoing
bubbles, which have the account's colour.

## Testing

- Link finding: schemes, `www.`, case, trailing punctuation, brackets,
  non-links (`https://` alone, `foo.www.bar`).
- Markup: text and URLs are escaped; links get `https://` for `www.`.
- Demo mode has messages with links.
