# Hotend desktop app: design specification

Scope: the window of `apps/desktop`

This is the one specification for the Hotend window. Where section 1 compares "Studio" and
"Guided", these are the two layouts that were considered. It specifies only what
the core could do when it was written (the methods of `hotend_core::Core`, listed per screen
in section 12). What the core could not do then is in section 14, "Later". Section 15 says how
each fact was checked. "This milestone" is the first version of the window. Section 13 and the
fixture mode of section 12 are the original plan, kept for the record; `README.md` says what
works today.

Words used throughout: the **maker** is the user. **Hotend** is the app and also the name
the AI goes by. A **version** is one immutable build of the model (`v2`). A **problem** is a
failed check of severity `error`; a **note** is a warning; a **guess** is a number
Hotend assumed and the maker should measure. The **core** is `hotend-core`; the **shell** is
the Rust side of `apps/desktop` (`src-tauri`), which owns the native dialog and file pickers.

## 1. Decision record

### 1.1 The structure is Studio's

The window is built on the Studio layout: three columns (conversation, the part, the
numbers), three stages in the title bar (Design, Prepare, Print), one verdict on the stage,
and nothing that ever opens over the part.

| Test | Studio | Guided | Why Studio |
| --- | --- | --- | --- |
| Familiar from printer software | Design, Prepare, Print across the top, a plate view, a device view | Five stops: Describe, Shape, Prove, Prepare, Print | A maker with a Bambu printer already reads three tabs across the top as "model, plate, printer". Five stops read as a wizard, however carefully they are called a readout |
| Restraint | One window, no sheets, one accent, one verdict | Review, calibration and settings are sheets over the workspace | `DESIGN.md` asks for "no modal walls" and "the part always centre stage". A sheet covers the part at the two moments the maker most wants to see it: review and measuring |
| Interactive | Sizes and Proof share the right column, so a dragged size and the proof it disturbs are on screen together | Sizes (Shape) and Proof (Prove) are two panels, one at a time | "Drag a size and watch the proof settle" is the product's signature moment. It cannot happen across two panels |
| Makes sense for a maker | Every stage has a different subject on the stage: the part, the plate, the print | Describe has nothing to show after the first version; Shape and Prove show the same part | A stage earns its place in the title bar only if the centre of the window changes |
| Honest about the build | A colour (`sim`) reserved for "Simulated" and "Stand-in", and a tag on every number | Badges in the normal text colours | In this milestone the printer is simulated and the slicer is usually the stand-in. That has to be impossible to miss and impossible to confuse with a status |
| Real hardware | Minimum window 1100 x 700 | Minimum 1280 x 800 | A 1366 x 768 laptop leaves about 700 px of height inside the window |

### 1.2 Grafted from Guided

1. **One next step, in one place, computed.** Guided's "Guide" (a pure function of project
   state: first matching row wins, one sentence, one primary action, pressed by
   `Mod+Enter`) is merged into Studio's verdict pill. The result is the **verdict bar** on the
   stage (section 3.4). It replaces Studio's primary buttons scattered between the pill and
   the bottom of the inspector.
2. **Stages are a readout, not a gate.** The marks on the stage switch are computed from the
   project and never stored. No stage is ever disabled: one that cannot do its job says what
   it waits for and offers the action that gets there. (Studio disabled Prepare and Print
   behind a tooltip.)
3. **Words come from the AI, numbers come from the engine.** AI prose appears only in the
   conversation. Every number, mark, tag and button is drawn by the app from typed data the
   core returned. Nothing the AI writes can look like proof or like a control.
4. **Sizes grouped by where the number comes from**: "To measure", "Your choices", "Fixed".
5. **A size that fails a check stays on screen with Undo**; only a value the engine cannot
   build snaps back. (Studio specified only the second case.)
6. **Version markers in the conversation and one live work line** with "Show steps", instead
   of one permanent line per tool call. The core stores only the text of a conversation, so
   markers (derived from the stored versions) are the part that survives a restart.
7. **First run is one page**, the health check starts by itself, a failed sandbox fails
   closed, a machine without a keychain is a designed state, and "Open the example" works
   without a key.
8. **Proof groups in the maker's words**, skipped checks listed last, and the sentence
   "Meets the guess. Not proven until you measure it."
9. **Error anatomy**: what happened, what to do, one button. Never a toast that disappears.
10. **An ellipsis only on a button that asks something more before anything happens**: a
    confirmation, a system picker, a popover of choices.
11. **No ruler in the 3D view**: a second, mesh-precise measurement would disagree with the
    engine's exact one.
12. **The printer chip is a menu** that shows the printer and a running print from any screen.

### 1.3 Every disagreement, resolved

| # | Topic | Studio | Guided | Decision |
| --- | --- | --- | --- | --- |
| 1 | Top navigation | 3 stages | 5 stops | 3 stages: Design, Prepare, Print |
| 2 | A stage that is not ready | Disabled, with a tooltip | Opens and explains | Opens and explains (3.3) |
| 3 | Where the next step lives | Pill, plus buttons in the inspector | Guide card above the message box | Verdict bar on the stage, everywhere (3.4). One exception: the button that starts a print sits under the numbers it starts (4.5) and has no shortcut |
| 4 | Right column in Design | Sizes and Proof together | One instrument per stop | Together |
| 5 | Print review | In the inspector | A sheet | In the inspector |
| 6 | Settings | A page | A sheet | A page; Back returns to where the maker was |
| 7 | Calibration | A route in the studio shell | A sheet | Neither: Later. The calibration wizard is in milestone 3, and the core can neither show a gauge in 3D nor slice one (14) |
| 8 | First run | Three steps beside a turning sample | One page, three cards | One page. No turning sample. The provider is chosen, never guessed from the key's prefix |
| 9 | Tool activity | A line per tool call | One work line, steps on demand | One work line, steps on demand (4.3.1) |
| 10 | A guessed number | A card above the composer, plus pill, row and proof | The Guide, the transcript, the row | One place to type: the row in Sizes. The verdict bar and the proof line point to it (5.4) |
| 11 | When a slider commits | On release; 400 ms after the last key | On release or after a 180 ms rest | On release, on Enter, or 400 ms after the last arrow key. A build takes about 6 s; a rest in mid-drag must not start one |
| 12 | Comparing versions | Hold Space to flip | Hold `\` for a ghost overlay | Neither. `[` and `]` step through versions at a fixed camera, which is the same blink test |
| 13 | Measuring in the view | A ruler tool | None | None |
| 14 | Minimum window | 1100 x 700 | 1280 x 800 | 1100 x 700; designed at 1280 x 800 |
| 15 | Conversation in Prepare and Print | Collapsed to a rail | Open | The maker's own choice, one setting for all stages; hidden means hidden, there is no rail |
| 16 | Fonts | System | System | Three bundled families (7.4). System fonts differ most on Linux, the first test bed, and acceptance is by screenshot |
| 17 | Token prefix | `--hotend-*` | Host tokens and module tokens | `--app-*` for the host window, `--hotend-*` for the module (`docs/IMPLEMENTATION_BRIEF.md`, section 9, rule 6) |
| 18 | Accent | Periwinkle `#8FA8FF`, fill `#4463EE` | Sky `#6CB2F2`, fill `#1B6FD0` | The brand's: ink for the one filled button, burnt orange (light) or amber (dark) for links and focus. Both stay clear of the viewer's fixed highlight blue `#53A0E3` |
| 19 | Simulated and stand-in | A reserved colour, `sim` | A badge | The reserved colour |
| 20 | Stage furniture | Opaque | 92 % alpha | Opaque: contrast over a 3D view must be provable |
| 21 | Panel titles | 11 px uppercase labels | Sentence-case headings | Uppercase labels for inspector sections only; sentence case everywhere else |
| 22 | Edge colour, light theme | `#3A4048` | `#16191D` | `#14161A`. Studio's value is below 3:1 against every one of its own part tints (measured 1.53 to 2.56) |
| 23 | Part tints | Eight neutrals | Six, with lilac, sage and rose | Studio's eight neutrals. In the light theme they are pale, like the dark set (bone stays bone, not khaki): the silhouette is carried by the 1 px edge, which is at least 3:1 against the stage and against every tint, on a slightly deeper stage |
| 24 | Stage background | Vertical gradient | Radial gradient | Vertical |
| 25 | Material look | Metalness 0.1, roughness 0.75 | 0, 0.7 | 0 and 0.7 |
| 26 | Stop key | `Mod+.` | `Esc` | `Mod+.`. `Esc` only closes and clears; it must not end a running AI request by accident |
| 27 | Stop a print, the second click | 4 s | 5 s | 4 s |
| 28 | Project thumbnails | Taken from the stage | The engine's `iso` picture | Taken from the stage and cached by the window. The engine's picture is a labelled diagram for the AI |
| 29 | Opening a project | Design | The stop the Guide points to | Design, or Print while one of its prints is running |
| 30 | Camera moves | 400 ms | 400 ms | Instant in this milestone; eased moves are Later. The viewer's presets cut, and the window does not ship its own copy of the 3D maths |
| 31 | Default theme | Dark | Dark first | Dark; the setting offers Dark, Light, System |
| 32 | Parts in Prepare | Take the filament's colour | Take the filament's colour | Keep their design tints: the core picks the spool only at review. Later, with colour |
| 33 | Pause a print | Shown | Shown | Later: the core can stop a job, not pause it |
| 34 | Picking faces, reference photos | With fallbacks | With fallbacks | Later (14) |

### 1.4 What was cut because the back end cannot do it yet

Calibration with a gauge, answering a measurement without an AI turn, checking a key when it
is saved, renaming or deleting a project, pausing a print, stopping a rebuild or a slice,
nozzle temperatures in the live view, choosing a nozzle or a spool, picking faces, reference
photos, "every size in this range was checked", and cost. Each is in section 14 with what it
needs. None of them has a placeholder in the window.

## 2. Rules that decide every argument

1. **The stage is never covered and never blank.** No dialog, sheet or drawer opens over the
   part. An old part stays on the stage until its replacement is ready.
2. **One verdict, one next step.** The verdict bar answers "can I print this, and what do I
   do now?". Everything else is detail behind it.
3. **Every number says where it came from**, in words, next to the number.
4. **Words from the AI, numbers from the engine.** See 1.2, item 3.
5. **The AI's work is shown as work on the part**, in plain words, never as code or a log.
6. **Nothing pretends.** "Simulated", "Stand-in slicer" and "Not proven on this printer" are
   written where the affected number is shown. Simulated and stand-in use a colour that is
   used for nothing else.
7. **The human starts the print.** The window can only ask the core for its confirmation
   dialog. No shortcut, no AI text and no page script can press Start.
8. **Going back costs nothing.** Versions are immutable; undo is "use the previous version".

## 3. Information architecture

### 3.1 Window, routes, namespaces

One window. No second window, no floating palettes, no document tabs. The only thing that
appears outside it is the confirmation dialog the shell draws (4.6), and the system's folder
picker.

| Route | Screen | Shown when |
| --- | --- | --- |
| `/setup` | Setup (4.1) | Automatically at start while no provider is chosen and no project exists; later through Settings |
| `/hotend` | Home (4.2) | Otherwise at start |
| `/hotend/p/:project/design` | Studio, Design (4.3) | A project opens here. `?v=4` while an older version is being looked at |
| `/hotend/p/:project/prepare` | Studio, Prepare (4.4) | |
| `/hotend/p/:project/print` | Studio, Print (4.5 to 4.7) | A project opens here while one of its prints is starting or printing |
| `/settings/:section` | Settings (4.8) | `ai`, `printer`, `slicer`, `appearance`, `computer`, `shortcuts`, `about` |

Which screen shows at start is derived from the settings and the project list, never stored.

The window is a host with Hotend as its one module (`docs/IMPLEMENTATION_BRIEF.md`, section 9,
rule 6). In code: `src/host/` holds the frame, the title bar, Settings, the theme and the `--app-*` tokens; `src/hotend/`
holds every Hotend screen, its stores (`hotend*` names) and the `--hotend-*` tokens. The
left end of the title bar, where the Home button is, is where a module switch would go.
Nothing is built for other modules.

### 3.2 The studio layout

```
┌────────────────────────────────────────────────────────────────────────────────────────────────────┐
│ [house] [<|]  Cable box v          ( Design )   Prepare   Print     [flask] X2D Simulated [|>] [*] │
├─────────────────────────┬──────────────────────────────────────────────────┬───────────────────────┤
│CONVERSATION             │STAGE                                             │INSPECTOR              │
│default 360, 300 to 520  │all the width that is left, never under 520       │default 340, 300 to 440│
│hidden with Mod+\        │                                                  │hidden with Mod+Shift+\│
│                         │ verdict bar                       view tools     │                       │
│transcript (scrolls)     │                                                  │the numbers of the     │
│                         │                  the part                        │stage in view, and why │
│                         │                                                  │to trust them          │
│                         │ axis marker (the viewer's)        part chips     │                       │
│                         │                                  overall size    │                       │
│composer (fixed)         │ version rail (a strip under the 3D view)         │                       │
└─────────────────────────┴──────────────────────────────────────────────────┴───────────────────────┘
```

Left is talk, centre is the part, right is the numbers of the stage in view. Left and centre
never change their job. Only the inspector changes with the stage:

| Stage | The stage shows | The inspector shows |
| --- | --- | --- |
| Design | The parts as modelled and assembled, on the grid | Sizes, then Proof |
| Prepare | The pieces lying on the plate, as they will be printed | Plate, Material, Slice |
| Print | The plate; while a job runs, cut at the printed height | Review, or the running job |

Sizes and behaviour:

- Title bar 44 px. Design size 1280 x 800. Hard minimum 1100 x 700 (set in the shell).
- Conversation 360 px (300 to 520). Inspector 340 px (300 to 440). The stage takes the rest
  and is never narrower than 520 px. When the window is narrower than 1220 px at start, the
  conversation starts hidden.
- Two drag handles: an 8 px hit area on a 1 px line, cursor `col-resize`, arrow keys move a
  focused handle by 16 px, double-click resets to the default width.
- `Mod+\` hides and shows the conversation, `Mod+Shift+\` the inspector; each also has a
  button in the title bar (`panel-left`, `panel-right`). A hidden column takes no space. The
  conversation button carries a 6 px `accent` dot when Hotend wrote something while the
  column was hidden.
- Widths and hidden states are one setting for all stages, kept in the window's
  `localStorage` (`hotend.layout`).
- Text never runs wider than 68 characters. Home's content is capped at 1168 px and centred.
- macOS uses an overlay title bar with the traffic lights inset left of the Home button.
  Windows and Linux keep the system frame; the 44 px bar sits under it.
- Each column scrolls by itself; the window never scrolls.

Title bar, left to right: Home (`house`), the conversation button, the project name (a menu:
Export files…, Show files), the stage switch in the centre, the printer chip, the inspector
button, Settings (`settings`). On Home and in Settings there is no project name and no stage
switch.

### 3.3 The stage switch

Three segments in the centre of the title bar. The one in view is filled (`--app-active`, a
1 px `--app-control` border, `--app-text`). Each segment may carry one mark after its label.
Marks are computed on every change of the project and never stored.

| Mark | Design | Prepare | Print |
| --- | --- | --- | --- |
| none | No version yet | Not sliced yet | No print job |
| ◌ busy (`loader-circle`) | Hotend is working, or a rebuild is running | Laying out or slicing | A print is starting or printing |
| × problem (`circle-x`, `fail`) | A problem | A piece fits no plate, or the last slice failed | The last job was refused, did not start, or failed |
| ! needs you (`circle-question-mark` for a number to measure, else `triangle-alert`; `warn`) | A number is still a guess, or there are notes | The slice is out of date | A print waits for your review |
| ✓ done (`circle-check`, `ok`) | No problem, no guess, no notes | The current version has a current slice | The newest job of the current version finished, was saved or was handed over |

A segment is never disabled. A stage that cannot do its job yet shows the part, an empty
inspector section with one sentence, and the verdict bar's button that gets there: in Prepare,
"Fix 1 problem in Design first" with `Go to Design`.

The window changes stage by itself in no case. A finished AI run, a failed check and a new
print proposal change the marks and the verdict bar, never the view.

Accessible name of a segment: the label and the mark in words ("Design, 1 problem").

### 3.4 The verdict bar

A capsule at the top left of the stage, 40 px high, opaque `--app-raised` with a 1 px
`--app-line` border and shadow 1. It holds a status icon, one sentence and at most one
primary button (`accent-fill`, 28 px high) and one quiet text button. It is the only place
a filled button appears in the studio, with the one exception of 4.5.

- The content is a pure function of the project state. Rows are tried top to bottom; the
  first that matches wins.
- `Mod+Enter` presses the primary button, from anywhere in the studio.
- Clicking the sentence scrolls the inspector to what it speaks about and opens that line.
- The bar is `role="status"` with `aria-live="polite"`. Status is never colour alone: each
  state has its own icon shape and its own words.
- When the sentence does not fit beside the view tools it ends in an ellipsis; the full
  sentence is the tooltip.
- A tick is never shown for a version whose checks are not back.

Design stage:

| # | State | Icon | Sentence | Primary | Quiet |
| --- | --- | --- | --- | --- | --- |
| 1 | No version, no AI key | none | Add an AI key to start designing. | Open Settings | Open the example |
| 2 | No version, nothing running | | (the bar is hidden; the empty stage speaks, 4.3.4) | | |
| 3 | An AI run is going | spinner | Hotend is working: (the work line, 4.3.1) | Stop | |
| 4 | A rebuild is running | spinner | Rebuilding with Height 46 mm (after 10 s: "… taking longer than usual") | | |
| 5 | An older version is on the stage | `history` | Looking at v1 of 2 | Back to v2 | Use v1 |
| 6 | The last size change could not be built | `circle-x` | Height 80 mm does not build. Still at 46 mm. | | |
| 7 | A problem, and the current version came from a size change | `circle-x` | Wall thickness 0.5 mm gives 1 problem: Wall too thin | Undo that change | Ask Hotend to fix it |
| 8 | A problem | `circle-x` | 1 problem: Wall too thin (or "2 problems to fix") | Ask Hotend to fix it | Show me |
| 9 | Numbers to measure | `circle-question-mark` | 3 numbers are guesses (or "1 number is a guess") | Measure them (or "Measure it") | Prepare anyway |
| 10 | Notes only | `triangle-alert` | Ready to print, with 1 note | Prepare | Read it |
| 11 | Everything passes | `circle-check` | Ready to print | Prepare | |

Row 6 stays until the next change of any size or the next message. "Prepare" and "Prepare
anyway" go to the Prepare stage. "Ask Hotend to fix it" sends the problem to the AI as a
message the maker can read (5.2). "Measure them" scrolls to the first row of "To measure",
opens it and puts the caret in its field. The sentences of rows 4, 6 and 7 are put together by
the window from the size's label, its value and unit, and the title of the first failing
Proof line (5.2); row 7 with several problems reads "Wall thickness 0.5 mm gives 2 problems".

Prepare stage (row 3 of the Design table applies here too, first):

| # | State | Icon | Sentence | Primary |
| --- | --- | --- | --- | --- |
| 1 | No version | none | Nothing to prepare yet. | Go to Design |
| 2 | The version has a problem | `circle-x` | Fix 1 problem in Design first. | Go to Design |
| 3 | Laying out | spinner | Laying out the plate | |
| 4 | A piece fits no plate | `circle-x` | The lid does not fit on a plate. Turn it, or make it smaller. | |
| 5 | Slicing | spinner | Slicing plate 1 | |
| 6 | No slicer can run | `triangle-alert` | No slicer is set up, so there is no time or weight yet. | Open Settings |
| 7 | The last slice failed | `circle-x` | (one sentence for the code, 4.4) | Slice again |
| 8 | Changed since the last slice | `triangle-alert` | Changed since the last slice. | Slice again |
| 8a | Sliced without plate previews while Hotend-made previews are on in Settings (a slice made before they were switched on) | `triangle-alert` | Sliced before Hotend drew plate previews. | Slice again (quiet: Review print) |
| 9 | Not sliced | `circle-check` | Fits on one plate. (or "Fits on 2 plates. Showing plate 1.") | Slice |
| 10 | Sliced | `circle-check` | Sliced: 59 min · 28 g | Review print |

"Review print" goes to the Print stage.

Print stage (row 3 of the Design table applies here too, first):

| # | State | Icon | Sentence | Primary |
| --- | --- | --- | --- | --- |
| 1 | The current version has no current slice, and no job is running | none | Slice this version first. | Go to Prepare |
| 2 | The dialog is open | spinner | Waiting for your answer in the Hotend dialog | |
| 3 | Sent, the printer has not started yet | spinner | Waiting for the printer to start · 0:42 | |
| 4 | Getting ready | spinner | Getting ready: heating | |
| 5 | Printing | spinner | Printing · layer 84 of 200 | |
| 6 | Finished | `circle-check` | Finished. | Print again |
| 7 | Did not start, or failed | `circle-x` | (the printer's sentence, 4.7) | Review again |
| 8 | Saved or handed over | `circle-check` | Saved to the folder. (or "Saved for Bambu Connect. You open it and start it there.") | |
| 9 | The file check refused the file, or no spool matches | `circle-x` | This print cannot be sent yet. | Back to Prepare |
| 10 | The file is being checked | spinner | Checking the sliced file | |
| 11 | Ready for review | `circle-question-mark` | Check the numbers, then start | |

In row 11 the bar has no button on purpose: the button that starts a print is under the
numbers in the inspector, and `Mod+Enter` must never lead to a print.

### 3.5 Always visible in the studio

Whatever the stage and whatever is hidden: the part, the verdict bar, the project name, the
stage switch with its marks, and the printer chip with its "Simulated" tag.


## 4. Screens and states

Marks in the wireframes: `(ok)` pass, `(!)` note, `(x)` problem, `(?)` needs a measurement or
an answer, `(..)` busy; in the title bar the same five are drawn short as `✓ ! × ? ◌`.
`[name]` is an icon by its lucide name (`[*]` Settings, `[<|]` and `[|>]` the two column
buttons, `[↑]` send, `[■]` stop, `[#]` a colour swatch), `[ Label ]` a button, `( Label )` the
selected segment, `v` a menu, `(●)` the ring on the rail. Frames are drawn at 1280 x 800.
The studio frames show the fixture project "Cable box" of section 12 at its second version,
with the numbers the core returned for it, so each frame is also what its screenshot must
show. A column of a frame is narrower than the real one: an engine sentence cut with "…" in
a frame is shown in full in the window.

### 4.1 Setup

One page, three sections, any order, no step counter. The health check starts by itself when
the page opens.

```
┌────────────────────────────────────────────────────────────────────────────────────────────────────┐
│ Hotend                                                                                            │
├────────────────────────────────────────────────────────────────────────────────────────────────────┤
│                                                                                                    │
│              Set up Hotend                                                                        │
│              You describe a product. Hotend models it, checks it and gets it ready to print.      │
│                                                                                                    │
│              YOUR AI                                                                               │
│              ┌──────────────────────────────────────────────────────────────────────┐              │
│              │ Hotend designs with an AI model that you pay for directly.          │              │
│              │ Provider   ( Anthropic )  OpenAI   Google   OpenRouter   Ollama      │              │
│              │ Model      [ model id                                           v ]  │              │
│              │ API key    [ ••••••••••••••••••••••••••••••••••••••••••• ] [ Save ]  │              │
│              │            Stored in this computer's keychain. Hotend's AI never    │              │
│              │            sees it.                    Where do I get a key? [link]  │              │
│              └──────────────────────────────────────────────────────────────────────┘              │
│              YOUR PRINTER                                                                          │
│              ┌──────────────────────────────────────────────────────────────────────┐              │
│              │ Bambu Lab X2D                                     [flask] Simulated  │              │
│              │ This version never sends a print to a real printer. Everything up to │              │
│              │ the print is real; the print itself is simulated, or you save the    │              │
│              │ sliced file for a USB drive or Bambu Connect.                        │              │
│              │ Material   [ PLA Basic                                          v ]  │              │
│              └──────────────────────────────────────────────────────────────────────┘              │
│              THIS COMPUTER                                         2 ready · 2 notes               │
│              ┌──────────────────────────────────────────────────────────────────────┐              │
│              │ (ok) Safe room for AI-written models   On: no network, and it cannot │              │
│              │                                        open your files.              │              │
│              │ (ok) Modelling engine                  Built a test block in 5.0 s.  │              │
│              │ (!)  Slicer                            None found.                   │              │
│              │                                        [ Use the stand-in ]          │              │
│              │ (!)  Key storage                       No keychain on this computer. │              │
│              └──────────────────────────────────────────────────────────────────────┘              │
│              [ Start designing ]              Open the example instead (no key needed)             │
│                                                                                                    │
└────────────────────────────────────────────────────────────────────────────────────────────────────┘
```

**Your AI.**
- Provider is a five-segment control: Anthropic, OpenAI, Google, OpenRouter, Ollama. Nothing
  is detected from the key.
- Model is a combobox that also takes typed text (the core accepts any model name of 1 to 200
  printable characters). Its suggestions come from one data file in the app,
  `src/host/data/providers.json`: per provider up to three model ids, the address of its key
  page, and the date and source the list was taken from. The first suggestion is prefilled.
  This specification names no model id on purpose; the file is filled from each provider's
  public model list when the app is built. For Ollama the field is empty with the placeholder
  "The name of a model you have pulled" and there is no key row, only "Uses the Ollama
  running on this computer."
- The key field is write-only. Save stores it (`provider_key_set`). The row then reads "Key
  saved · in this computer's keychain" with Replace and Remove, and never shows the key or
  part of it. Under it: "Hotend checks the key with your first message." (The core cannot
  test a key yet, 14.)

| Key state | What the row shows |
| --- | --- |
| Empty | The field and a disabled Save |
| Saving | "Saving…" in the button, the field read-only |
| Saved | "Key saved · in this computer's keychain" · Replace · Remove |
| No keychain (the doctor's `secrets.keyring` is not `ok` and no key is found) | The block below instead of the field, with the provider's own variable name |
| Key found in the environment (the core runs with `HOTEND_SECRETS=env`) | "Using the key in ANTHROPIC_API_KEY. Hotend reads it and never stores it." No Replace, no Remove |
| The store failed | `(x)` and the core's message and next step, word for word |

```
┌──────────────────────────────────────────────────────────────────────┐
│YOUR AI                                                               │
│Provider   ( Anthropic )  OpenAI   Google   OpenRouter   Ollama       │
│Model      [ model id                                           v ]   │
│API key    (!) This computer has no keychain, so Hotend cannot store │
│           a key here. Close Hotend and start it again with these two│
│           set; it then reads the key from there and never stores it: │
│             HOTEND_SECRETS=env                                      │
│             ANTHROPIC_API_KEY=<your key>                             │
│           [ Copy these lines ]                                       │
└──────────────────────────────────────────────────────────────────────┘
```

The variable names are `ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, `GOOGLE_API_KEY`,
`OPENROUTER_API_KEY`.

**Your printer.** The one printer of this build, from `printers_list`: its name, the
"Simulated" tag and the paragraph drawn above. Material is a menu of the three materials the
build has slicer settings for: PLA Basic, PETG Basic, TPU 95A (4.8). There is no "Add
printer".

**This computer.** The doctor report (`doctor_report`), which takes about 10 s. Until it
returns: four rows with a spinner and "Checking this computer…". Then four rows, each the
worst status of a group of the report's checks:

| Row | Checks of the report (by `id`) | When ok | When not ok |
| --- | --- | --- | --- |
| Safe room for AI-written models | `sandbox` and `sandbox.*` | On: no network, and it cannot open your files. | The failing check's `detail`, then its `fix` |
| Modelling engine | `python`, `engine`, `agent` | Built a test block in 5.0 s. (the time from the `engine` detail) | The failing check's `detail`, then its `fix` |
| Slicer | `slicer.*` | Bambu Studio 2.x found. | None found. You can design and check parts; slicing needs Bambu Studio. `Open Settings, Slicer` |
| Key storage | `secrets.*` | This computer's keychain. | No keychain on this computer. (the AI section shows what to do) |

A check with an unknown `id` gets a row of its own with the report's `label`. "Details" under
the list opens every check of the report as the core worded it. The section's caption counts
rows: "2 ready · 2 notes".

`Use the stand-in` switches the stand-in slicer on (4.8, Slicer). The stand-in is never
switched on silently.

**Leaving the page.**
- `Start designing` (the page's one filled button) saves the provider and model and goes to
  Home. It is enabled when no required check failed, a provider and a model are chosen, and
  a key is saved or found (or the provider is Ollama). While disabled, one line under it says
  what is missing: "Save an AI key to start, or open the example."
- `Open the example instead (no key needed)` builds the bundled cable box as a project named
  "Cable box (example)" and opens it. It needs no AI: the app builds a model file it ships
  with, in the sandbox, about 6 s.
- A failed required check (the sandbox, the engine) disables both and replaces the line with:
  "Hotend cannot run AI-written models safely on this computer, so it will not run them at
  all." and the check's fix. There is no way to continue without the sandbox.

### 4.2 Home

```
┌────────────────────────────────────────────────────────────────────────────────────────────────────┐
│ Hotend                                                                 [flask] X2D Simulated  [*] │
├────────────────────────────────────────────────────────────────────────────────────────────────────┤
│                                                                                                    │
│                                   What do you want to make?                                        │
│              ┌────────────────────────────────────────────────────────────────────┐                │
│              │ Describe it, with the sizes you know                           [↑] │                │
│              └────────────────────────────────────────────────────────────────────┘                │
│              [ A box that hides a power strip… ]  [ A wall hook for a 32 mm pipe… ]                │
│              [ A drawer divider for cutlery… ]                                                     │
│                                                                                                    │
│  Projects                                                                                          │
│  ┌──────────────────┐  ┌──────────────────┐  ┌──────────────────┐  ┌──────────────────┐            │
│  │                  │  │                  │  │                  │  │                  │            │
│  │    (picture)     │  │    (picture)     │  │    (picture)     │  │      [box]       │            │
│  │                  │  │                  │  │                  │  │                  │            │
│  │ Cable box        │  │ Cup              │  │ Bracket          │  │ Plain box        │            │
│  │ (..) Printing 42%│  │ (x) 1 problem· v2│  │ (!) Ready, 1 note│  │ (ok) Ready · v1  │            │
│  │ Just now         │  │ 12 min ago       │  │ Yesterday        │  │ 3 Nov            │            │
│  └──────────────────┘  └──────────────────┘  └──────────────────┘  └──────────────────┘            │
│  ┌──────────────────┐                                                                              │
│  │    (picture)     │                                                                              │
│  │ Cable box (exam… │                                                                              │
│  │ (?) 3 to measure │                                                                              │
│  │ 3 Nov            │                                                                              │
│  └──────────────────┘                                                                              │
└────────────────────────────────────────────────────────────────────────────────────────────────────┘
```

- **The prompt is the page.** A text area of one to six lines, 720 px wide, focused when the
  page opens. Enter sends, Shift+Enter makes a new line. Sending creates a project named
  with the first six words of the text (at most 48 characters), opens the studio and sends
  the text as the first message. There is no "New project" dialog.
- **Suggestions**: three quiet buttons under the prompt. Choosing one fills the prompt and
  does not send.
- **Cards**: 264 px wide, a 3:2 picture, the name (one line, ellipsis), one status line and
  the time of the last change. Most recently changed first. Up to four in a row at 1280. A
  click opens the project. The `ellipsis` button that appears on hover or focus offers Export
  files… and Show files.
- **Picture**: the stage as it looked the last time that version was on screen in Design,
  kept by the window (10.6). A version that was never on screen (a project made with the
  command line) shows the `box` icon in `--app-text-3` on `--app-subtle`.
- **Status line**: the Design verdict of the current version, short, with its version:

| State | Line |
| --- | --- |
| No version | Nothing built yet |
| Problems | `(x)` 2 problems · v1 |
| Numbers to measure | `(?)` 3 to measure · v2 |
| Notes | `(!)` Ready, 1 note · v1 |
| All pass | `(ok)` Ready · v1 |
| A print is starting or printing | `(..)` Printing 42 % (and the `sim` tag "Simulated") |

- **Time**: "Just now" (under a minute), "12 min ago", "2 hours ago", "Yesterday", "3 Nov",
  "3 Nov 2025" for another year.
- **Search** (`search`) appears when there are more than eight projects; it filters by name
  as the maker types.

| State | What shows |
| --- | --- |
| No projects | The prompt and suggestions, centred vertically, and under them: "Your projects will appear here." and the link "Open the example: a cable box with a snap-on lid" |
| Loading | Card outlines at 40 % opacity, no shimmer; the names come first, pictures appear when read |
| The project folder cannot be opened | In place of the grid: "Hotend could not open its project folder." the path in monospace, the core's next step, and `Try again` |
| No AI key | The prompt works as a field. Sending shows under it: "Add an AI key to start." with `Open Settings`, and keeps the text |
| A required health check fails | Above the prompt, a `fail` card with the sentence of 4.1 and `Open This computer`; the prompt is disabled |

### 4.3 Studio: Design

```
┌────────────────────────────────────────────────────────────────────────────────────────────────────┐
│ [house] [<|]  Cable box v         ( Design ! )   Prepare   Print    [flask] X2D Simulated [|>] [*] │
├─────────────────────────┬──────────────────────────────────────────────────┬───────────────────────┤
│You                      │ ┌────────────────────────────────────────────┐   │SIZES                  │
│ A box that hides a power│ │ (?) 3 numbers are guesses [ Measure them ] │   │To measure             │
│ strip, with a snap-on   │ └────────────────────────────────────────────┘   │(?) Inside width       │
│ lid                     │                      [scan] [box] [scissors]     │    Hotend's guess    │
│Hotend                  │                                                  │    [ 120 ] mm         │
│ Here is the plan: a box │               ┌───────────┐                      │   [Use my measurement]│
│ and a lid that snaps on…│              ╱           ╱│                      │(?) Inside depth  60 mm│
│── v1 · Hotend ─────────│             ┌───────────┐ │                      │(?) Cable slot    12 mm│
│ (x) 2 problems          │             │           │╱                       │Your choices           │
│── v2 · Hotend ─────────│             └───────────┘                        │Height          [40] mm│
│ Now passes: Walls thick │                                                  │├────●────────────────┤│
│ enough, Wall of the box │                                                  │26 to 80               │
│ It is 124 × 64 × 42.4 mm│                                                  │Wall thickness   [2] mm│
│ outside. The inside     │                                                  │├─────●───────────────┤│
│ sizes are my guesses.   │                                                  │1.6 to 3               │
│ 9 steps · 38k tokens    │                                                  │───────────────────────│
│                         │                                                  │PROOF         10 of 15 │
│                         │                                                  │What it must do 2 of 5 │
│                         │ ╵z                       (o) box  (o) lid        │(?) Room inside the box│
│                         │ └─x  10 mm    124 × 64 × 42.4 mm · 81 g solid    │    Does not meet the  │
│┌───────────────────────┐│──────────────────────────────────────────────────│    guess. Measure it  │
││ Ask for a change…  [↑]││ ●──(●)   v2 · Hotend · 2 min ago                │    first.            v│
│└───────────────────────┘│                                                  │(more below)           │
└─────────────────────────┴──────────────────────────────────────────────────┴───────────────────────┘
```

#### 4.3.1 Conversation

No bubbles. The maker's text sits in a `--app-subtle` block (radius 10, padding 8 x 12);
Hotend's text is plain body text on the panel. The label above each ("You", "Hotend") is a
caption in `--app-text-3`. Text streams in as it arrives; no typewriter effect is added.

Hotend's text is rendered from a safe subset of Markdown: paragraphs, line breaks, bold,
italic, lists and inline code. Headings render as bold body text. Links are shown as their
text and cannot be clicked, images are dropped, raw HTML is shown as text. Nothing in a
message can become a button.

**The work line.** While a run is going, one line under the streaming text shows what
Hotend is doing now, beside a 14 px spinner. It is replaced in place by the next step.
"Show steps (3)" under it lists the steps of this run that are finished. When the run ends
the line becomes a caption: "9 steps · 38k tokens" (the `tool_calls` and the sum of
`input_tokens` and `output_tokens` of the last `usage` event). The work line and its steps
belong to the session: after a restart only the text and the version markers remain.

| Event | Work line | Finished step |
| --- | --- | --- |
| `thinking`, or nothing yet | Thinking… | (not listed) |
| `tool_start` `write_model_code` | Writing the model: (the call's `note`, one line) | Wrote the model · v3 · or: The model did not build, trying again |
| `set_parameter` | Changing (the parameter's label)… | Set Height to 46 mm · v3 |
| `run_checks` | Checking it… | Checked it |
| `render_views` | Looking at it… | Looked at 2 views |
| `measure` | Measuring… | Measured it |
| `read_docs`, `list_cards`, `list_components` | Reading the manual… | (not listed) |
| `slice` | Slicing… | Sliced: 59 min, 28 g · or: Could not slice |
| `propose_print` | Getting a print ready for you to review… | (the card below) |

The finished step is written by the window from the tool's name, its `ok` flag and the
project as it is after the call (the new version, the stored slice). The tool's own `summary`
text is written for the AI and is not shown. A model that did not build on the first try is a
finished step, never a red banner: Hotend fixing its own mistake is normal.

**Version markers.** Every version appears in the conversation at the time it was made, as a
rule with a label and up to three lines:

- the label: "v2 · Hotend" for a version whose model was written (origin
  `write_model_code`), "v3 · Height set to 46 mm" for one that changed one size (origin
  `set_parameter`; the size is the parameter that differs from the parent). The core records
  the same origin whether the maker or the AI changed the size, so the label does not say
  who (14). Going back to an older version makes no version and no marker; the rail shows it
  (5.5);
- sizes that changed ("Lid height 12 → 14 mm");
- Proof lines that changed state ("Now passes: Walls thick enough, Wall of the box", "Now
  fails: Too big for the printer"); if neither changed: "Same sizes, same proof." The first
  version has nothing to compare with and shows its verdict in the words of a card's status
  line (4.2): "2 problems", "3 to measure", "Ready".

For a version the AI wrote, its `note` is shown in quotes as the first line. Clicking a
marker puts that version on the stage (5.5).

**A print the AI prepared.** After a `propose_print` call that worked, a card in the
conversation: "Hotend prepared a print for you to review." with the AI's note in quotes as
plain text and the button `Go to Print`. The card can start nothing.

**The composer.** A text area of one to eight lines fixed to the bottom of the column, 40 px
high when empty, with the send button (`arrow-up`) at its right. Enter sends, Shift+Enter
makes a new line. Placeholder: "Describe what you want to make" in an empty project, "Ask
for a change…" otherwise. While a run is going the field reads "Hotend is working" and the
send button is a Stop button (`square`, 6.4). While an older version is on the stage, a
caption above the field says "Messages change v2, the current version."

| Composer state | What shows |
| --- | --- |
| No AI key | Disabled, and above it: "Add an AI key to talk to Hotend." `Open Settings`. Sizes, proof, slicing and printing still work |
| The project was started with another provider | Disabled, and above it: "This project was started with Anthropic. Switch back to it in Settings to continue the conversation." `Open Settings` |
| A line with `remembered: false` | The line at 60 % opacity with the caption "Hotend will not remember this part of the conversation." |

#### 4.3.2 Stage furniture

All furniture is opaque `--app-raised` with a 1 px `--app-line` border and shadow 1. Nothing
over the 3D view is blurred or translucent.

- **Verdict bar**, top left (3.4).
- **View tools**, top right, three icon buttons in a toolbar, each with a tooltip that names
  it and its key: Fit (`scan`, `F`), Views (`box`: a menu of Corner, Front, Top, Right; keys
  `1` to `4`), Cut open (`scissors`, `C`). Cut open is a toggle; while it is on, a popover
  under the button holds a three-segment choice (Across, Along, From the top) and a slider
  that moves the cut. In Prepare and Print the tools are Fit and Views only.
- **Part chips**, bottom right: one chip per part with its tint as a 10 px dot and its name.
  A click hides or shows the part; a hidden part's chip is at 40 % with a struck-through
  name. Chips appear only when there are two or more parts.
- **Overall size**, under the chips: the bounding box of all parts and their solid weight
  from the build result, "124 × 64 × 42.4 mm · 81 g solid". Always the numbers of the version
  on the stage. The word "solid" is there because the slicer gives the real weight.
- **The viewer's own marker and scale**, bottom left: its small axis marker and the size of
  one grid square. These are the viewer's and stay where it puts them.
- **Version rail**, a 32 px strip under the 3D view (5.5).
- The project name in the title bar is a menu: **Export files…** (a popover with three
  checkboxes, STEP, 3MF and STL, all ticked, and `Choose a folder and export`; afterwards
  "3 files saved." and `Show in folder`) and **Show files** (opens the project's folder in
  the system's file manager).

#### 4.3.3 Inspector: Sizes and Proof

One scrolling column with two sections, SIZES and PROOF, in that order. Section 5 specifies
both: the rows of Sizes in 5.3 and 5.4, the lines of Proof in 5.2.

#### 4.3.4 States of the Design stage

| State | Stage | Conversation | Inspector |
| --- | --- | --- | --- |
| Empty: a new project, nothing sent | The empty plate at true size on the grid, with the centre text "Nothing here yet. Describe what you want to make." and the caption "Bambu Lab X2D · plate 256 × 256 mm · Simulated". No verdict bar, no tools, no rail | The three suggestions as quiet buttons above the focused composer | Hidden |
| Working, no version yet | The empty plate; verdict bar row 3 with Stop. No fake part and no skeleton solid | Text streams; the work line | Hidden until the first version |
| Working, a version exists | The current part stays. Each new version is swapped in when it arrives and the rail gains a dot | As above | Sizes disabled, with the tooltip "Hotend is working on this" |
| Rebuilding (a size change) | 6.1 | | The changed row shows a spinner in place of its tag; proof marks are spinners |
| Ready, or ready with notes | The part; verdict rows 10 or 11 | | Sizes, Proof |
| A problem | The part; verdict rows 7 or 8 | If the AI left a version with a problem, nothing extra: the verdict bar carries "Ask Hotend to fix it" | The failing lines first, open |
| A size could not be built | The last good part stays; verdict row 6 | | The row is back at its last good value and says why (6.1) |
| Measurements needed | The part; verdict row 9 | | "To measure" first (5.4) |
| An older version on the stage | That version; verdict row 5 | The caption above the composer | That version's sizes and proof, read-only |
| The run failed | Unchanged | An error card as the last item (4.3.5) | Unchanged |
| Opening a project | Stage background and grid at once; the part appears when its mesh is read, with no fade and no spinner in the centre | The stored text and markers at once | Sizes and Proof from the stored result at once |

```
┌────────────────────────────────────────────────────────────────────────────────────────────────────┐
│ [house] [<|]  A box that hides v   ( Design )   Prepare   Print     [flask] X2D Simulated [|>] [*] │
├─────────────────────────┬──────────────────────────────────────────────────────────────────────────┤
│ [ A box that hides a   ]│                                                                          │
│ [ power strip…         ]│                                                                          │
│ [ A wall hook for a 32 ]│                                                                          │
│ [ mm pipe…             ]│                                                                          │
│ [ A drawer divider for ]│               ┌ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ┐                    │
│ [ cutlery…             ]│                                                                          │
│                         │               │   Nothing here yet. Describe what   │                    │
│                         │                   you want to make.                                      │
│                         │               │                                     │                    │
│                         │                                                                          │
│                         │               └ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ┘                    │
│┌───────────────────────┐│                                                                          │
││ Describe what you     ││ ╵z                                                                       │
││ want to make       [↑]││ └─x  50 mm            Bambu Lab X2D · plate 256 × 256 mm · Simulated     │
│└───────────────────────┘│                                                                          │
└─────────────────────────┴──────────────────────────────────────────────────────────────────────────┘
```

```
┌────────────────────────────────────────────────────────────────────────────────────────────────────┐
│ [house] [<|]  Cable box v         ( Design ◌ )   Prepare   Print    [flask] X2D Simulated [|>] [*] │
├─────────────────────────┬──────────────────────────────────────────────────┬───────────────────────┤
│You                      │ ┌────────────────────────────────────────────┐   │SIZES                  │
│ Make it 6 mm taller and │ │ (..) Hotend is working: writing  [ Stop ] │   │To measure             │
│ round the corners       │ └────────────────────────────────────────────┘   │(?) Inside width 120 mm│
│Hotend                  │                      [scan] [box] [scissors]     │(?) Inside depth  60 mm│
│ I will raise the box to │                                                  │(?) Cable slot    12 mm│
│ 46 mm and round the     │               ┌───────────┐                      │Your choices           │
│ four outer corners.▍    │              ╱           ╱│                      │Height            40 mm│
│                         │             ┌───────────┐ │                      │├────○────────────────┤│
│(..) Writing the model:  │             │   (v2)    │╱                       │Wall thickness     2 mm│
│     taller box, round   │             └───────────┘                        │├─────○───────────────┤│
│     corners             │                                                  │(tooltip: Hotend is   │
│     Show steps (3)  >   │                                                  │ working on this)      │
│                         │                                                  │───────────────────────│
│                         │                                                  │PROOF         10 of 15 │
│                         │                                                  │What it must do 2 of 5 │
│                         │ ╵z                       (o) box  (o) lid        │How parts fit   3 of 4 │
│                         │ └─x  10 mm    124 × 64 × 42.4 mm · 81 g solid    │How it prints   5 of 6 │
│┌───────────────────────┐│──────────────────────────────────────────────────│                       │
││ Hotend is working [■]││ ●──(●)   v2 · Hotend · 9 min ago                │                       │
│└───────────────────────┘│                                                  │                       │
└─────────────────────────┴──────────────────────────────────────────────────┴───────────────────────┘
```

#### 4.3.5 When a run fails

One card as the last item of the conversation, on `--app-fail-wash`: what happened, what to
do, one button. It stays until the next message. It is never a dialog and never a toast.

```
┌────────────────────────────────────────────┐
│(x) There is no AI key for Anthropic.       │
│    Add one, then send the message again.   │
│    Nothing was lost.                       │
│    [ Open Settings ]                       │
└────────────────────────────────────────────┘
```

| Cause (how the window knows) | Text | Button |
| --- | --- | --- |
| No key (`no_credential`) | There is no AI key for Anthropic. Add one, then send the message again. Nothing was lost. | Open Settings |
| Another message is still running (`busy`) | Hotend is still working on the last message. | Stop it |
| The step limit (`stop_reason: budget`) | Hotend used all 24 steps for this message and stopped at v2. | Continue (sends "Continue.") |
| The run's own limits (`cap_exceeded`, `timeout`) | This message reached Hotend's limit for one run. The part is at v2. | Continue |
| The AI kept asking for things it may not do (`too_many_refusals`) | Hotend stopped this run because the AI kept asking for things it is not allowed to do. | Try again |
| The sandbox cannot be applied (`sandbox_unavailable`) | Hotend cannot run AI-written models safely on this computer, so it did not run this one. | Open This computer |
| Stopped by the maker (`stop_reason: cancelled`) | No card. A line: "Stopped. v2 is still here." (or "Stopped. Nothing was made.") | |
| Anything else (`stop_reason: error` with its text, or a `failed` event) | Hotend could not finish this message. (then the core's own sentence, at most 300 characters) Your project is unchanged. | Try again |

Under every card, as a caption in monospace: the code (`agent_error`, `upstream_error`, …).
"Try again" sends the maker's last message again. The window does not guess why a provider
refused: it shows the sentence the core gives.

### 4.4 Studio: Prepare

Entering Prepare lays the current version out on plates (`prepare_run`) unless a layout for
this version, printer and material is already stored. The stage shows the plate at true size
with the pieces lying as they will be printed, seen from the corner. The parts keep their
design tints.

```
┌────────────────────────────────────────────────────────────────────────────────────────────────────┐
│ [house] [<|]  Cable box v        Design !   ( Prepare ✓ )   Print   [flask] X2D Simulated [|>] [*] │
├─────────────────────────┬──────────────────────────────────────────────────┬───────────────────────┤
│You                      │ ┌──────────────────────────────────────────────┐ │PLATE 1 of 1           │
│ A box that hides a power│ │ (ok) Sliced: 59 min · 28 g  [ Review print ] │ │box          [-] 1 [+] │
│ strip, with a snap-on   │ └──────────────────────────────────────────────┘ │ As modelled  [ Turn… ]│
│ lid                     │                               [scan] [box]       │ (!) About 48 mm2 needs│
│Hotend                  │                                                  │     support…          │
│ Here is the plan: a box │     ┌──────────────────────────────────┐         │lid          [-] 1 [+] │
│ and a lid that snaps on…│     │                                  │         │ Upside down  [ Turn… ]│
│── v1 · Hotend ─────────│     │  ┌───────────┐   ┌───────────┐   │         │ (!) Its snap rails    │
│ (x) 2 problems          │     │  │    box    │   │    lid    │   │         │     stand upright…    │
│── v2 · Hotend ─────────│     │  └───────────┘   └───────────┘   │         │───────────────────────│
│ Now passes: Walls thick │     │                                  │         │MATERIAL               │
│ enough, Wall of the box │     │                                  │         │PLA Basic · main nozzle│
│ It is 124 × 64 × 42.4 mm│     └──────────────────────────────────┘         │Change in Settings     │
│ outside. The inside     │                                                  │───────────────────────│
│ sizes are my guesses.   │                                                  │SLICE                  │
│ 9 steps · 38k tokens    │                                                  │Quality [Standard   v] │
│                         │ ╵z                       (o) box  (o) lid        │59 min                 │
│┌───────────────────────┐│ └─x  50 mm     Plate 256 × 256 mm · X2D Simulated│28 g · 200 layers      │
││ Ask for a change…  [↑]││──────────────────────────────────────────────────│[flask] Stand-in slicer│
│└───────────────────────┘│ ●──(●)   v2 · Hotend · 2 min ago                │(!) Generic profile    │
└─────────────────────────┴──────────────────────────────────────────────────┴───────────────────────┘
```

**Plate.** "PLATE 1 of 1". One row per part: its name, a stepper for copies (1 to 64), how
it lies in the engine's words (`turned`: "As modelled", "Upside down"), and `Turn…`. Under
it, as a `warn` line with the engine's sentence, each warning of the layout that names the
part (`preparation.checks`); a warning that names no part goes under the last row. The
engine's longer reason for the way a part lies (`explanation`) is the tooltip of the row and
the first line of its Turn popover. With several plates, a
row of small buttons "1 · 2 · 3" beside the section label chooses the plate on the stage;
each plate is sliced and printed on its own.

`Turn…` opens a popover anchored to the row: the ways the part can lie (the engine's
candidates whose up direction is one of the six axis directions), best first, as radio rows
in plain numbers: the engine's `turned`, the support it needs (`overhang`), what lies on the
bed (`contact`) and the height. Choosing one lays the plate out again.

```
┌──────────────────────────────────────────────┐
│How should the lid lie on the plate?          │
│(o) Upside down · no support · 7 918 mm² on   │
│    the bed · 10.4 mm tall                    │
│( ) Onto its -X side · support under 525 mm²  │
│    · 139 mm² on the bed · 124 mm tall        │
│    (three more ways, then:)                  │
│( ) As modelled · support under 7 630 mm² ·   │
│    288 mm² on the bed · 10.4 mm tall         │
└──────────────────────────────────────────────┘
```

**Material.** Read-only: the material from Settings and "main nozzle", with the link "Change
in Settings". In this milestone every part prints on the main nozzle, and the core chooses
the spool at review.

**Slice.** A Quality menu with two entries, "Standard · 0.20 mm" (profile `demo-0.20mm`) and
"Fine · 0.12 mm" (`demo-0.12mm`). After a slice: the time in `numeric-lg`, then grams and
layers, then the name and version of the slicer as a caption, then one tag per warning.
There is no Slice button in the inspector: the verdict bar carries it.

| Warning code | Tag | Colour |
| --- | --- | --- |
| `fake_slicer` | Stand-in slicer: rough numbers, no printable file | `sim` |
| `demo_profile` | Generic profile, not tuned for this printer | `warn` |
| `slicer_version` | This slicer version has not been tested with Hotend | `warn` |
| `reach_unchecked` | The slicer did not confirm the nozzle's reach | `warn` |
| `unverified_printer_facts` | X2D limits not confirmed on a real printer (the core's `message` is the tooltip) | `warn` |
| `time_unknown`, `layers_unknown`, `filament_use_unknown` | The number is replaced by "not stated by the slicer"; nothing is estimated | |
| any other | The core's own `message` | `warn` |

| State | What shows |
| --- | --- |
| Laying out | Verdict "Laying out the plate"; the pieces stay where they were until the layout arrives |
| A piece fits no plate | The piece is listed under "Does not fit" with the engine's reason and `Turn…`; it is not drawn |
| Not sliced | The Slice section holds the Quality menu and "Not sliced yet." |
| Slicing | Verdict "Slicing plate 1"; a 2 px line in the heat colours runs along the top edge of the stage; the summary area keeps its height so nothing jumps. A slice cannot be stopped in this milestone |
| Sliced | The summary as drawn |
| Changed since the slice (copies, a turn, the quality, or the material in Settings) | The summary stays at 60 % opacity under "Changed since this slice." |
| Sliced before Hotend-made plate previews were switched on (the slice record's `job.previews` is not `hotend_made` and it has no `hotend_made_previews` warning) | The summary under "Sliced before Hotend drew plate previews. Slice again to hand it to Bambu Connect." |
| No slicer can run (`not_configured`, `slicer_not_installed`) | "No slicer is set up, so there is no time or weight yet." |
| The slice failed | One sentence by code, with the code as a monospace caption: `slicer_timeout` "The slicer took too long and was stopped."; `out_of_reach` "A piece lies outside the nozzle's reach."; `material_not_allowed` "This material cannot be printed on that nozzle."; `slicer_version_refused` "This slicer version is too old to be run safely."; any other: the core's sentence |
| The version has a problem | The part as in Design, an empty inspector with "Prepare opens when the part has no problems." |

### 4.5 Studio: Print, review

Entering Print with a current slice and no running job makes the window ask the core to
check the sliced file (`print_propose` when no pending job for this slice exists, then
`print_review`). The inspector shows what the core read from the file: the same facts the
confirmation dialog will show. A print the AI proposed arrives here exactly like one the
maker prepared, with the AI's note quoted above the review as plain text; nothing is
preselected for it. The window keeps at most one pending job per project: it withdraws an
older one when the slice has changed.

```
┌────────────────────────────────────────────────────────────────────────────────────────────────────┐
│ [house] [<|]  Cable box v       Design !   Prepare ✓   ( Print ! )  [flask] X2D Simulated [|>] [*] │
├─────────────────────────┬──────────────────────────────────────────────────┬───────────────────────┤
│You                      │ ┌───────────────────────────────────┐            │REVIEW                 │
│ A box that hides a power│ │ (?) Check the numbers, then start │            │Cable box · v2, plate 1│
│ strip, with a snap-on   │ └───────────────────────────────────┘            │59 min · 200 layers    │
│ lid                     │                               [scan] [box]       │28.3 g PLA, main nozzle│
│Hotend                  │                                                  │AMS 1, slot 1:         │
│ Here is the plan: a box │     ┌──────────────────────────────────┐         │[#] PLA Basic          │
│ and a lid that snaps on…│     │                                  │         │Hottest: not stated    │
│── v1 · Hotend ─────────│     │  ┌───────────┐   ┌───────────┐   │         │(!) the G-code contains│
│ (x) 2 problems          │     │  │    box    │   │    lid    │   │         │    no moves; nothing  │
│── v2 · Hotend ─────────│     │  └───────────┘   └───────────┘   │         │    would be printed   │
│ Now passes: Walls thick │     │                                  │         │Read from the sliced   │
│ enough, Wall of the box │     │                                  │         │file.                  │
│ It is 124 × 64 × 42.4 mm│     └──────────────────────────────────┘         │───────────────────────│
│ outside. The inside     │                                                  │SEND TO                │
│ sizes are my guesses.   │                                                  │(o) X2D · Simulated    │
│ 9 steps · 38k tokens    │                                                  │    Nothing is made.   │
│                         │                                                  │( ) Bambu Connect      │
│                         │                                                  │( ) A folder or USB    │
│                         │                                                  │    drive              │
│                         │ ╵z                       (o) box  (o) lid        │(?) 3 numbers are still│
│┌───────────────────────┐│ └─x  50 mm     Plate 256 × 256 mm · X2D Simulated│    guesses: Inside…   │
││ Ask for a change…  [↑]││──────────────────────────────────────────────────│[ Start print… ]       │
│└───────────────────────┘│ ●──(●)   v2 · Hotend · 2 min ago                │                       │
└─────────────────────────┴──────────────────────────────────────────────────┴───────────────────────┘
```

**Review.** Project, version and plate; time and layers; per filament its grams, material
and nozzle, and the spool the printer will use ("AMS 1, slot 1", a swatch in the spool's
colour, and the product's name, "PLA Basic"; the core counts units and slots from 0, the
window and the dialog show them from 1; a colour has no name, only its swatch); the
hottest nozzle and bed temperature in the file; the caption "Read from the sliced file.";
then any warning of the file check as a `warn` line in the core's words. A value the file
does not state reads "not stated": a file from the stand-in slicer states no temperature and
carries the warning that it contains no moves. Grams here and in the dialog keep one
decimal, as the core read them.

**Send to.** One radio row per way the build can deliver (`printers_list`, the entries with
`enabled`), with the first sentence of what it cannot do:

| Choice | Under it | Button |
| --- | --- | --- |
| X2D · `sim` tag "Simulated" | A simulated print. Nothing is made. | Start print… |
| Bambu Connect | Hotend saves the file. You open it there and start it; Hotend does not control the print. | Save for Bambu Connect… |
| A folder or USB drive | (after choosing) the folder's path · `Choose a folder…` | Save file… |

The button is the one filled button of this screen and sits at the bottom of the
inspector, under the numbers. It has no keyboard shortcut. The ellipsis is a promise that
nothing starts from this click: it opens the confirmation of 4.6. A file from the stand-in
slicer can go to the simulated printer only; the other two rows are disabled with "The
stand-in slicer makes no printable file."

Blocked reviews replace the Review section and disable the button:

```
┌────────────────────────────────────────────┐
│REVIEW                                      │
│(x) Hotend stopped this file before it     │
│    reached the printer.                    │
│    Line 18 204: a move goes outside the    │
│    main nozzle's reach.                    │
│    gcode_rejected · move_out_of_reach      │
│    [ Back to Prepare ]                     │
└────────────────────────────────────────────┘
```

| Cause (`code`) | Text | Button |
| --- | --- | --- |
| `gcode_rejected` | Hotend stopped this file before it reached the printer. (then the first finding: its line number and message) | Back to Prepare |
| `no_spool` | The job needs PLA on the main nozzle. No loaded spool matches. | Back to Prepare |
| `slice_changed` | The sliced file has changed since it was made. Slice it again. | Back to Prepare |
| `plan_changed` | The print settings of this plate changed since it was sliced. Slice it again. | Back to Prepare |
| `not_importable` with `reason` `slice_before_previews` | (the core's sentence: "This slice was made before Hotend drew plate previews. Slice again, then hand it off. ...", then its next step) | Slice again (slices here, as Prepare does) |
| `delivery_failed`, `unsupported`, any other `not_importable` | (the core's sentence) | Review again |

### 4.6 The confirmation

The confirmation is a system message dialog drawn by the shell, not part of the web page: the
page cannot draw over it, read it or press it, and the AI cannot reach it. The window's job
is to make it expected. The shell builds its text from the review's summary, one fact per
line because a system dialog has no table:

```
Start this print?

SIMULATED PRINTER. Nothing will be printed.

Project: Cable box, version 2, plate 1
Printer: Bambu Lab X2D (simulated), serial ending 0001
Time: 59 min, 200 layers
Filament: PLA, 28.3 g, main nozzle, AMS 1 slot 1 (PLA Basic)
Hottest: nozzle not stated, bed not stated
File: plate_1.gcode.3mf (b8a6e6…bcf3)

Warning: the G-code contains no moves; nothing would be printed

These numbers were read from the sliced file.

                                              [ Cancel ]   [ Start print ]
```

- The frame shows the dialog for the recorded example: a file from the stand-in slicer,
  which states no temperature and carries one warning. The second line appears only for the
  simulated printer. Warnings of the file check, if any, are listed above the last line,
  each as "Warning: " and the core's sentence.
- For Bambu Connect the title is "Save this file for Bambu Connect?" and the button "Save" (Hotend saves the file; it does not open Bambu Connect);
  for a folder, "Save this file?" and "Save". Both name the folder instead of the printer.
- Cancel is the default button and answers Enter and Esc. The confirming button has no
  shortcut and is never the default.
- While the dialog is open the inspector is covered by `--app-scrim` and the verdict bar
  reads "Waiting for your answer in the Hotend dialog". The stage stays clear.
- A confirmation the core refuses (the file changed, the printer is busy, two minutes passed)
  starts nothing: the Review section shows the core's sentence and the button again. For the
  time limit: "That took longer than two minutes, so nothing was started. Review it again."

### 4.7 Studio: Print, the running job

```
┌────────────────────────────────────────────────────────────────────────────────────────────────────┐
│ [house] [<|]  Cable box v       Design !   Prepare ✓   ( Print ◌ )  [flask] X2D Simulated [|>] [*] │
├─────────────────────────┬──────────────────────────────────────────────────┬───────────────────────┤
│You                      │ [flask] Simulated printer: nothing is being      │PRINTING               │
│ A box that hides a power│         printed                                  │42 %                   │
│ strip, with a snap-on   │ ┌─────────────────────────────────┐              │━━━━━━━━━━─────────────│
│ lid                     │ │ (..) Printing · layer 84 of 200 │              │About 34 min left      │
│Hotend                  │ └─────────────────────────────────┘              │Layer 84 of 200        │
│ Here is the plan: a box │                               [scan] [box]       │Bed 55 °C              │
│ and a lid that snaps on…│     ┌──────────────────────────────────┐         │[ Stop ]               │
│── v1 · Hotend ─────────│     │                                  │         │───────────────────────│
│ (x) 2 problems          │     │  ┌───────────┐   ┌───────────┐   │         │AMS 1 · main nozzle    │
│── v2 · Hotend ─────────│     │  │ box, cut  │   │ lid, cut  │   │         │1 [#] PLA Basic    80 %│
│ Now passes: Walls thick │     │  └───────────┘   └───────────┘   │         │2 [#] PLA Basic    55 %│
│ enough, Wall of the box │     │                                  │         │3 [#] PLA Matte    30 %│
│ It is 124 × 64 × 42.4 mm│     └──────────────────────────────────┘         │4 [#] PLA Basic    95 %│
│ outside. The inside     │                                                  │AMS HT · auxiliary     │
│ sizes are my guesses.   │ Drawn from the printer's layer count. There      │1 [#] PETG HF      70 %│
│ 9 steps · 38k tokens    │ is no camera in this version.                    │───────────────────────│
│                         │ └─x  50 mm     Plate 256 × 256 mm · X2D Simulated│Cable box · v2, plate 1│
│┌───────────────────────┐│──────────────────────────────────────────────────│28.3 g PLA             │
││ Ask for a change…  [↑]││ ●──(●)   v2 · Hotend · 2 min ago                │                       │
│└───────────────────────┘│                                                  │                       │
└─────────────────────────┴──────────────────────────────────────────────────┴───────────────────────┘
```

The part is still the hero. The 3D view is cut by a horizontal plane at `layer / total_layers`
of the plate's height (at the progress percentage when the printer gives no layer), so the
part grows as it prints. The caption under it says where the picture comes from. A band in
`sim` on `--app-sim-wash` across the top of the stage says "Simulated printer: nothing is being
printed" for as long as the job is on the simulated printer.

The inspector shows the percentage in `numeric-lg`, a progress bar, the time left as the
printer reports it, the layer, and the bed temperature (the only temperature the core
reports in this milestone); then the printer's AMS units and trays; then what is being
printed.

| Job state | Verdict bar | Inspector |
| --- | --- | --- |
| `starting` | `(..)` Waiting for the printer to start · 0:42 | "Hotend sent the job. A print counts as started only when the printer says so." · `Stop` |
| `printing`, phase `preparing` or `heating` | `(..)` Getting ready: heating | The bed temperature as it rises |
| `printing` | `(..)` Printing · layer 84 of 200 | As drawn |
| `finished` | `(ok)` Finished. | What was printed · `Print again` · `Back to Design`. No duration is shown: the core does not report one, and the simulated printer runs sixty times faster than its clock |
| `not_started` | `(x)` The printer accepted the job but did not start. | The core's message (for a drying AMS: how long is left) · `Review again` |
| `failed` | `(x)` The printer stopped. | Each problem's sentence, with the printer's own code as a monospace caption (`HMS_0300_0100_0001_0007`) · `Review again` |
| `cancelled` | none · Stopped. Nothing more will be printed. | `Review again` |
| `exported` | `(ok)` Saved to the folder. | The file's path · `Show in folder` |
| `handed_off` | `(ok)` Saved for Bambu Connect. You open it and start it there. | "Hotend does not open Bambu Connect itself. Open this file in it:" with the path, then "Hotend cannot follow this print." |
| phase `offline` | `(!)` Lost contact with the printer. | "The print may still be running." The last values at 60 % |

**Stop** is the only control of a running print. One click turns the button, in place, into
"Stop for good?" with `Keep printing` and `Stop` (`fail` fill) for four seconds; then it
turns back. Two clicks without moving the pointer, no dialog, no shortcut.

**The printer chip** in the title bar shows the printer and its tag on every screen:
"X2D · Simulated", and while a job runs "X2D · Printing 42 %". It opens a popover:

```
┌────────────────────────────────────────┐
│Bambu Lab X2D          [flask] Simulated│
│Nothing is printed on this printer.     │
│Printing · 42 % · about 34 min left     │
│Cable box, plate 1                      │
│────────────────────────────────────────│
│AMS 1 · main nozzle                     │
│1 [#] PLA Basic                   80 %  │
│2 [#] PLA Basic                   55 %  │
│3 [#] PLA Matte                   30 %  │
│4 [#] PLA Basic                   95 %  │
│AMS HT · auxiliary nozzle               │
│1 [#] PETG HF                     70 %  │
│────────────────────────────────────────│
│[ Go to the print ]                     │
└────────────────────────────────────────┘
```

With no job the third and fourth lines read "Idle". `Go to the print` opens the Print stage of
the project that is printing.

### 4.8 Settings

A page: a section list on the left (220 px), one column of content (at most 640 px), and a
Back button (`arrow-left`) that returns to where the maker was. Changes apply at once; there
is no Save button except for the key. `Mod+,` opens it.

```
┌────────────────────────────────────────────────────────────────────────────────────────────────────┐
│ [arrow-left] Settings                                                                              │
├──────────────────────┬─────────────────────────────────────────────────────────────────────────────┤
│                      │                                                                             │
│ AI model             │ AI model                                                                    │
│ Printer and material │ Provider   ( Anthropic )  OpenAI   Google   OpenRouter   Ollama             │
│ Slicer               │ Model      [ model id                                        v ]            │
│ Appearance           │ API key    Key saved · in this computer's keychain   [ Replace ] [ Remove ] │
│ This computer        │            Hotend's AI never sees this key. Hotend adds it to each request│
│ Shortcuts            │            on the way out.                                                  │
│ About                │ Work limit [-] 24 [+]  steps per message                                    │
│                      │                                                                             │
│                      │ Hotend sends your messages, the model's code, what Hotend's tools answer    │
│                      │ (checks, measurements, print settings, slices, what the code prints) and    │
│                      │ pictures of the part to Anthropic, using your key. With web research on,    │
│                      │ search words and page addresses also go to the search service you chose. If │
│                      │ you set up a printer, its status is read over your network.                 │
└──────────────────────┴─────────────────────────────────────────────────────────────────────────────┘
```

| Section | Content |
| --- | --- |
| AI model | The AI section of 4.1, plus Work limit: a stepper, 1 to 200 (`max_turns`), "steps per message"; and the sentence about what leaves the computer. Changing the provider while a project was started with another says so before the change: "Projects started with Anthropic cannot continue their conversation with another provider." |
| Printer and material | The printer card: name, "Simulated", "Everything up to the print is real. The print is not.", its state and its AMS trays as in the chip's popover, and the line "X2D limits have not been confirmed on a real printer yet." Then Material: a menu of PLA Basic, PETG Basic and TPU 95A; under it "Parts are checked for this material the next time they are built." Then "Ways to send a print": each delivery with what it can and cannot do, as the core words it. There is no "Add printer" |
| Slicer | One row per slicer the core knows (`slicers_list`): its name, version and where it was found; the verdict in words ("Works", "Not tested with Hotend", "Too old to run safely", "Not found"); the core's reason and fix; `Choose program…`. Then the switch "Use the stand-in slicer" with "Rough time and weight, no printable file. For trying Hotend without a slicer." |
| Appearance | Theme: Dark, Light, System. Nothing else |
| This computer | The four rows of 4.1 with "Details", `Check again`, and `Copy report` (the report as JSON; the button reads "Copied" for two seconds) |
| Shortcuts | The table of 6.3 |
| About | "Hotend 0.1.0", the data folder's path with `Show files` |


## 5. Proof, sizes and versions

Everything in this section is drawn from the engine result of the version on the stage
(`version_get`): `params`, `param_specs`, `parts`, `checks`, `questions`. The window never
judges geometry itself.

### 5.1 The verdict

Three counts decide the verdict (3.4), the Design mark (3.3) and the status line of a card
(4.2):

- **problems**: the Proof lines (5.2) whose status is fail;
- **numbers to measure**: the parameters whose provenance is `guessed`, plus each Proof line
  of a requirement that rests on a guess, names no parameter and is not a fit (5.2);
- **notes**: every other Proof line whose status is warn. A requirement whose guess is
  already counted through its parameters is not counted again.

Worst first: problems, then numbers to measure, then notes, then "Ready to print". The words are
"checked", "passes" and "as drawn for this printer", never "guaranteed", "certified" or
"safe".

### 5.2 The Proof list

Header: "PROOF" and a count, "10 of 15": the lines that pass out of the lines shown. Each
group's header carries its own count the same way ("2 of 5"). The engine returns one result
per check per part (38 for the two-part cable box); the list shows one line per kind of
check, in three groups, failing lines first.

| Group | Lines | From |
| --- | --- | --- |
| What it must do | One per requirement of the model's contract | the results of `contract`, one per requirement |
| How parts fit | "Parts do not collide", then one per joint, then one per other fit finding | `parts_do_not_intersect`; the results of `fit_pairs` |
| How it prints | Solid, in one piece · Fits the printer · Walls thick enough · Prints without support · Sits flat on the bed · No detail too fine | `valid_solid`, `manifold` and `single_solid_per_part` as one line; `fits_build_volume`; `min_wall`; `overhangs`; `bed_contact`; `small_features` |

A line's status is the worst status of the results behind it. Titles are the window's, by
check and status; the sentences inside a line are the engine's own `message`, word for word.

| Results | Title when it passes | Title when it warns or fails |
| --- | --- | --- |
| `valid_solid`, `manifold`, `single_solid_per_part` | Solid, in one piece | Not a printable solid |
| `fits_build_volume` | Fits the printer | Too big for the printer |
| `min_wall` | Walls thick enough | Wall too thin |
| `overhangs` | Prints without support | Needs support |
| `bed_contact` | Sits flat on the bed | Little contact with the bed |
| `small_features` | No detail too fine | Detail too fine to print |
| `parts_do_not_intersect` | Parts do not collide | Parts collide |
| `fit_pairs` with `data.instance` (the pairs and the joint of one card, grouped) | (Card): goes together, gaps as drawn | (Card): a gap is off · or, when the joint result fails: (Card): cannot be put together |
| `fit_pairs`, `data.kind` `robustness` | | Fits not proven on this printer |
| `fit_pairs`, `data.kind` `unverified` | | A clearance that nothing verifies |
| `fit_pairs`, `data.kind` `declaration` | | A declared fit could not be checked |
| `fit_pairs`, `data.kind` `undeclared` | | A joint made by hand, not checked |
| `contract`, `requirement_kind` `cavity_min` | Room inside the (part) | the same title |
| `contract`, `outer_max` | Outer size of the (part) | the same title |
| `contract`, `wall_min` | Wall of the (part) | the same title |
| `contract`, `fit` | Fit between the (part) and the (part), with the card's kind in brackets when `data.cards` names one: "Fit between the box and the lid (snap lid)" | the same title |
| `contract`, `dimension` | A named size of the (part) | the same title |
| any other result | | The first sentence of its `message`, at most 60 characters |

"(Card)" is the card's kind in words: `snap_lid` is "Snap lid"; a second card of the same
kind is "Snap lid 2". "(part)" is the part's name as the model gives it.

**Line anatomy.** 28 px: status icon, title, a chevron. A passing line is closed; a group
whose lines all pass collapses to its header row ("How it prints · 6 of 6") and opens on a
click. A line that warns or fails is open and shows, per result behind it, the
part's name and the engine's sentence. A failing line adds two buttons:

```
┌────────────────────────────────────────────┐
│(x) Wall too thin                        v  │
│    box: A wall is only 0.60 mm thick near  │
│    x=-5.2, y=-30.4, z=32.7; the minimum is │
│    0.80 mm. About 175.2 mm2 of surface is  │
│    too thin.                               │
│    [ Ask Hotend to fix it ]  [ Show me ]  │
└────────────────────────────────────────────┘
```

- `Ask Hotend to fix it` sends this message, visible in the conversation as the maker's:
  "Fix this: (the engine's sentence)". It is disabled without a key or while a run is going.
- `Show me` hides the other parts and fits the named part in view; pressing it again shows
  all parts. It is offered on results that name a part. Lighting the exact faces is Later.

**Requirements that rest on a guess.** The engine never passes them: the result warns and
carries a `question`. Under its title the line shows one fixed sentence chosen from the
data, then the engine's sentence.

| The result | Icon | Fixed sentence | Counts as |
| --- | --- | --- | --- |
| The question names guessed parameters, `met` is true | `(?)` | Meets the guess. Not proven until you measure it. | Nothing more: its parameters are already in "To measure" |
| The same, `met` is false | `(?)` | Does not meet the guess. Measure it first. | The same |
| The same, `met` is null | `(?)` | Hotend cannot measure this on the part. | The same |
| No guessed parameter, `requirement_kind` is `fit` | `(!)` | Goes together as drawn. Not proven on this printer. | A note |
| No guessed parameter, any other kind | `(?)` | As the first three rows, by `met` | One number to measure; the line carries `Tell Hotend what you measured` (5.4) |

Under the fixed sentence of the first three rows, each guessed parameter is named as a quiet
link ("Inside width") that opens its row in Sizes.

**The recorded example.** The cable box of `example_open`, as the core builds it today, gives
these fifteen lines (38 results: 32 pass, 5 warn, 1 skipped), so "PROOF 10 of 15", no
problem, three numbers to measure (Inside width, Inside depth, Cable slot) and three notes:

| Group | Lines |
| --- | --- |
| What it must do · 2 of 5 | `(?)` Room inside the box (does not meet the guess) · `(?)` A named size of the box (cannot be measured) · `(!)` Fit between the box and the lid (snap lid) · `(ok)` Fit between the box and the lid (screw boss) · `(ok)` Wall of the box |
| How parts fit · 3 of 4 | `(!)` Fits not proven on this printer · `(ok)` Parts do not collide · `(ok)` Snap lid: goes together, gaps as drawn · `(ok)` Screw post: goes together, gaps as drawn |
| How it prints · 5 of 6 | `(!)` Needs support (the box, 48 mm² on 2 faces) · `(ok)` the other five |

**Fits on an uncalibrated printer.** "Fits not proven on this printer" is a note. Under the
engine's sentence the window adds: "Hotend uses general values for fits until your printer
is calibrated. Calibration comes in a later version; until then, print one and try the fit."
There is no link, because there is nothing to open.

**No contract.** When the model declares none, the first group holds one grey line, "Hotend
has not written down what this product must do.", and the quiet button `Ask Hotend to write
it down`, which fills the composer with "Write down what this product must do." and does not
send.

**Skipped checks** are listed last in their group in `--app-text-3` with the engine's reason,
and do not count. A group whose results are all skipped is left out ("How parts fit" for a
model of one part). `parameter_ranges` is never listed: the window never asks for it.

**While a rebuild runs** every status icon is a spinner and the header reads "PROOF …".

### 5.3 Sizes

Header: "SIZES". One row per parameter of the model, grouped by where its number comes
from, because that decides what the maker may do with it. Inside a group the model's order
is kept. Labels are the model's `label`.

| Group | Provenance | Tag under the row | Control |
| --- | --- | --- | --- |
| To measure | `guessed` | `circle-question-mark` "Hotend's guess", in `warn` on `--app-warn-wash` | A number field and `Use my measurement` (5.4). No slider: a measurement is typed, not dragged |
| Your choices | `style` | The range: "26 to 80" in `--app-text-3` | A slider with a number field when the model gives `min` and `max`; a number field alone when it does not. A `count` is a stepper |
| Fixed (closed, with a count: "Fixed · 2") | `measured` | `ruler` "You measured" | The value and `Change`, which turns it into a number field for one edit |
| | `standard` | `book-marked` "Standard part" | Read-only |
| | `calibrated` | `gauge` "From your printer" | Read-only |

An empty group is not shown. Only `guessed` is coloured.

**Row anatomy** (56 px for a slider row): the label on the left and the value with its unit
on the right in `ui` weight 500; under them the track; under the track the tag. The slider
track is 4 px in `--app-control`, the filled part in `--app-accent-fill`, the thumb 16 px,
`--app-on-fill` with a 1 px `--app-control` border. A hollow tick on the track marks the value of the
version on the stage.

**Numbers.** Millimetres with at most two decimals and no trailing zeros ("120 mm", "1.6
mm"); clearances always with two ("0.20 mm"); degrees whole with "°"; a `ratio` with two
decimals and no unit; a `count` whole. The decimal mark is a point; thousands are separated
by a thin space ("7 918 mm²"). Sizes are joined with "×". Time is "1 h 42 min", "59 min" or
"55 s", rounded to the nearest minute from one minute up. Weight is whole grams from 10 g
and one decimal under it; the review and the dialog keep one decimal (4.5). Units are always
written. Every number uses tabular figures.

### 5.4 A guessed number is a question

A guess is never shown as a fact with a caveat. Each guessed number is a row of the first
group of Sizes, and one row is open at a time:

```
┌────────────────────────────────────────────┐
│To measure                                  │
│(?) Inside width           Hotend's guess  │
│    [ 120 ] mm   [ Use my measurement ]     │
│(?) Inside depth                     60 mm  │
│(?) Cable slot                       12 mm  │
│Measure it and type what you read. Hotend  │
│rebuilds the part around it.                │
└────────────────────────────────────────────┘
```

- A closed row shows `(?)`, the label and the value. A click, Enter on the row or the verdict
  bar's `Measure them` opens it and closes the one that was open. The first row is open when
  the project opens.
- The open row shows the tag "Hotend's guess", a field holding the guess (selected when it
  gets the caret, steps of 0.1 mm), the unit and `Use my measurement`. Enter in the field is
  the same as the button. Under the group, once: "Measure it and type what you read. Hotend
  rebuilds the part around it."
- `Use my measurement` sends this message, visible in the conversation as the maker's: "I
  measured (label): (value) (unit). Use this value and mark it as measured." Hotend
  rebuilds the part; when the new version arrives the row moves to "Fixed" as "You
  measured", the requirement in Proof is judged for real, and the verdict moves on. In this
  milestone this takes an AI turn, so the button is disabled without a key or while a run is
  going, with the reason as its tooltip (14 has the command that removes the AI turn).
- A question that names no parameter has no row. Its Proof line carries the quiet button
  `Tell Hotend what you measured`, which puts "I measured " in the composer and the caret
  after it.
- Nothing here locks anything. A part whose numbers are still guesses can be prepared and
  printed; the verdict bar offers "Prepare anyway", and the Review section of 4.5 repeats,
  above its button: "3 numbers are still guesses: Inside width, Inside depth, Cable slot."
  It is the maker's product and their filament.

### 5.5 Versions: the rail, the comparison, undo

**The rail** is a 32 px strip under the 3D view, on `--app-panel` with a 1 px `--app-line` on
top: one dot per version, oldest left, then the label of the version on the stage ("v2 ·
Hotend · 2 min ago", "v3 · Height set to 46 mm · just now").

| Dot | Meaning |
| --- | --- |
| 8 px, filled `--app-text-2` | A version whose model was written (origin `write_model_code`) |
| 6 px, filled `--app-text-3` | A version that changed one size (origin `set_parameter`). A run of changes to one size collapses to its last dot with a count ("·3") |
| A `--app-fail` dot | The version has a problem |
| A 12 px ring in `--app-accent` | The version on the stage |
| 40 % opacity | A version newer than the current one (after going back) |

The rail shows the newest 24 dots; older ones scroll. Each dot is a button with the name
"Version 1, written by Hotend, has 2 problems".

**The comparison card.** Hovering or focusing a dot opens a card above it that compares that
version with the one before it. Drawn here for v2 of the recorded example while v3 is
current:

```
┌────────────────────────────────────────────┐
│v2 · Hotend · 12 min ago                   │
│"Wall 2 mm (slider 1.6 to 3 mm) so the snap │
│ groove leaves 0.8 mm"                      │
│Wall thickness   1 → 2 mm                   │
│box    122 × 62 → 124 × 64 mm · 36 → 54 g   │
│lid    122 × 62 → 124 × 64 mm · 25 → 26 g   │
│Now passes: Walls thick enough, Wall of the │
│box                                         │
│[ Look at v2 ]                [ Use v2 ]    │
└────────────────────────────────────────────┘
```

Lines, in order: the marker's label and when (for a written model its `note` in quotes);
sizes that
changed; each part whose size or solid weight changed; proof lines whose status changed,
worst first ("Now fails: Too big for the printer"), or "Same 10 of 15." What did not change
is summed up, never listed. The first version's card shows its sizes and its proof count.
The two buttons act on the version of the card; the card of the current version has
neither.

- `Look at v2`, or a click on the dot: that version goes on the stage, read-only, at the
  same camera. The verdict bar becomes row 5, the route gets `?v=2`, and Sizes and Proof show
  that version. `[` and `]` step to the older and the newer version; stepping back and forth
  between two versions at one camera is the comparison a maker needs for a 2 mm change.
- `Use v2`: makes it the current version (`version_revert`). Nothing is deleted. Later dots
  stay on the rail at 40 %, and the next change continues from v2 with a new, higher number.
- `Mod+Z` is "use the version this one was made from" (its `parent`); `Mod+Shift+Z` goes
  forward again through the versions undone in this session, until a new version is made.
  In a text field both keep their usual meaning.
- There is no geometric diff, no ghost overlay and no code diff.

## 6. Interaction details

### 6.1 Sizes that rebuild

A rebuild is a fresh sandboxed process. The two-part cable box takes about 6 s. The design
makes the wait legible instead of hiding it.

| When | What happens |
| --- | --- |
| Drag | The number follows the thumb at once. Steps: 0.5 mm (0.1 mm when the range is under 20 mm), 1°, 0.01 for a ratio, 1 for a count. The hollow tick stays at the built value. The part does not change: the window never shows geometry the engine did not build |
| Commit | On release, on Enter or Tab in the field, or 400 ms after the last arrow key. Arrow keys move one step, Shift+arrow ten, Home and End go to the ends. One `parameter_set` |
| 0 to 150 ms | Nothing else changes, so a fast build does not flicker |
| From 150 ms | The 3D view drops to 55 % opacity and stays. A 2 px `accent` line runs along the top edge of the stage. Verdict row 4. The row's tag is a spinner. Proof icons are spinners |
| Meanwhile | Other sizes stay usable. Changes queue: one build runs at a time, and the latest value of each size wins |
| Built | The mesh is swapped in place, the camera does not move, opacity returns in 120 ms, proof lines resolve, every number that changed gets an `--app-accent-wash` flash for 600 ms, the rail gains a dot, the conversation gains a marker |
| Built, with a problem | The version stays on the stage so the maker sees what the change did and why it fails. Verdict row 7: "Undo that change" makes the version it came from current again |
| Refused: the engine could not build it | No version is made. The thumb returns to the hollow tick in 180 ms and a line under the row says why in `fail`: "Height 80 mm does not build: (the engine's sentence). Still at 46 mm." Verdict row 6 |
| After 10 s | The verdict adds "taking longer than usual". A rebuild cannot be stopped in this milestone |

Sizes are disabled while an AI run is going, because both make versions; the tooltip says
"Hotend is working on this".

### 6.2 Focus and reading order

Every control is reachable with Tab in reading order: title bar, conversation, verdict bar,
view tools, the 3D view, part chips, rail, inspector. Focus is a 2 px `--app-accent` ring
with a 2 px gap, on every surface. No target is smaller than 28 x 28 px. The 3D view is one
tab stop with the accessible name "3D view of Cable box: box and lid, 124 × 64 × 42.4 mm";
single keys work while it has focus (a click on it, or `Mod+0`).

### 6.3 Keyboard

`Mod` is Command on macOS and Ctrl elsewhere. The viewer's own keys are switched off; these
replace them.

| Keys | Action | Keys, while the 3D view has focus | Action |
| --- | --- | --- | --- |
| `Mod+N` | Home, with the prompt focused | `F` | Fit the part in view |
| `Mod+1` `2` `3` | Design, Prepare, Print | `1` `2` `3` `4` | Corner, front, top, right view |
| `Mod+Enter` | Press the verdict bar's button | `C` | Cut open, on and off |
| `Mod+L` | Focus the composer | `[` `]` | Look at the older, the newer version |
| `Enter` / `Shift+Enter` | Send / new line | `Esc` | Leave Cut open; go back to the current version |
| `Mod+.` | Stop Hotend's run | | |
| `Mod+Z` / `Mod+Shift+Z` | Use the previous / the next version | | |
| `Mod+\` / `Mod+Shift+\` | Conversation / inspector | | |
| `Mod+0` | Focus the 3D view | | |
| `Mod+,` | Settings | | |
| `Mod+/` | Settings, Shortcuts | | |
| `Esc` | Close a popover or a menu | | |

No key starts or stops a print.

### 6.4 Stop

While Hotend's run is going, the send button is a Stop button (`square`), the verdict bar's
button is Stop, and `Mod+.` does the same (`run_cancel`). The button reads "Stopping…" for at
most five seconds; engine work in flight is stopped with the run. The conversation then ends
with "Stopped. v2 is still here." Versions already made stay. Text the AI will not remember
is shown as 4.3.1 says. A rebuild, a layout and a slice cannot be stopped in this milestone;
a running print is stopped only from the Print stage (4.7).

### 6.5 Reduced motion and other system settings

With `prefers-reduced-motion`: every duration is 0 except opacity changes, which stay at
120 ms; the 2 px progress line is a still bar; the number flash is dropped. The window
honours the system's text size up to 125 % without clipping: rows grow, columns keep their
widths.


## 7. Visual language

Quiet chrome, so the part and its numbers carry the screen. Dark is the default; light is a
full peer. Ink, one accent, and one hot spot. Colour is spent only on state.

### 7.1 Tokens

Host tokens are `--app-*`; tokens that only Hotend's screens use are `--hotend-*`. Tailwind 4
reads them through `@theme inline`. The theme is the attribute `data-theme` on the root
element; "System" is resolved by the window to one of the two. This block is the source of
truth: `docs/ux/contrast_check.py` reads the colours from it.

```css
:root,
[data-theme="dark"] {
  /* surfaces */
  --app-bg: #0E1114;                 /* window, title bar, Home, Settings */
  --app-panel: #151A1E;              /* the columns, the version rail */
  --app-raised: #1B2126;             /* cards, fields, popovers, stage furniture */
  --app-subtle: #1B2126;             /* the maker's message block, inset areas */
  --app-hover: #222930;              /* a row or a quiet button under the pointer */
  --app-active: #2A323A;             /* pressed; the selected segment */
  --app-line: #252C33;               /* separators and card borders (decorative) */
  --app-control: #6C7883;            /* borders of fields, sliders, steppers, segments */
  --app-scrim: rgb(0 0 0 / 0.5);
  /* text */
  --app-text: #E9ECEE;
  --app-text-2: #98A2AB;
  --app-text-3: #8E98A1;
  /* accent */
  --app-accent: #FFB45C;             /* links, the focus ring, the version on the stage */
  --app-accent-fill: #E9ECEE;        /* ink: the one primary button, the slider's filled part */
  --app-accent-fill-hover: #C9D0D6;
  --app-on-fill: #0E1114;
  --app-accent-wash: #2B2013;
  /* state */
  --app-ok: #45D08F;
  --app-ok-wash: #12281F;
  --app-warn: #EFD34A;
  --app-warn-wash: #29250F;
  --app-fail: #FF7A7A;
  --app-fail-wash: #2E1717;
  --app-fail-fill: #F26464;          /* the second click of Stop */
  --app-fail-fill-hover: #FF7A7A;
  --app-sim: #C0A6FF;                /* simulated and stand-in, nothing else */
  --app-sim-wash: #221B36;
  /* elevation */
  --app-shadow-1: 0 1px 2px rgb(0 0 0 / 0.24);
  --app-shadow-2: 0 8px 24px rgb(0 0 0 / 0.40);
  /* the stage */
  --hotend-stage-top: #151A1E;
  --hotend-stage-bottom: #0E1114;
  --hotend-stage-plate: #6C7883;    /* plate outline */
  --hotend-stage-edge: #0B0C0E;     /* edges of the parts */
  --hotend-part-1: #C9D0D6;         /* aluminium */
  --hotend-part-2: #7A818A;         /* graphite */
  --hotend-part-3: #C58F73;         /* clay */
  --hotend-part-4: #9AA39A;         /* stone */
  --hotend-part-5: #CDBE9E;         /* oat */
  --hotend-part-6: #8F7362;         /* cocoa */
  --hotend-part-7: #D8D2C4;         /* bone */
  --hotend-part-8: #8B879B;         /* dusk */
}

[data-theme="light"] {
  --app-bg: #EDEFF0;
  --app-panel: #F7F8F9;
  --app-raised: #F7F8F9;
  --app-subtle: #EDEFF0;
  --app-hover: #E6E9EB;
  --app-active: #D9DEE1;
  --app-line: #D2D7DB;
  --app-control: #76808A;
  --app-scrim: rgb(22 24 28 / 0.25);
  --app-text: #12161A;
  --app-text-2: #56606A;
  --app-text-3: #5D6771;
  --app-accent: #9A4A0E;
  --app-accent-fill: #12161A;
  --app-accent-fill-hover: #2B3238;
  --app-on-fill: #F7F8F9;
  --app-accent-wash: #F6E8DB;
  --app-ok: #0B6E43;
  --app-ok-wash: #DFF1E7;
  --app-warn: #6B5900;
  --app-warn-wash: #F3ECC4;
  --app-fail: #B82222;
  --app-fail-wash: #FAE3E3;
  --app-fail-fill: #C02626;
  --app-fail-fill-hover: #A81F1F;
  --app-sim: #5F35C4;
  --app-sim-wash: #EBE5FA;
  --app-shadow-1: 0 1px 2px rgb(0 0 0 / 0.06);
  --app-shadow-2: 0 8px 24px rgb(0 0 0 / 0.14);
  --hotend-stage-top: #E9ECED;
  --hotend-stage-bottom: #D9DEE1;
  --hotend-stage-plate: #69727E;
  --hotend-stage-edge: #12161A;
  --hotend-part-1: #C9D0D6;
  --hotend-part-2: #8C929B;
  --hotend-part-3: #C98F73;
  --hotend-part-4: #A3ADA3;
  --hotend-part-5: #D2C4A2;
  --hotend-part-6: #9A7C69;
  --hotend-part-7: #D9D2C2;
  --hotend-part-8: #948FA6;
}

:root {
  /* type */
  --app-font-sans: "Instrument Sans Variable", -apple-system, BlinkMacSystemFont, "Segoe UI", system-ui, sans-serif;
  --app-font-display: "Archivo Variable", "Helvetica Neue", Arial, sans-serif;
  --app-font-mono: "JetBrains Mono Variable", ui-monospace, "SF Mono", "Cascadia Mono", "DejaVu Sans Mono", monospace;
  /* heat: the mark's bead and the work that runs now, nothing else (7.2) */
  --app-heat-1: #FFC46B;
  --app-heat-2: #FF7A2E;
  --app-heat-3: #E8304A;
  /* radius */
  --app-radius-tag: 4px;             /* tags, chips */
  --app-radius-control: 6px;         /* buttons, fields, segments */
  --app-radius-card: 10px;           /* cards, popovers, menus */
  --app-radius-pill: 999px;          /* the verdict bar and its button, slider thumbs */
  /* sizes */
  --app-titlebar: 44px;
  --app-h-sm: 28px;                  /* buttons in the verdict bar and on the stage */
  --app-h: 32px;                     /* buttons, fields, rows of controls */
  --app-h-lg: 40px;                  /* the composer, the prompt, the verdict bar */
  /* motion */
  --app-dur-fast: 120ms;
  --app-dur-base: 180ms;
  --app-dur-slow: 240ms;
  --app-ease: cubic-bezier(0.2, 0, 0, 1);       /* entering and moving */
  --app-ease-out: cubic-bezier(0.4, 0, 1, 1);   /* leaving */
}
```

Disabled controls are at 40 % opacity and always carry a tooltip that says why.

The vendored shadcn/ui components read shadcn's own variable names. They are mapped once, so
no component file holds a colour:

```css
:root {
  --background: var(--app-bg);            --foreground: var(--app-text);
  --card: var(--app-raised);              --card-foreground: var(--app-text);
  --popover: var(--app-raised);           --popover-foreground: var(--app-text);
  --primary: var(--app-accent-fill);      --primary-foreground: var(--app-on-fill);
  --secondary: var(--app-subtle);         --secondary-foreground: var(--app-text);
  --muted: var(--app-subtle);             --muted-foreground: var(--app-text-2);
  --accent: var(--app-hover);             --accent-foreground: var(--app-text);
  --destructive: var(--app-fail-fill);
  --border: var(--app-line);              --input: var(--app-control);
  --ring: var(--app-accent);              --radius: 6px;
}
```

Note the names: shadcn's `--accent` is a hover surface, not Hotend's accent colour. The
style's hover for the default button (`hover:bg-primary/80`) is replaced by
`--app-accent-fill-hover`, and its focus ring (`ring-ring/50`, 3 px) by the solid 2 px ring
of 6.2: a half-transparent fill or ring has no provable contrast.

### 7.2 How colour is used

- **Ink** (`accent-fill`, with `on-fill` on it) fills the one primary button in view, the
  slider's range, and a switch or box that is on.
- **Accent** is for exactly three things: focus, links, and the version on the stage (its
  ring on the rail). It is never a status.
- **Heat** (`heat-1` to `heat-3`, the same in both themes) is for what runs now: the 2 px
  line along the top of the stage while Hotend builds, lays out or slices, at most one on a
  screen. The only other heat is the bead of the mark (7.7). It is never text and never the
  only signal: the verdict bar says the same in words. A simulated print is not hot: it keeps
  its violet.
- **Green** (`ok`) appears as ticks and counts, never as a filled area.
- **Yellow** (`warn`, away from the accent's orange) means "worth your attention": a guess, a note, something out of date.
- **Red** (`fail`) is for problems and for the second click of Stop.
- **Violet** (`sim`) is for "Simulated" and "Stand-in" and nothing else: the printer chip's
  tag, the band over a simulated print, the stand-in slicer's tag, the dialog's second line.
- State is never colour alone: every state has its own icon shape and its own words.
- A wash is the background of a tag or a card for that state, with the state colour or
  `--app-text` on it.
- Text that sits directly on the stage (captions under the 3D view) uses `--app-text-2`,
  never `--app-text-3`.

### 7.3 Contrast

WCAG 2.x contrast, (L1 + 0.05) / (L2 + 0.05), computed by `docs/ux/contrast_check.py` from
the block in 7.1. Each cell is the lowest ratio of that foreground over the backgrounds
named, and where it occurs. The tables below are that script's output.

<!-- contrast:begin -->
**Text (WCAG 1.4.3)**

| Pair | Dark, lowest | Light, lowest | Floor |
| --- | --- | --- | --- |
| `text` on every surface and both ends of the stage | 10.95 (on `active`) | 13.40 (on `active`) | 4.5 |
| `text-2` on every surface and both ends of the stage | 5.01 (on `active`) | 4.73 (on `active`) | 4.5 |
| `text-3` on `bg`, `panel`, `raised`, `subtle`, `hover` | 5.01 (on `hover`) | 4.73 (on `hover`) | 4.5 |
| `accent` as text on `bg`, `panel`, `raised`, `subtle`, `hover` | 8.36 (on `hover`) | 5.13 (on `hover`) | 4.5 |
| `ok` as text on `bg`, `panel`, `raised`, `subtle`, `hover` | 7.47 (on `hover`) | 5.18 (on `hover`) | 4.5 |
| `warn` as text on `bg`, `panel`, `raised`, `subtle`, `hover` | 9.87 (on `hover`) | 5.64 (on `hover`) | 4.5 |
| `fail` as text on `bg`, `panel`, `raised`, `subtle`, `hover` | 5.82 (on `hover`) | 5.23 (on `hover`) | 4.5 |
| `sim` as text on `bg`, `panel`, `raised`, `subtle`, `hover` | 7.10 (on `hover`) | 6.17 (on `hover`) | 4.5 |
| `accent` on `accent-wash` | 9.05 | 5.20 | 4.5 |
| `ok` on `ok-wash` | 7.91 | 5.37 | 4.5 |
| `warn` on `warn-wash` | 10.31 | 5.76 | 4.5 |
| `fail` on `fail-wash` | 6.65 | 5.22 | 4.5 |
| `sim` on `sim-wash` | 7.93 | 6.13 | 4.5 |
| `text` on any wash | 12.96 (on `warn-wash`) | 14.82 (on `sim-wash`) | 4.5 |
| `text-2` on any wash | 5.92 (on `warn-wash`) | 5.22 (on `sim-wash`) | 4.5 |
| `on-fill` on `accent-fill` and `accent-fill-hover` | 12.15 (on `accent-fill-hover`) | 12.22 (on `accent-fill-hover`) | 4.5 |
| `on-fill` on `fail-fill` and `fail-fill-hover` | 6.12 (on `fail-fill`) | 5.57 (on `fail-fill`) | 4.5 |

**Control boundaries, icons and focus (WCAG 1.4.11)**

| Pair | Dark, lowest | Light, lowest | Floor |
| --- | --- | --- | --- |
| `control` (field, slider and stepper borders) on `bg`, `panel`, `raised`, `subtle` | 3.60 (on `raised`) | 3.48 (on `bg`) | 3.0 |
| `accent-fill` (primary button, slider range) against `bg`, `panel`, `raised`, `subtle` | 13.70 (on `raised`) | 15.76 (on `bg`) | 3.0 |
| `accent-fill-hover` against `bg`, `panel`, `raised`, `subtle` | 10.43 (on `raised`) | 11.27 (on `bg`) | 3.0 |
| `fail-fill` (Stop) against `panel`, `raised` | 5.25 (on `raised`) | 5.57 (on `panel`) | 3.0 |
| `accent` focus ring on every surface and both ends of the stage | 7.39 (on `active`) | 4.61 (on `active`) | 3.0 |
| `ok` icon on every surface | 6.60 (on `active`) | 4.66 (on `active`) | 3.0 |
| `warn` icon on every surface | 8.72 (on `active`) | 5.07 (on `active`) | 3.0 |
| `fail` icon on every surface | 5.15 (on `active`) | 4.71 (on `active`) | 3.0 |
| `sim` icon on every surface | 6.28 (on `active`) | 5.54 (on `active`) | 3.0 |

**The stage (WCAG 1.4.11)**

| Pair | Dark, lowest | Light, lowest | Floor |
| --- | --- | --- | --- |
| `stage-plate` (plate outline) on both ends of the stage | 3.88 (on `stage-top`) | 3.59 (on `stage-bottom`) | 3.0 |
| `part-1` told from both ends of the stage, by its fill or by `stage-edge` | 11.25 (by its fill, on `stage-top`) | 11.66 (by its edge, on `stage-bottom`) | 3.0 |
| `part-2` told from both ends of the stage, by its fill or by `stage-edge` | 4.45 (by its fill, on `stage-top`) | 5.80 (by its edge, on `stage-bottom`) | 3.0 |
| `part-3` told from both ends of the stage, by its fill or by `stage-edge` | 6.30 (by its fill, on `stage-top`) | 6.63 (by its edge, on `stage-bottom`) | 3.0 |
| `part-4` told from both ends of the stage, by its fill or by `stage-edge` | 6.74 (by its fill, on `stage-top`) | 7.84 (by its edge, on `stage-bottom`) | 3.0 |
| `part-5` told from both ends of the stage, by its fill or by `stage-edge` | 9.56 (by its fill, on `stage-top`) | 10.52 (by its edge, on `stage-bottom`) | 3.0 |
| `part-6` told from both ends of the stage, by its fill or by `stage-edge` | 4.00 (by its fill, on `stage-top`) | 4.73 (by its edge, on `stage-bottom`) | 3.0 |
| `part-7` told from both ends of the stage, by its fill or by `stage-edge` | 11.63 (by its fill, on `stage-top`) | 12.07 (by its edge, on `stage-bottom`) | 3.0 |
| `part-8` told from both ends of the stage, by its fill or by `stage-edge` | 5.04 (by its fill, on `stage-top`) | 5.83 (by its edge, on `stage-bottom`) | 3.0 |
| `stage-edge` against every part tint | 4.47 (on `part-6`) | 4.73 (on `part-6`) | 3.0 |
<!-- contrast:end -->

Known gap, not a token: the viewer's selection colours are fixed in its code (`#53A0E3`
picked, `#89B9E3` hovered). This milestone does not pick faces, so the window never shows
them; the colour must become an option of the viewer before picking is built (14).

### 7.4 Type

Three families, bundled with the app (`@fontsource-variable/archivo`, `-instrument-sans`,
`-jetbrains-mono`, SIL OFL 1.1; no font is fetched at run time). Archivo, at 125 % width, is
for `display`, `title` and the wordmark. Instrument Sans is the interface. JetBrains Mono is
for data: sizes written as a value, the overall size on the stage, the print percentage, the
time left, the layer and the bed temperature, hashes, paths, serials and the printer's own
codes. A number field and the slice time keep Instrument Sans, because their width is counted
for it. Numbers that can change use `font-variant-numeric: tabular-nums`. Instrument Sans has Latin letters
only: other scripts and symbols fall back to the system's font. Its space is narrow, so the
window adds 0.05em between words.

| Role | Size / line | Weight | Used for |
| --- | --- | --- | --- |
| `display` (Archivo) | 28 / 34, -0.03em | 800 | "What do you want to make?", "Set up Hotend" |
| `title` (Archivo) | 20 / 26, -0.02em | 750 | Settings section titles |
| `numeric-lg` | 22 / 28 | 600 | Slice time, print percentage |
| `heading` | 15 / 20 | 600 | Project name in the title bar, card names |
| `body` | 14 / 21 | 400 | Conversation text, paragraphs |
| `ui` | 13 / 18 | 400; 500 for values, buttons and the verdict | Rows, controls, the verdict bar, tooltips |
| `caption` | 12 / 16 | 400 | Tags, ranges, times, the work line, version markers |
| `label` | 11 / 14, +0.04em, uppercase | 600 | Inspector section labels (SIZES, PROOF, PLATE, MATERIAL, SLICE, REVIEW, SEND TO, PRINTING) and the three section labels of Setup. The smallest text there is, and the only uppercase |
| `mono` (JetBrains Mono) | 12 / 16 | 400 | Codes, hashes, paths |

### 7.5 Space, radius, elevation

- Spacing steps: 2, 4, 8, 12, 16, 24, 32, 48. Column padding 16. Gap between cards 12.
- Row heights: 28 (proof lines, the work line, menu items), 32 (buttons, fields, part chips),
  40 (the verdict bar, the composer when empty), 56 (a size row with a slider).
- Radius: 4 tags and chips, 6 buttons and fields, 10 cards and popovers, 999 the verdict bar
  and slider thumbs. Columns and the stage have square corners: they are the window, not
  cards.
- Elevation, three levels. 0: columns, separated by a 1 px `--app-line`. 1: stage furniture
  and cards (`--app-shadow-1` plus the border). 2: popovers, menus and tooltips
  (`--app-shadow-2` plus the border). No blur and no translucency anywhere.

### 7.6 Icons

`lucide-react`, 16 px in rows and controls, 18 px in the title bar and the view tools, 1.5 px
stroke, `currentColor`. An icon stands alone only in the title bar and the view tools, each
with a tooltip and an accessible name. No emoji, no filled icons, no illustrations.

| Use | Names |
| --- | --- |
| Status | `circle-check`, `triangle-alert`, `circle-x`, `circle-question-mark`, `loader-circle` (busy), `circle-dashed` (skipped) |
| Where a number comes from | `circle-question-mark` (guess), `ruler` (measured), `book-marked` (standard), `gauge` (from your printer) |
| Simulated and stand-in | `flask-conical` |
| Title bar | `house`, `panel-left`, `panel-right`, `settings`, `arrow-left`, `printer` |
| Stage | `scan` (fit), `box` (views, and the empty picture), `scissors` (cut open), `history` (an older version) |
| Conversation | `arrow-up` (send), `square` (stop), `chevron-right`, `chevron-down` |
| Print | `spool`, `thermometer`, `usb`, `external-link`, `folder-open` |
| Other | `ellipsis`, `search`, `copy`, `check`, `plus`, `minus`, `sun`, `moon`, `monitor` |

### 7.7 The mark

The mark is the hot end of a printer and the bead it lays: block, hex band, nozzle, bead, on a
48 x 48 grid (`src/host/BrandMark.tsx`, `public/favicon.svg`). Block, band and nozzle are
`text`; the bead runs from `text` over `heat-3` and `heat-2` to `heat-1`. Never smaller than
16 px; clear space of at least the hex band's height (5.5 of 48). The wordmark beside it is
"hotend" in lowercase, Archivo 800 at 125 % width, -0.035em. Running text says "Hotend", and so
does the mark's accessible name.

The app icons and the favicon are the mark in the dark theme's colours on a dark rounded tile.
`docs/ux/make_icons.py` draws all of them from these numbers and nothing else:

```sh
(cd python && uv sync --frozen)                      # Pillow
python/.venv/bin/python docs/ux/make_icons.py            # writes the icons and the favicon
python/.venv/bin/python docs/ux/make_icons.py --check    # fails if they are not its output
```

## 8. Components

shadcn/ui in its Base UI variant, style "nova" (32 px controls), copied into
`apps/desktop/src/components/ui/`. The source files are
`apps/v4/registry/bases/base/ui/<name>.tsx` of the shadcn-ui repository, with the class
sheet `apps/v4/registry/styles/style-nova.css`. Primitives come from `@base-ui/react`. No
other component library, and no Radix package, is added.

| In Hotend | shadcn file | Base UI primitive | Notes |
| --- | --- | --- | --- |
| Button: primary, outline, quiet, destructive; 28, 32 and 40 px; icon-only | `button` | Button | Variants `default`, `outline`, `ghost`, `destructive`. At most one `default` button on a screen |
| Tooltip on every icon button and every disabled control | `tooltip` | Tooltip | 600 ms delay, none between neighbours |
| Stage switch | `tabs` | Tabs | Each tab is a route link; the mark sits after the label |
| Segmented choice: provider, theme, the direction of Cut open | `toggle-group` | ToggleGroup | One always selected |
| Size slider | `slider` | Slider | Track and thumb as in 5.3; the hollow tick is Hotend's |
| Number field, stepper: sizes, copies, effort | none | NumberField | Built on the primitive, styled like `input` |
| Text field | `input`, `label`, `field` | Input, Field | The key field is `type="password"` and write-only |
| Composer, Home prompt | `textarea` | none | Grows from one line to eight (six on Home) |
| Menu: project name, card, Views | `dropdown-menu` | Menu | |
| Select: material, quality | `select` | Select | |
| Model field | `combobox` | Combobox | Free text allowed |
| Radio rows: Send to, Turn | `radio-group` | RadioGroup | The sentence under a choice is part of its label |
| Switch: the stand-in slicer | `switch` | Switch | |
| Checkbox: export formats | `checkbox` | Checkbox | |
| Popover: comparison card, Turn, Export, Cut open, printer chip | `popover` | Popover | Anchored, never centred, never over the part's centre |
| Disclosure: a proof line, "Fixed", "Show steps", "Details" | `collapsible` | Collapsible | |
| Scrolling columns | `scroll-area` | ScrollArea | |
| Column handles | `resizable` | none (`react-resizable-panels`) | Double-click to reset is Hotend's |
| Print progress | `progress` | Progress | |
| Tag: provenance, Simulated, slice warnings | `badge` | none | 20 px high, radius 4, an icon and two words |
| Card: project, error, proposed print | `card` | none | |
| View tools | none | Toolbar | Arrow keys move between its buttons |
| Busy mark | `spinner` | none | `loader-circle`, one turn per 1.2 s |
| Key hints in tooltips and Shortcuts | `kbd` | none | |
| Loading outlines on Home | `skeleton` | none | Static, no shimmer |
| Separator | `separator` | Separator | |
| Shortcuts table | `table` | none | |

Not used, on purpose: `dialog`, `alert-dialog`, `sheet`, `drawer` (nothing opens over the
part), `sonner` and `toast` (no message that disappears by itself), `command` (no command
palette), `hover-card`, `sidebar`, `navigation-menu`.

Hotend's own components, built from the tokens and the pieces above: `Stage` (hosts the
viewer), `VerdictBar`, `VersionRail`, `ComparisonCard`, `PartChips`, `SizeRow`, `ProofLine`,
`Composer`, `WorkLine`, `VersionMarker`, `PrinterChip`, `PlateRow`, `ReviewPanel`,
`HealthRows`, `ProjectCard`.

## 9. Motion

CSS transitions for hover, press, focus and popups (Base UI's `data-starting-style` and
`data-ending-style`); the `motion` library for things that change size or enter a list (a
column hiding, the verdict bar changing width, a card entering the conversation, a proof
line opening). No springs, no bounce, no overshoot.

| Token | Value | Used for |
| --- | --- | --- |
| `--app-dur-fast` | 120 ms | Hover, press, focus ring, tooltips, the 3D view's opacity returning after a rebuild |
| `--app-dur-base` | 180 ms | Popovers and menus, a card entering (opacity, and 4 px up), a proof line opening, a tag changing, the slider thumb returning, the verdict bar's content cross-fading |
| `--app-dur-slow` | 240 ms in, 160 ms out | A column hiding or showing, the inspector's content changing with the stage |
| `--app-ease` | `cubic-bezier(0.2, 0, 0, 1)` | Entering and moving |
| `--app-ease-out` | `cubic-bezier(0.4, 0, 1, 1)` | Leaving |
| (loop) | 1.2 s, linear | The spinner, and the 2 px progress line along the top of the stage |
| (flash) | 600 ms, once | `--app-accent-wash` behind a number that changed, fading out |

What moves: columns, popovers, cards, opacity, the thumb. What never moves:

- **Numbers.** A changed number is replaced at once and flashed, never counted up. A number
  is a fact.
- **Status.** An icon and its colour swap at once.
- **Geometry.** Versions are swapped, never morphed: a morph shows shapes that were never
  built or checked.
- **The camera**, unless the maker asks (Fit, a view) or the stage changes; then it cuts. It
  never moves on a rebuild.
- **Streamed text.** No typewriter effect.
- **The confirmation dialog, Stop, errors.**

Nothing loops except the spinner and the progress line. Reduced motion: 6.5.

## 10. The 3D viewport

The view is `three-cad-viewer` 5.1.3 with its own toolbar, tree and tabs off. Hotend draws
its furniture as ordinary HTML over it (4.3.2).

### 10.1 Viewer options

| Option | Value | Why |
| --- | --- | --- |
| `tools`, `glass`, `pinning` | `false` | Hotend draws its own tools |
| `theme` | `"dark"` or `"light"`, following `data-theme` | The viewer's grid and marker follow it |
| `control`, `up` | `"orbit"`, `"Z"` | The part turns like an object on a turntable |
| `ortho` | `false` (perspective) | A product, not a drawing |
| `grid` | `[true, false, false]` | The bed plane only |
| `axes`, `axes0` | `false` | The small marker in the corner is enough |
| `ambientIntensity`, `directIntensity` | `1.0`, `1.1` | The viewer's defaults |
| `metalness`, `roughness` | `0`, `0.7` | Matte plastic, not metal |
| `edgeColor` | `--hotend-stage-edge` as a number | Edges are what makes a solid read as CAD-exact |
| `keymap` | every action unbound | Hotend's keys of 6.3 replace the viewer's |

The viewer's studio mode is never switched on: its environments are fetched from the
internet (`dl.polyhaven.org`), which the window's content policy forbids.

### 10.2 Look

- **Ground.** A vertical CSS gradient behind a transparent canvas, from `--hotend-stage-top`
  to `--hotend-stage-bottom`. No horizon, no skybox, no floor shadow.
- **Grid.** The viewer's own XY grid, in the viewer's colours for the theme, with the size
  of one square written by the viewer at the bottom left.
- **Parts.** The mesh is `version_mesh`, loaded unchanged. Before it is rendered the window
  replaces each part's colour with a tint, `--hotend-part-1` to `-8` in the order of the
  parts, repeating after eight. The tints are neutrals chosen so that no part is ever green,
  amber, red, blue or violet: those mean something. The engine's own palette stays in the
  pictures it renders for the AI.
- **The plate.** Drawn as lines in `--hotend-stage-plate`: the outline of the bed the main
  nozzle reaches (256 × 256 mm for the X2D), at Z 0. It is shown in an empty project, in
  Prepare and in Print. In Design with a part there is no plate: the parts are shown as
  assembled, not as printed.
- **Camera.** The corner view by default. Opening a project fits the part. After that the
  camera moves only when the maker moves it, presses Fit or a view, or changes stage
  (Prepare and Print fit the plate from the corner). It never moves on a rebuild or when a
  version is swapped.

### 10.3 What the stage shows, by stage

| Stage | Scene |
| --- | --- |
| Design, no version | The grid and the plate outline |
| Design | The parts of the version on the stage, at the positions the model gives them |
| Prepare | The plate outline, and one copy of each part's mesh per piece of the selected plate, placed with the piece's `transform` from the layout (a 3 x 4 matrix, plate = R x model + t) |
| Print | As Prepare for the job's plate; while the job runs, cut by a horizontal plane at the printed height |

### 10.4 States of the view

| State | Look |
| --- | --- |
| Rebuilding | The canvas at 55 % opacity (a CSS opacity on its container) and the 2 px line; the old part stays |
| Swapped | The new mesh replaces the old in place (`updatePart` per part; when the parts of the model changed, a new render with the camera kept) |
| A part hidden by its chip, or by "Show me" | Not drawn |
| Cut open | One clip plane through the middle of the part across X (Across), across Y (Along) or across Z (From the top); the slider moves it; cut faces take the part's own colour |
| An older version | The same camera; nothing else marks the view. The verdict bar and the rail say which version it is |
| The mesh could not be read | The stage background with, in the centre: "This version has no 3D view." and the build's own warning as a caption |
| No WebGL | In the centre: "This computer cannot show 3D here." and `Open This computer`. Sizes, proof and slicing still work |

### 10.5 First: prove the viewer

Historical: this is the plan from before the window was built; the viewer is in use now
(`apps/desktop/src/hotend`). When it was written nothing here had been tried in a page. The
first build step that touched the stage was to be a spike that proves each use below, with
the cable box, in headless Chromium. Each has a decided answer if it fails, so the build
never waits on a design question.

| # | Use | If it does not work |
| --- | --- | --- |
| 1 | `render()` of `mesh.json` unchanged with `tools: false`, in headless Chromium without a GPU | No fallback: this is the contract of PROTOCOL 3.4 |
| 2 | A transparent canvas over the CSS gradient (`--tcv-bg-color: transparent` on the container) | A flat stage in `--hotend-stage-top` through the same variable |
| 3 | Part colours and `edgeColor` set by the window | No fallback |
| 4 | `updatePart` swaps a version's mesh without moving the camera | A new `render()` with the camera saved and restored (`getCameraLocationSettings`, `setCameraLocationSettings`) |
| 5 | Hiding and showing a part (`setState`) | Chips and "Show me" are left out |
| 6 | One clip plane set and moved from code (`setClipNormal`, `setClipSlider`), with cut faces coloured | Cut open is left out, and a running print shows no cut, only the numbers |
| 7 | Every viewer key unbound (`keymap`, `setKeyMap`) | Single keys of 6.3 are left out; the `Mod` keys stay |
| 8 | `getImage()` returns the canvas as a picture | Cards show the `box` icon |
| 9 | The plate outline as an `edges` part, and copies of a part that share one mesh in Prepare | No outline; copies carry their own buffers |
| 10 | `resizeCadView` follows the column handles without a flash | Resize on release of the handle |

### 10.6 Pictures for the cards

When a version has been on the stage in Design for one second with no rebuild running, the
window takes the canvas (`getImage`), scales it to 528 x 352 and keeps it in the window's
own storage (IndexedDB, keyed by project and version, newest 200). Home shows the picture of
each project's current version. It is a cache for display: nothing else reads it and nothing
is lost when it is cleared.


## 11. Copy

Tone: a calm colleague at the next bench. Plain words a maker uses (part, size, wall, fit,
plate), short sentences, millimetres, the maker's own nouns ("the lid", not "part 2"). Say
what is true, then what to do. Rules:

- The AI is "Hotend". It "guessed", "checked", "could not build"; it never "thinks" or
  "feels". The words "AI" and "model" appear only where the maker deals with the provider:
  Setup and Settings.
- Never: exclamation marks, "oops", "sorry", "AI-powered", "guaranteed", "certified",
  "demo". Never CAD words (B-rep, manifold, tessellation, extrude, parameter) or developer
  words (run, sandbox, exception, token as a secret) outside Settings, This computer.
- Checked, never guaranteed. Simulated, never "demo". Stand-in, never "fake".
- Buttons are a verb and its object. An ellipsis ends only a button that asks
  something more before anything happens: a confirmation, a system picker, a popover of
  choices.
- Engine and core sentences are shown word for word. The window adds its own sentence
  before them, never rewrites them.
- "Anthropic" below stands for the chosen provider's name.

All strings live in one catalogue (`src/hotend/strings.ts`, `src/host/strings.ts`) from the
first line, so that translation is possible later.

| # | Where | Exact wording |
| --- | --- | --- |
| 1 | Setup, title and line | "Set up Hotend" · "You describe a product. Hotend models it, checks it and gets it ready to print." |
| 2 | Setup, AI | "Hotend designs with an AI model that you pay for directly." · "Stored in this computer's keychain. Hotend's AI never sees it." · "Where do I get a key?" · "Hotend checks the key with your first message." |
| 3 | Key states | "Key saved · in this computer's keychain" · `Replace` · `Remove` · "Using the key in ANTHROPIC_API_KEY. Hotend reads it and never stores it." |
| 4 | No keychain | "This computer has no keychain, so Hotend cannot store a key here. Close Hotend and start it again with these two set; it then reads the key from there and never stores it:" · `Copy these lines` |
| 5 | Setup, printer | "This version never sends a print or a command to a real printer. Everything up to the print is real; the print itself is simulated, or you save the sliced file for a USB drive or Bambu Connect." |
| 6 | Health rows | "Safe room for AI-written models" · "Modelling engine" · "Slicer" · "Key storage" · "On: no network, and it cannot open your files." · "None found. You can design and check parts; slicing needs Bambu Studio." · "No keychain on this computer." · "Checking this computer…" |
| 7 | Fails closed | "Hotend cannot run AI-written models safely on this computer, so it will not run them at all." |
| 8 | Setup, leaving | `Start designing` · "Open the example instead (no key needed)" · "Save an AI key to start, or open the example." |
| 9 | Home | "What do you want to make?" · placeholder "Describe it, with the sizes you know" · "Projects" · "Your projects will appear here." · "Open the example: a cable box with a snap-on lid" · "Add an AI key to start." |
| 10 | Suggestions | "A box that hides a power strip, with a snap-on lid" · "A wall hook for a 32 mm pipe, held by two screws" · "A drawer divider for cutlery, 300 mm long" |
| 11 | Card status | "Nothing built yet" · "1 problem" · "3 to measure" · "Ready, 1 note" · "Ready" · "Printing 42 %" |
| 12 | Empty stage | "Nothing here yet. Describe what you want to make." · "Bambu Lab X2D · plate 256 × 256 mm · Simulated" |
| 13 | Composer | "Describe what you want to make" · "Ask for a change…" · "Hotend is working" · "Messages change v2, the current version." · "Add an AI key to talk to Hotend." |
| 14 | Work line | "Thinking…" · "Writing the model: …" · "Changing Height…" · "Checking it…" · "Looking at it…" · "Measuring…" · "Reading the manual…" · "Slicing…" · "Getting a print ready for you to review…" · "Show steps (3)" · "9 steps · 38k tokens" |
| 15 | Finished steps | "Wrote the model · v3" · "The model did not build, trying again" · "Set Height to 46 mm · v3" · "Checked it" · "Looked at 2 views" · "Measured it" · "Sliced: 59 min, 28 g" · "Could not slice" |
| 16 | Version markers | "v2 · Hotend" · "v3 · Height set to 46 mm" · "Now passes: Walls thick enough, Wall of the box" · "Now fails: Too big for the printer" · "Same sizes, same proof." · "2 problems" · "3 to measure" |
| 17 | Proposed print | "Hotend prepared a print for you to review." · `Go to Print` |
| 18 | Verdict, Design | "Ready to print" · "Ready to print, with 1 note" · "3 numbers are guesses" · "1 number is a guess" · "1 problem: Wall too thin" · "2 problems to fix" · "Wall thickness 0.5 mm gives 1 problem: Wall too thin" · "Height 80 mm does not build. Still at 46 mm." · "Rebuilding with Height 46 mm" · "… taking longer than usual" · "Looking at v1 of 2" · "Hotend is working: …" |
| 19 | Verdict buttons | `Prepare` · `Prepare anyway` · `Read it` · `Measure them` · `Measure it` · `Ask Hotend to fix it` · `Show me` · `Undo that change` · `Back to v2` · `Use v1` · `Stop` · `Go to Design` · `Go to Prepare` · `Slice` · `Slice again` · `Review print` · `Review again` · `Print again` · `Open Settings` |
| 20 | Proof | The titles of 5.2 · "Meets the guess. Not proven until you measure it." · "Does not meet the guess. Measure it first." · "Hotend cannot measure this on the part." · "Goes together as drawn. Not proven on this printer." · "Hotend uses general values for fits until your printer is calibrated. Calibration comes in a later version; until then, print one and try the fit." · "Hotend has not written down what this product must do." · `Ask Hotend to write it down` · `Tell Hotend what you measured` |
| 21 | Sizes | "To measure" · "Your choices" · "Fixed · 2" · "Hotend's guess" · "You measured" · "Standard part" · "From your printer" · "60 to 200" · `Use my measurement` · `Change` · "Measure it and type what you read. Hotend rebuilds the part around it." · "Hotend is working on this" |
| 22 | Messages the window sends for the maker | "Fix this: (the engine's sentence)" · "I measured Inside width: 118 mm. Use this value and mark it as measured." · "Continue." |
| 23 | Size refused | "Height 80 mm does not build: (the engine's sentence). Still at 46 mm." |
| 24 | Stop | `Stop` · "Stopping…" · "Stopped. v2 is still here." · "Stopped. Nothing was made." · "Hotend will not remember this part of the conversation." |
| 25 | Run errors | The table of 4.3.5 |
| 26 | Prepare | "PLATE 1 of 1" · `Turn…` · "How should the lid lie on the plate?" · "no support" · "support under 7 630 mm²" · "288 mm² on the bed" · "10.4 mm tall" · "Does not fit" · "Change in Settings" · "Standard · 0.20 mm" · "Fine · 0.12 mm" · "Not sliced yet." · "Changed since this slice." · "Sliced before Hotend drew plate previews. Slice again to hand it to Bambu Connect." · "not stated by the slicer" |
| 27 | Verdict, Prepare | "Laying out the plate" · "Fits on one plate." · "Fits on 2 plates. Showing plate 1." · "The lid does not fit on a plate. Turn it, or make it smaller." · "Slicing plate 1" · "Sliced: 59 min · 28 g" · "Changed since the last slice." · "Sliced before Hotend drew plate previews." · "No slicer is set up, so there is no time or weight yet." · "Fix 1 problem in Design first." · "Nothing to prepare yet." |
| 28 | Slice tags | "Stand-in slicer: rough numbers, no printable file" · "Generic profile, not tuned for this printer" · "This slicer version has not been tested with Hotend" · "The slicer did not confirm the nozzle's reach" · "X2D limits not confirmed on a real printer" |
| 29 | Review | "Read from the sliced file." · "not stated" · "A simulated print. Nothing is made." · "Hotend saves the file. You open it there and start it; Hotend does not control the print." · `Choose a folder…` · `Start print…` · `Save for Bambu Connect…` · `Save file…` · "The stand-in slicer makes no printable file." · "3 numbers are still guesses: Inside width, Inside depth, Cable slot." |
| 30 | Review, blocked | "Hotend stopped this file before it reached the printer." · "The job needs PLA on the main nozzle. No loaded spool matches." · "The sliced file has changed since it was made. Slice it again." · `Back to Prepare` |
| 31 | Confirmation (drawn by the shell) | "Start this print?" · "SIMULATED PRINTER. Nothing will be printed." · "These numbers were read from the sliced file." · `Cancel` · `Start print` · "Save this file for Bambu Connect?" · `Save` · "Save this file?" · `Save` |
| 32 | After the dialog | "Waiting for your answer in the Hotend dialog" · "That took longer than two minutes, so nothing was started. Review it again." |
| 33 | Running print | "Simulated printer: nothing is being printed" · "Drawn from the printer's layer count. There is no camera in this version." · "Waiting for the printer to start · 0:42" · "Hotend sent the job. A print counts as started only when the printer says so." · "Getting ready: heating" · "Printing · layer 84 of 200" · "About 34 min left" · "Finished." · "The printer accepted the job but did not start." · "The printer stopped." · "Lost contact with the printer." · "The print may still be running." |
| 34 | Stop a print | `Stop` · "Stop for good?" · `Keep printing` · `Stop` · "Stopped. Nothing more will be printed." |
| 35 | Delivered | "Saved to the folder." · `Show in folder` · "Saved for Bambu Connect. You open it and start it there." · "Hotend does not open Bambu Connect itself. Open this file in it:" · "Hotend cannot follow this print." |
| 36 | Simulated and stand-in | Tag "Simulated" · "Everything up to the print is real. The print is not." · "Nothing is printed on this printer." · "X2D limits have not been confirmed on a real printer yet." · "Use the stand-in slicer" · "Rough time and weight, no printable file. For trying Hotend without a slicer." |
| 37 | Settings | "AI model" · "Printer and material" · "Slicer" · "Appearance" · "This computer" · "Shortcuts" · "About" · "steps per message" · "Hotend sends your messages, the model's code, what Hotend's tools answer (checks, measurements, print settings, slices, what the code prints) and pictures of the part to Anthropic, using your key. With web research on, search words and page addresses also go to the search service you chose. If you set up a printer, its status is read over your network." · "Projects started with Anthropic cannot continue their conversation with another provider." · "Parts are checked for this material the next time they are built." · "Works" · "Not tested with Hotend" · "Too old to run safely" · "Not found" · `Choose program…` · `Check again` · `Copy report` · "Copied" |
| 38 | Export | `Export files…` · `Choose a folder and export` · "3 files saved." · `Show files` |
| 39 | The 3D view | "This version has no 3D view." · "This computer cannot show 3D here." |
| 40 | Home, failure | "Hotend could not open its project folder." · `Try again` |

Plurals follow the count ("1 note", "2 notes"; "1 problem", "2 problems"; "1 number is a
guess", "3 numbers are guesses"). Counts above nine are still written as digits.

## 12. What each screen reads and does

Every row is a method of `hotend_core::Core` that exists today, exposed to the window as a
typed Tauri command of the same name. Errors arrive as `ApiError`: a `kind`, a `message` and
a `next_step` written for the user, which the window shows whenever this specification gives
no sentence of its own.

| Screen | Reads | Does |
| --- | --- | --- |
| Start | `settings_get`, `projects_list` | |
| Setup, Settings | `settings_get`, `provider_key_status`, `printers_list`, `slicers_list`, `doctor_report` | `settings_set`, `provider_key_set`, `provider_key_delete`, `slicer_choose` |
| Home | `projects_list`; per project `version_get` of its current version and `print_jobs` | `project_create`, then `message_send` |
| Design | `project_get`, `version_get`, `version_mesh`, `questions_list` | `message_send`, `run_cancel`, `parameter_set`, `version_revert`, `export_run` |
| Prepare | `preparation_get`, `slice_get` | `prepare_run`, `slice_run` |
| Print | `slice_get`, `print_jobs`, `print_status`, `printers_list` | `print_propose`, `print_review`, `print_confirm` (through the shell), `print_cancel` |
| Everywhere | The core's event stream (`Core::events`): `run` events (text, tool starts and ends, usage, the end of a run) and `print` events (status, phase, progress, problems) | |

Facts about the data that the design relies on:

- A version has `number`, `created_at_ms`, `parent`, `origin`, `note`, `status` and
  `params`. The origin is `write_model_code` for a written model (the AI's tool and the
  facade's `model_build` alike) and `set_parameter` for one changed size (the AI's tool and
  the facade's `parameter_set` alike). Size changes between versions come from `params`;
  proof changes need both results.
- `version_revert` makes an older version current and creates no version. A value the model
  refuses (outside its range, or one that does not build) comes back from `parameter_set` as
  a report without a version, with the engine's sentence in `result.error`.
- The stored conversation is text only (`role`, `text`, `at_ms`, `remembered`). Tool steps
  are not stored, which is why they are session-only (4.3.1).
- The material in the settings is `{id, family, density_g_cm3}`: `pla-basic`, PLA, 1.24;
  `petg-basic`, PETG, 1.27; `tpu-95a`, TPU, 1.21 (the values of the build's filament
  settings).
- A slice is out of date when the stored layout is newer than the slice
  (`preparation.created_at_ms` after the slice's `created_at_ms`), or when the quality in
  the menu is not the slice's `profile`.
- The simulated printer reports phase, progress, time left, layer, the bed temperature, AMS
  units and trays. It runs sixty times faster than a real print.

What the shell adds (Rust in `apps/desktop/src-tauri`; none of it needs a change to the
core):

| Shell command | What it does |
| --- | --- |
| `print_confirm(job_id)` | Draws the confirmation of 4.6 from the reviewed summary. Only on the confirming button does it call the core's `print_confirm` with the process's one `ConfirmationAuthority`, which no other command can reach. Returns the job |
| `example_open` | `project_create("Cable box (example)")`, then `model_build` with the model file bundled with the app as a resource (a copy of `python/tests/models/cable_box.py`; a test fails when the two differ) |
| `stand_in_slicer(on)` | On: `slicer_choose` with the path of the stand-in slicer shipped with the app. Off: `settings_set` without that entry |
| `folder_pick`, `file_pick` | The system's pickers, for export, Save file and Choose program |
| `show_in_folder(path)` | Opens a folder of the data directory, or one the maker picked, in the file manager |
| `open_link(which)` | Opens one of a fixed list of addresses outside the window: the providers' key pages, and the `bambu-connect://` link the core returned for a handed-over job |
| events | Forwards the core's event stream to the window |

The webview gets no other command, loads no remote content and has a strict content policy
(`docs/IMPLEMENTATION_BRIEF.md`, rule 7).

**Fixture mode (the original plan).** The plan was a data layer with two implementations, the
Tauri commands and a set of recorded fixtures, so that every screen could be reached without a
core, a key or a printer. It was not built. Instead the interface's end-to-end tests and the
pictures run against the development bridge with a scripted core (`apps/desktop/e2e`), and the
unit tests read recorded results in `apps/desktop/tests/fixtures`.

The projects of that plan, and what the core returned for each (section 15). The
frames of section 4 draw the first two, so a screenshot can be held against its frame:

| Fixture | Recorded from | What it gives |
| --- | --- | --- |
| Cable box (example) | `example_open` | v1: no problem, 3 numbers to measure (Inside width 120 mm, Inside depth 60 mm, Cable slot 12 mm), 3 notes, "PROOF 10 of 15"; sizes Height 40 mm (26 to 80) and Wall thickness 2 mm (1.6 to 3); box and lid, 124 × 64 × 42.4 mm, 81 g solid |
| Cable box | The core's scripted agent with the conversation `hotend demo` replays | v1 with 2 problems (Wall too thin, Wall of the box), v2 as the example; the layout (box as modelled, lid upside down); a stand-in slice of 59 min, 28 g, 200 layers with three warnings; a pending print job; its review (no temperature stated, the warning "no moves"); the job running and finished on the simulated printer |
| Plain box | `python/tests/models/box.py` with Wall thickness as `style` | "Ready to print", "PROOF 6 of 6", no contract, one part |
| Bracket | `python/tests/models/bracket.py` with Thickness as `style` | "Ready to print, with 1 note": Needs support |
| Cup | `python/tests/models/broken_thin_wall.py` with the wall as `style`, labelled "Wall thickness", 2 mm | v1 ready; `parameter_set(wall, 0.5)` makes v2 with the problem Wall too thin; `parameter_set(wall, 9)` is refused with no version |

The three changed models are copies of the named test model with one line of `PARAMS`
changed; they are not in the repository. Every test model of the
repository carries a guessed number, and a guessed number is never "Ready".


## 13. The original build list

This is the plan the first version of the window was built from, kept for the record. Where
it names fixtures or a `/kit` route, that part was replaced as section 12 says or not built.

In this order. Each step leaves the app working and `bash scripts/test-all.sh` green. A step
is done when its tests pass and its screenshots show what is listed; the screenshots are
taken by a Playwright suite in fixture mode (section 12) and kept as test artefacts.

For every screenshot, without saying it again each time: one in the dark theme and one in the
light theme at 1280 x 800; all text in the fonts of 7.4; no text clipped or overlapping; at most one
filled button; the "Simulated" tag in the title bar's printer chip; no element of the
viewer's own toolbar or tree anywhere. Screenshot tests assert what is on the page (text,
roles, counts, computed colours); they do not compare pixels of the 3D view. The projects
named in the lists are the fixtures of section 12; the numbers quoted are what the core
returned for them when this was written, and the fixture script is the authority if the
engine's numbers move.

### Step 1. Shell, tokens and the kit

Build: the Tauri 2 app in `apps/desktop` (React 19, TypeScript, Vite, Tailwind 4, Zustand,
tauri-specta), the host frame with the routes of 3.1, the token stylesheet of 7.1 and the
shadcn mapping, the theme switch, the fonts, the vendored components of section 8, the string
catalogues, the data layer with its fixture implementation, the Playwright suite added to
`scripts/test-all.sh`, and a `/kit` route that exists only in development and fixture builds.

Accept:
- `kit`: every component of section 8 on `--app-bg`, `--app-panel` and `--app-raised`: the
  four button variants in three heights and disabled; a size row with its slider, hollow
  tick and range; a number field and a stepper; a segmented choice; an open menu, select and
  popover; a tooltip; the four provenance tags of 5.3, the `sim` tag and a `warn` tag; a progress
  bar; a spinner; the verdict bar in rows 3, 7, 8, 9, 10 and 11 of the Design table; one
  button with the focus ring on each surface.
- `python3 docs/ux/contrast_check.py <the app's token stylesheet>` exits 0, and a test fails
  when the stylesheet's colours differ from the block in 7.1.
- A test fails when a component file under `src/components/ui/` contains a colour literal.
- The window opens at 1280 x 800 and cannot be made smaller than 1100 x 700.

### Step 2. The stage

Build: the spike of 10.5, then the `Stage` component with the options and look of section 10.
Each of the ten rows of 10.5 is answered with "works" or the fallback taken.

Accept:
- `stage-design`: the cable box in two tints (bone and graphite), dark edges on every
  visible face boundary, the grid under it, the vertical gradient behind it, the viewer's
  axis marker at the bottom left, nothing else of the viewer's interface.
- `stage-empty`: the grid and a square plate outline, no part.
- `stage-rebuilding`: the same part at 55 % opacity with the 2 px line along the top edge.
- `stage-cut`: the box cut open across its length, the inside visible.
- A test swaps the mesh of one version for the next and asserts that the camera position
  and target did not change.

### Step 3. Home and the example

Build: Home (4.2), project creation, `example_open`, the card pictures of 10.6.

Accept:
- `home-empty`: the question, the prompt with its placeholder, three suggestions, "Your
  projects will appear here." and the example link, centred; no grid.
- `home-projects`: the five fixture projects, four cards in the first row and one in the
  second, with the status lines "Printing 42 %" and the `sim` tag (Cable box), "1 problem ·
  v2" (Cup), "Ready, 1 note · v1" (Bracket), "Ready · v1" (Plain box) and "3 to measure ·
  v1" (Cable box (example)), each with its own icon shape; a picture on the cards whose
  version was on the stage, the `box` icon on the others.
- `home-no-key`: the line "Add an AI key to start." and `Open Settings` under a prompt that
  still holds the text.
- Against the real core, headless: `example_open` gives a project whose current version is 1,
  whose checks hold no failure and whose result carries three questions.

### Step 4. Design, read from the store

Build: the studio frame (3.2), the stage switch with marks (3.3), the verdict bar (3.4),
the stage furniture (4.3.2), Sizes and Proof as read-only views (5.2, 5.3), the rail, the
comparison card and looking at an older version (5.5).

Accept:
- `design-example` (Cable box, v2; the frame of 4.3): three columns at 360, 580 and 340 px;
  the verdict bar "3 numbers are guesses" in the `circle-question-mark` shape with `Measure
  them` as the only filled button and `Prepare anyway`; Design marked `!`; "To measure" as
  the first group with Inside width open (the field, "Hotend's guess", `Use my
  measurement`) and Inside depth and Cable slot closed; "Your choices" with Height (26 to
  80) and Wall thickness (1.6 to 3); "PROOF 10 of 15" with the group counts "2 of 5", "3 of
  4" and "5 of 6"; "Room inside the box" first, with `(?)` and "Does not meet the guess.
  Measure it first."; part chips "box" and "lid"; "124 × 64 × 42.4 mm · 81 g solid"; the
  rail with two dots, the first in `fail`, the ring on the second.
- `design-ready` (Plain box): the verdict "Ready to print" in the `circle-check` shape with
  `Prepare`; Design marked `✓`; four sliders under "Your choices"; "PROOF 6 of 6"; the grey
  line "Hotend has not written down what this product must do."; no part chips.
- `design-note` (Bracket): "Ready to print, with 1 note" with `Prepare` and `Read it`;
  Design marked `!`; "Needs support" open with the engine's sentence.
- `design-problem` (Cable box, looking at v1): "Looking at v1 of 2" with `Back to v2` and
  `Use v1`; the ring on the first dot and the second at 40 %; "PROOF 8 of 15" with "Wall too
  thin" and "Wall of the box" first and open, each with the engine's sentence, `Ask Hotend
  to fix it` and `Show me`; sizes read-only. And the same version as the current one (the
  recording stops the script after v1): the verdict "2 problems to fix" in the `circle-x`
  shape with `Ask Hotend to fix it` and `Show me`; Design marked `×`; Prepare still
  clickable.
- `design-comparison`: the card of 5.5 for v2, open above the second dot.
- `design-1100`: `design-example` at 1100 x 700 with the conversation hidden and nothing
  clipped.

### Step 5. Sizes that rebuild, and undo

Build: 6.1, `Undo that change`, `Use vN`, `Mod+Z`, `Mod+Shift+Z`, `[` and `]`.

Accept:
- `design-rebuilding` (Cable box, Height dragged from 40 to 46): the Height row with a
  spinner in place of its range and the new number, the hollow tick at 40, the part at 55 %,
  the verdict "Rebuilding with Height 46 mm" with no tick anywhere in Proof.
- `design-size-problem` (Cup, Wall thickness set to 0.5): "Wall thickness 0.5 mm gives 1
  problem: Wall too thin" with `Undo that change`; the thin cup on the stage; the rail's
  second dot in `fail`.
- `design-size-refused` (Cup, the core asked for 9 mm, which the window's own field would
  have clamped to 4): the thumb back at the built value and the `fail` line under the row
  with the engine's sentence; no new dot on the rail.
- Against the real core, headless: committing a size makes a version whose origin is
  `set_parameter` and whose parent is the version it started from; two commits during one
  build leave exactly one more version, with the latest value; `Undo that change` makes the
  parent current and deletes nothing.

### Step 6. Setup and Settings

Build: 4.1 and 4.8, the theme setting, `stand_in_slicer`.

Accept:
- `setup`: the three sections; the four health rows with their icons; `Start designing` as
  the only filled button; the example link.
- `setup-no-keychain`: the block of 4.1 in place of the key field, with the provider's
  variable name; `Start designing` disabled with its reason.
- `setup-fails-closed`: the sandbox row with `circle-x`, the sentence of copy 7, both ways
  out disabled.
- `settings-ai`, `settings-printer`, `settings-slicer`, `settings-computer`: as 4.8; in
  `settings-slicer` the stand-in switch with its sentence and the `sim` tag when it is on.
- A test asserts that no command returns a key and that the key field is empty after saving.

### Step 7. The conversation

Build: 4.3.1, 4.3.5, 6.4, the streaming states of 4.3.4, "Ask Hotend to fix it" and "Use my
measurement".

Accept:
- `design-empty`: the empty plate with the centre text, the suggestions above a focused
  composer, no inspector, no verdict bar.
- `design-working-first`: the empty plate with "Hotend is working: …" and `Stop`; text
  streaming; the work line.
- `design-working`: as drawn in 4.3.4: the part of v2 on the stage, sizes disabled, the send
  button as Stop.
- `design-run-error`: the card of 4.3.5 for a missing key as the last item, with one button
  and the code as a caption.
- `design-stopped`: "Stopped. v2 is still here."
- Against the real core with its scripted agent, headless and with no network: one message
  produces the versions of the script, a marker per version in the conversation, and a
  finished caption; Stop during the run ends it within five seconds and keeps the versions
  already made.

### Step 8. Prepare

Build: 4.4 and the Prepare rows of 3.4.

Accept:
- `prepare-not-sliced` (Cable box): box and lid lying on the plate outline, seen from the
  corner; "PLATE 1 of 1" with two rows, each with a stepper: the box "As modelled" with the
  `warn` line about 48 mm² that needs support, the lid "Upside down" with the `warn` line
  about its snap rails; "Not sliced yet."; the verdict "Fits on one plate." with `Slice`.
- `prepare-sliced`: "59 min" in `numeric-lg`, "28 g · 200 layers", the `sim` tag "Stand-in
  slicer: rough numbers, no printable file" and the `warn` tags "Generic profile, not tuned
  for this printer" and "X2D limits not confirmed on a real printer"; the verdict "Sliced:
  59 min · 28 g" with `Review print`; Prepare marked `✓`, Design still `!`.
- `prepare-turn`: the Turn popover of the lid with its six ways to lie, "Upside down"
  first and chosen, "As modelled" last, in the numbers of 4.4.
- `prepare-changed`: the summary at 60 % under "Changed since this slice." and `Slice again`.
- `prepare-no-slicer`, `prepare-does-not-fit`, `prepare-problem`: as the state table of 4.4.
- Against the real core with the stand-in slicer: changing copies lays the plate out again
  and marks the slice as changed.

### Step 9. Print

Build: 4.5, 4.6, 4.7, the shell's `print_confirm`, the printer chip.

Accept:
- `print-review` (Cable box): the Review section with "59 min · 200 layers", "28.3 g PLA,
  main nozzle", "AMS 1, slot 1" with a white swatch and "PLA Basic", "Hottest: not stated",
  the `warn` line about a file with no moves, "Read from the sliced file." and "3 numbers
  are still guesses: Inside width, Inside depth, Cable slot."; three ways to send with the
  first selected
  and tagged "Simulated", the other two disabled with the stand-in sentence; `Start print…`
  as the only filled button, at the bottom of the inspector; the verdict bar without a
  button.
- `print-review-blocked`: the block of 4.5 with the finding's line number and no enabled
  start button.
- `print-waiting`: the inspector under the scrim and "Waiting for your answer in the Hotend
  dialog"; the stage clear.
- `print-starting`, `print-printing`, `print-finished`, `print-not-started`: as the table of
  4.7; in `print-printing` the `sim` band across the stage, "42 %", the progress bar, the
  layer, the bed temperature, the AMS trays with their swatches, `Stop` as an outline button,
  and the parts cut at the printed height.
- `print-stop-armed`: "Stop for good?" with `Keep printing` and a `fail`-filled `Stop`.
- `printer-chip`: the popover of 4.7.
- A unit test of the dialog's text for the three ways to send: every line of 4.6, the
  simulated line only for the simulated printer, the hash shortened to six and four
  characters.
- Against the real core and the simulated printer, with the dialog replaced in test code
  only by a function that answers: confirming moves the job to `printing` and then
  `finished`; cancelling leaves it `pending`; `Mod+Enter` on the review does nothing. No
  build of the app has a setting, variable or command that answers the dialog.
- To check by hand on each system, because a system dialog cannot be driven headless: Cancel
  is the default button and answers Enter and Esc.

### Step 10. Hardening

Accept:
- Every key of 6.3 does what the table says, and none starts or stops a print.
- An automated accessibility check (axe) of every screenshot state reports no serious or
  critical finding; every icon button has a name; the verdict bar is announced when it
  changes.
- With reduced motion set, no transition other than opacity runs.
- `design-example`, `prepare-sliced` and `print-review` at 1100 x 700, and `design-example`
  at 125 % text size: nothing clipped.
- A test fails on a user-facing string outside the catalogues.

## 14. Later

Cut from the first version of the window; some rows have been built since (`README.md` says
what works today). The first group needed the core; each line names what. Nothing here
has a placeholder in the window: no empty tab, no disabled button, no "coming soon".

| Later | What it needs |
| --- | --- |
| Calibration with a gauge, "From your printer" on fits | Milestone 3. The core already has `gauge_export`, `calibration_record` and `calibration_get`. Missing: a mesh of the gauge for the 3D view, slicing a gauge plate, and a command that rebuilds the current version with the new table |
| Answering a measurement with no AI turn | `parameter_confirm(project, name, value)`: set the value, mark it `measured`, rebuild, new version |
| Checking a key when it is saved | `provider_key_check(kind)`: one small request through the egress |
| A list of the provider's models | A core command; until then the data file of 4.1 |
| Renaming, deleting, duplicating a project; naming it from the contract | `project_rename`, `project_delete`; the contract's `name` in the result |
| Pause and resume, speed, the chamber light | Core commands over the printer layer's `Pause`, `Resume`, `SetSpeed`, `Light` |
| Stopping a rebuild, a layout or a slice | Cancellable `parameter_set`, `prepare_run`, `slice_run` |
| Nozzle and chamber temperatures in the running print | The fields in the core's `PrinterState` (the printer layer has them) |
| Choosing the nozzle per part and the spool per filament | `nozzles` in the core's prepare options; a spool choice in `print_review` |
| Slicing every plate at once, totals over plates | `slice_get` per plate |
| "Every size in this range was checked" on a slider | The range sampler wired into the engine's checks (PROTOCOL 4.5 says it is not) |
| Proof titles with their sizes ("Holds 250 × 55 × 40 mm") | The requirement's `subject` in the `contract` result |
| Lighting the faces a check names, a pin at the thinnest wall | Selecting faces from code in the viewer; the check data already names them |
| Pointing at a face and saying what to change | `message_send` with typed context, the agent told how to read it, the viewer's highlight colour as an option |
| Reference photos and sketches | `message_send` with attachments, read by the agent |
| Saying who changed a size ("you set Height") | A different `origin` for the facade's `parameter_set` and `model_build` than for the AI's tools; the store's own comment already names `parameter_set` |
| The steps of an old run, an activity log | Tool steps in the stored conversation; `audit_log` exists and has no screen |
| Parts in the colour of their filament, colour planning | The colour pipeline of milestone 3 |
| A real printer: pairing, the LAN, the camera | A later milestone |

Left out as product decisions, with no core work waiting:

| Later | Why not now |
| --- | --- |
| A ruler in the 3D view | A mesh-precise number would disagree with the engine's exact one |
| Taking parts apart, a ghost overlay of two versions, eased camera moves | Part chips, stepping through versions and Fit cover the need |
| Rating a finished print, the print ledger, product cards, cost | Milestone 3 and later; no price data exists |
| A code view, a code diff, token prices | Not a maker's concepts. The model file is on disk (Show files) |
| A command palette, several windows, project tabs, custom themes, a photoreal render | None earns its place in this milestone |
| STEP import, opening the model in Fusion or FreeCAD | Roadmap; export is in the project menu |
| A licences page, other languages, windows under 1100 x 700 | Release work; the strings are in catalogues from the first line |

## 15. How the facts in this document were checked

Historical: this says how the facts were checked when the document was written, against the
core of that time. Commands, counts and times have changed since.

- **What the core can do**: the public methods of `crates/hotend-core/src/core.rs` and the
  types of `printing.rs`, `modeling/plates.rs`, `engine/result.rs`, `gateway/slice.rs`,
  `settings.rs`, `api.rs` and `store.rs`, with `docs/PROTOCOL.md` sections 1 to 9, in
  November 2026. Section 8 of the protocol and `core.rs` say what is true now.
- **Rebuild time and check results**: `target/debug/hotend --data-dir <scratch> build
  python/tests/models/cable_box.py` took 6 to 12 s and returned 38
  check results: 32 pass, 5 warn, 1 skipped, and three questions. Read by the rules of 5.1
  and 5.2 that is the list of 5.2: 10 of 15 lines pass, three numbers are guesses, three
  notes. That is why "3 numbers are guesses" is the first verdict the example shows, why a
  product with a fit cannot reach a note-free "Ready to print" before calibration exists,
  and why the design assumes seconds, not milliseconds.
- **The fixtures of section 12**: `target/debug/hotend demo --out <scratch>` (45 s, exit 0)
  gave v1 with two failures (`min_wall` and the `walls_print` requirement, 8 of 15 lines
  pass), v2 as above, the layout (box as modelled, lid upside down, six axis candidates for
  the lid), the stand-in slice (58.6 min, 28.32 g, 200 layers; warnings `fake_slicer`,
  `demo_profile`, `unverified_printer_facts`) and the review (58 min 35 s, 28.3 g PLA, AMS
  0 tray 0 "PLA Basic #FFFFFFFF", no temperature set, the warning `no_moves`). `hotend
  build` of the three changed models in a scratch folder gave: Plain box 8 pass and 4
  skipped; Bracket 1 warning (`overhangs`); Cup 8 pass, then `hotend param set wall 0.5`
  made version 2 with one failure (`min_wall`) and `param set wall 9` was refused with no
  version ("parameter 'wall' = 9.0 is outside its allowed range [0.1, 4.0]"). `hotend
  versions` shows the origin `set_parameter` for a size changed through the facade.
- **Health rows**: `target/debug/hotend doctor --json`, 9 s: sandbox ok, engine self-test
  5.0 s, no usable slicer, no keychain. The no-keychain state of 4.1 is that state.
- **The viewer**: the type files and readme of `three-cad-viewer` 5.1.3 (MIT), unpacked from
  npm: the options of 10.1, `updatePart`, `setState`, `setClipNormal`, `setClipSlider`,
  `clipObjectColors`, `setKeyMap`, `getImage`, `resizeCadView`, `presetCamera`, the `edges`
  shape type, the `--tcv-bg-color` variable, the two fixed highlight colours and the remote
  address of its studio environments. Read, not run: 10.5 is the list of what must be proven
  in a page first.
- **Components**: `@base-ui/react` 1.9.0 (MIT) has every primitive named in section 8. The
  shadcn files named there exist under `apps/v4/registry/bases/base/ui/` of the shadcn-ui
  repository; `number-field` and `toolbar` have no shadcn file, which is why those two are
  built on the primitive. The files are copied, not installed by the shadcn command.
- **Icons**: every name in 7.6 is a file of `lucide-static` 1.74.0.
- **Contrast**: `python3 docs/ux/contrast_check.py --verify-doc` computes every ratio of 7.3
  from the block in 7.1 and exits 0.
- **Not checked**: anything on macOS or Windows; how each system lays out the dialog of 4.6
  and whether its default button can be set; the viewer in a page; how long a rebuild takes
  inside the app.
