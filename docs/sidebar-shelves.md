# Sidebar shelves

This page records how t3term lays out the sidebar's shelves and where it differs from the desktop app. The desktop source it follows is T3 nightly f570bd2, mostly `apps/web/src/components/Sidebar.tsx`.

## The shelves

From the top, the sidebar lists Pinned, the inbox, Working, Done, Snoozed and Settled. The inbox is the desktop's Active shelf.

| Shelf | What it holds | Heading | Starts |
| --- | --- | --- | --- |
| Pinned | Pinned threads that aren't snoozed or settled | `Pinned` | Open, doesn't fold |
| Inbox | Everything else that isn't on another shelf | `Inbox`, only under pins | Open, doesn't fold |
| Working | Inbox threads with a run under way or background work that will wake the agent, while nothing waits on the user | `Working (N)` and a chevron | Open |
| Done | Inbox threads whose latest run finished after the last visit | `Done (N)` and a chevron | Open |
| Snoozed | Threads asleep until their wake time | `Snoozed` | Open, doesn't fold |
| Settled | Settled threads, newest first, one line each | `Settled (N)` and a chevron | Closed |

Working follows `isThreadWorking` in `packages/client-runtime/src/state/threadInbox.ts`. A pinned, snoozed or settled thread keeps its shelf while it works. Done takes the inbox's threads whose status word is Done, after `hasUnseenCompletion`, and keeps them in the inbox's order. A thread that works never reads Done, so the two shelves never compete for a thread.

A closed shelf still lists the open thread, as `renderedSettledThreads` and `visibleWorkingThreads` do in the desktop, so sending a message or opening a settled thread never hides the card you are on.

## Keys and clicks

In the sidebar, `w`, `d` and `e` open or close Working, Done and Settled, and a click on a heading does the same. In the composer the letters are text. The status line offers each key only while its heading is in the list. A click acts on the heading the last frame drew on that row, which is the one on screen. Opening or closing a shelf moves the rows below its heading, so the next frame draws at once instead of waiting out the frame budget. Until it does, the sidebar forgets where it drew its cards and headings, and a click on the list does nothing rather than open a thread that has moved or fold another shelf.

Closing the shelf that holds the highlight moves the highlight to the next thread below the shelf, or else the last one above. A list scrolled into the closed shelf's threads keeps the heading at the top. The draft, the open thread and the transcript's place stay as they were. Nothing goes to T3.

The highlight is a `▎` bar, in the primary color while the sidebar has the keyboard and in the muted tone while it doesn't. The open thread has the open row's background and a bold title, so it stays distinct where a 256-color terminal makes the two backgrounds look alike. A heading says whether its shelf is open by its chevron, `⌄` or `›`, and not by color alone.

## Layout

- On a screen at least 20 rows tall, the sidebar keeps a blank row over the wordmark and another between the environment and the list. The conversation's header keeps a blank row over the breadcrumb, level with the wordmark. A shorter screen gives those rows back.
- A card takes three lines and a blank one. A settled thread takes one line with no blank. A heading under a card has a blank line over it, and one under another heading has none, so empty shelves stack.
- When the whole list fits, the leftover rows go above the first of the Working, Done, Snoozed and Settled headings, so those shelves sit at the bottom. That matches the desktop's `mt-auto` on the Working header, and on Snoozed and Settled when the shelves above are empty. A list that doesn't fit starts at the top.
- Scrolling keeps the highlighted card in view with its shelf's heading above it, and with the headings straight under it when they fit. That brings the closed shelves at the end of a long list into view from the last card.

## Settings and the migration

The choices live in `~/.config/t3term/settings.json`:

| Key | Default | Who owns it |
| --- | --- | --- |
| `sidebarWorkingShelfEnabled` | `true` | Shared with the desktop, which defaults to `false` |
| `sidebarWorkingShelfExpanded` | `true` | t3term |
| `sidebarDoneShelfExpanded` | `true` | t3term |
| `sidebarSettledShelfExpanded` | `false` | t3term |
| `sidebarShelvesVersion` | `1` | t3term |

Earlier builds started with the Working shelf off and wrote `false` for both Working keys in every save, whatever the user chose elsewhere. A `false` in an old file is therefore almost always the old default. A file without `sidebarShelvesVersion` is upgraded once as it is read. If the shelf is off, it becomes on and open. If it is on, it was turned on by hand, so whether it is open stays as saved. Reading writes nothing. The next save writes version 1, and from then on the file's values stand, so turning the shelf off sticks. The migration lives in `Settings::upgrade` in `src/settings.rs`.

## Where t3term differs from the desktop

- **Done is t3term's own shelf.** The desktop lists those threads in its inbox with the Done word. A separate shelf keeps unread results apart from threads that wait on the user.
- **Working starts on.** The desktop's setting starts off and is marked beta.
- **Counts show open or closed.** The desktop adds `(N)` only to a closed heading.
- **Working and Done keep their headings at 0** once the list has any thread, so `w` and `d` always act on something on screen. The desktop shows Working only while it has threads.
- **Pinned and Inbox have headings at rest.** The desktop's pinned header and divider are zero-height markers that label only during a drag. t3term has no drag, so it labels the shelves all the time, and shows the inbox's heading only when pins sit above it.
- **Snoozed doesn't fold** and its threads are full cards. The desktop starts Snoozed closed and shows one-line rows.
- **No visits.** Opening a Done thread in t3term doesn't clear the word or move it, because t3term doesn't send `thread.visit` yet.
