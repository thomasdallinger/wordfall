# Design

The product is about **your words**. Everything else (settings, models, permissions) is plumbing
that should stay quiet until something needs attention. The UI should feel like a well-made Mac
utility (Things, Raycast, Craft), not a SaaS dashboard.

## Principles

1. **Words first.** Transcripts are the content and are typeset like writing (serif, comfortable
   measure, real line height). Chrome is small, neutral and sans.
2. **One signal colour, with one meaning.** The accent means *recording / live*. It appears on the
   pill while listening, the record button, and the single primary action on a screen. Nowhere else.
3. **Quiet until it matters.** No dashboards of numbers. Problems (permissions, missing model,
   blocked shortcut) surface as one clear, actionable line at the top. When all is well, the app
   simply shows your words.
4. **Explain by doing.** Teach the gesture by letting the user try it (onboarding ends with a live
   practice field), not with feature cards.
5. **Follow the system.** Light and dark, following macOS appearance. Respect reduced motion.

## Tokens

| Role | Light | Dark | Use |
|---|---|---|---|
| `bg` | `#F7F7F8` | `#151618` | window background |
| `surface` | `#FFFFFF` | `#1C1D20` | lists, sheets, inputs |
| `ink` | `#16171A` | `#ECECEE` | primary text |
| `ink-2` | `#5B5E66` | `#A0A3AB` | secondary text (≥ 4.5:1 on bg) |
| `ink-3` | `#8A8D95` | `#71747C` | tertiary / timestamps (large or non-essential only) |
| `line` | `#E6E6E9` | `#2A2B2F` | separators (prefer spacing over lines) |
| `signal` | `#E8472B` | `#FF5B3D` | recording, primary action |
| `signal-ink` | `#FFFFFF` | `#1A0A06` | text on signal |
| `ok` / `warn` / `bad` | `#1F8A4C` / `#B7791F` / `#C2362B` | `#4CC38A` / `#E3A43B` / `#F06A5A` | status only, always with words |

- **Type:** UI = *Schibsted Grotesk* (400/500/700). Transcripts = *Literata* (400, italic for
  drafts/processing). Both OFL, bundled via `@fontsource`, no network. Scale: 12 / 13 / 15 / 17 / 22 / 32.
  Weights contrast through size and 400 vs 700, not 500 vs 600 everywhere.
- **Spacing:** 4 px base, 8 px rhythm. Generous vertical space separates groups; lines are a last resort.
- **Shape:** radius 6 (controls), 10 (sheets/popovers), full (pill, record button). No drop shadows
  except floating layers (menus, popovers, the pill).
- **Icons:** sparingly, 1.5 px stroke, only where they carry meaning (search, delete, copy).
  Never as decoration next to labels or stats.
- **Motion:** 120–180 ms ease-out, no bounce, no count-ups, no fade-up-on-scroll. Waveform is the only
  continuous animation. All motion off under `prefers-reduced-motion`.

## Never

Purple/indigo or any gradients · glassmorphism / backdrop-blur panels · glowing borders ·
stat-tile grids · icon-in-rounded-square · three-card feature grids · uppercase tracked eyebrows ·
monospace "meta" chips · `A · B · C` meta strings · decorative numbering · arrows on buttons ·
"Get started" / "Seamless" / "Powerful" copy · emoji · sparkle icons · cream + terracotta ·
permanent dark mode.

## Information architecture

- **Sidebar:** *Transcripts* (home), *Dictionary*, *Settings*. Voice Lab lives under Settings → Accuracy.
- **Transcripts:** status line (only if something needs attention, or a one-line "Hold fn to dictate"
  hint when the list is empty) → search field → transcripts grouped by day ("Today", "Yesterday",
  "Monday 19 October"). Each entry: the text in serif, time on the left margin, actions on hover
  (copy, correct, delete). A quiet footer sentence carries stats: "4,210 words this week, 131 per minute."
- **Dictionary:** two simple lists: *Replacements* ("super base" → "Supabase") and *Words to
  recognise* (hint terms), plus suggestions inline as a small "Suggested" group you accept with one click.
- **Settings:** one scrolling page with anchored sections (General, Recording, Model, Pill,
  Accuracy, Data, About), macOS System Settings style rows: label + help text left, control right.
- **Onboarding:** one column, three steps with live checks: Permissions → Model → Try it
  (hold the key, see your words appear in a practice field).
- **Pill:** idle = small, low-contrast capsule with a single dot (or hidden). Listening = expands,
  signal-coloured live waveform. Processing = waveform settles into a slow pulse. Errors = one short
  sentence above the pill, dismissing itself.
