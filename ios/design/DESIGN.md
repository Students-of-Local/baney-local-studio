# Local Studio for iPhone and iPad: design contract

Direction B, "Needs you first", with direction A's thread view and direction C's machine health and pairing. The contact sheet that chose it lives beside this file (`contact-sheet.html`).

## Family rules (match the desktop app)

- Colors come from the desktop tokens (macos/Sources/Features/Pages/Theme/ThemeStore.swift `LegacyPalette`): canvas #0A0A0A dark / #FCFCFC light, card #111 / #FFF, accent #346BF1 dark / #1B4ED8 light, working #38BDF8, woke #FB923C, approval orange #FB923C, success #34D399, warning #FBBF24, danger #F87171, text white 85 percent, secondary #818181. Light mode uses the light palette. When a machine reports its theme in `Settings`, the phone may adopt its accent; default to the tokens above.
- Status words and glyphs are the desktop's: Working (cyan, animated mark only while live), Approval (hand, orange), Failed (x, red), Done (check, green), Woke (alarm, orange), Monitoring (eye, cyan). State never shows by color alone.
- Project tiles: 16 pt rounded square, two-letter monogram, same colors as desktop.
- System font (SF Pro), Dynamic Type everywhere, SF Symbols monochrome, regular weight. Code and commands in SF Mono.
- Native iOS 26 chrome: NavigationStack, Liquid Glass bars and buttons, sheets with detents, swipe actions, context menus, haptics on approve, deny, send and queue. No custom navigation, no web views for the transcript.

## Screens

1. **Home: Needs you.** Large title "Needs you" with a count. Cards for every thread across every machine whose status is Approval, Failed or Woke, newest first. An Approval card shows the command and effects tags with Decline, Always (session) and Approve buttons that answer in place. A Failed card shows the last error line with Open and Retry. Below: "Working" and "Done today" as dense one-line rows (status dot, title, age, machine), then "Earlier" (paged), "Snoozed" collapsed. Pull to refresh reconnects. Empty state: "Nothing needs you" with the count of working threads.
   - Swipe right on any row: Approve when it has one approval, else Mark read. Swipe left: Snooze (1 h, tonight, tomorrow), Settle, Pin. Long press: context menu with the same plus Rename and Archive.
   - Bottom floating glass cluster: New (plus), Voice (big mic), Search.
   - Top left: machines button showing a health dot (all green, any amber, any red); opens the Machines sheet.
2. **Machines sheet.** One card per paired machine: name, OS, app version and channel, link kind (Tailscale or Tunnel) and round-trip ms, counts (working, need you, threads). Tap filters Home to that machine. Add a machine at the bottom.
3. **Thread.** Navigation title is the thread title, subtitle is status and machine. Transcript mirrors the desktop: user bubbles right, assistant prose full width, thinking collapsed to one line, tool rows as one line (glyph, summary, state) expanding to output, file changes with +/- counts, turn summary rows. Streaming text appends without layout jumps. Opens at the bottom; "Load earlier" at the top pages back. Approval card pinned above the composer exactly like the desktop (orange border, command, tags, Decline and Approve, n/m counter). Queued messages show as a strip above the composer with Send now and remove.
4. **Composer.** One line that grows, attachments (photo library and camera), model and effort chip (opens a sheet), access mode chip, Stop while running, mic button. Send while a turn runs offers Queue (default) or Steer now by long press.
5. **Voice.** Full-screen sheet: target thread (or "New thread on <machine>/<project>"), live waveform, live on-device transcript (SpeechAnalyzer), Queue, Steer now, Send. Nothing leaves the phone until the user sends. Later this is the Silas engine.
6. **New thread.** Sheet: machine and project picker (recent first), worktree choice, model and effort and access, prompt with attachments and mic. Start returns to the new thread.
7. **Pairing.** QR scanner (camera), paste link, list of machines already reachable on the tailnet that are not paired yet (later). Pairing names the device.
8. **Settings.** Machines (rename, unpair), Notifications (which kinds), Appearance (system, dark, light), Voice (on-device only), About (version, channel, protocol). Small: no 40-page settings.

## iPad

NavigationSplitView: Needs you list in the sidebar column, thread in the detail column, machines as the sidebar's header. Keyboard: Cmd-N new, Cmd-Return send, Cmd-Period stop, Cmd-1..9 machines.

## Speed budget (measured with os_signpost, beaten or explained)

- Cold start to cached Home list: under 300 ms on iPhone 15-class hardware, under 500 ms in the Simulator.
- Cached list to live list: one round trip after the socket opens.
- Send to first transcript update: server time plus one round trip; the UI echoes the user bubble instantly.
- Scrolling a 2,000-item transcript: no dropped frames (List or LazyVStack with stable ids, no full re-render per token).

## Copy

Plain, short, sentence case. No em or en dashes. Errors say what failed and what to do: "parish1 is unreachable over Tailscale. Retrying in 8 s." with Retry now.
