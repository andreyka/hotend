# Evaluating the agent with a real model

A plan and a harness. The agent has been run for real with Anthropic (not with OpenAI, Google,
OpenRouter or Ollama). The harness asks a real model only when you supply a provider key. No
result of a run of these briefs is recorded here: everything below that is a number about a
real model is an estimate and is marked as one.

What runs without a key: the prompt-regression tests in `tests/agent` (the prompt names
the registry's tools; a written conversation is held against the real engine), and the harness
below run on that written conversation (`--scripted`). They show that prompt, gateway and
scripts agree. They cannot show what a model does with the prompt. This plan is for that.

## How to run it

```
export HOTEND_EVAL_PROVIDER=anthropic        # or openai, google, openrouter, ollama
export HOTEND_EVAL_MODEL=<the provider's model id>
export ANTHROPIC_API_KEY=...                  # the provider's usual variable; ollama needs none
cd python
uv run python -m hotend_agent.evals --brief coaster    # one brief first, to see the real cost
uv run python -m hotend_agent.evals                    # all ten
```

Without `HOTEND_EVAL_PROVIDER`, `HOTEND_EVAL_MODEL` and a key, the program prints these steps
and exits with 2. Other options: `--list` (the briefs and criteria), `--out <dir>` (keep the
data directories and `report.json` there), `HOTEND_EVAL_HOTEND` (the `hotend` binary;
default: PATH, then `target/debug/hotend`), `HOTEND_EVAL_PRICE_IN` and
`HOTEND_EVAL_PRICE_OUT` (USD per million tokens, to print the cost). Exit code 0: every brief
passed; 1: at least one failed; 2: not enabled or could not run.

`uv run python -m hotend_agent.evals --scripted` runs the same harness and scorer on the
shipped conversation `builtin:cable_box_print_demo`: no model, no key, no cost, about 90
seconds. It needs a built `hotend` (`cargo build --workspace`).

How it runs. Every message goes through `hotend chat --provider ... --model ... --secrets env
--max-turns 24`, so the key is read by the core (this program never reads it, it asks `hotend
key status`), the agent runs in its sandbox, and model traffic passes the core's egress rules
as in normal use. Each brief gets its own data directory. Slicing uses Hotend's stand-in
slicer and the `demo-0.20mm` profile, so no vendor slicer is needed: the plate, the
orientation and the plate checks are the engine's real ones; print time and filament are not
judged. Nothing is ever printed: no criterion needs a printer, and the harness has no way to
confirm a job.

## The conversation

Three messages of the maker per brief, each one run of at most 24 model requests:

1. the request, without the numbers the product depends on;
2. the facts: what the maker measured;
3. the same for every brief: "Slice it with the demo-0.20mm profile and queue the print for my
   review. I know my printer's fits have not been measured yet; tell me what I should print
   first."

The first message tests the contract and the questions (does the agent ask, or flag its guess,
or guess silently?). The second tests whether the contract follows the answer. The third tests
the reaction to the plate summary, the gauge suggestion and the print proposal.

## The ten briefs

| id | level | what is asked | measured by the engine |
|---|---|---|---|
| `coaster` | 1, one part | coaster with a rim for a mug | holds 82 x 82 x 1 mm, outside at most 110 x 110 x 30 mm |
| `pen_cup` | 1 | six-sided pen cup | holds 60 x 60 x 90 mm |
| `drawer_tray` | 1 | drawer tray, three compartments | holds 76 x 76 x 20 mm, outside at most 240 x 150 x 45 mm |
| `phone_stand` | 1 | stand, phone leaning back | one part, checks pass (no cuboid fits a slanted slot) |
| `battery_tray` | 2, around a standard part | tray for four AA cells | holds 14.5 x 50.5 x 7 mm per cell (standard size, not asked) |
| `headphone_hook` | 2 | hook screwed to a desk | one part, checks pass |
| `steamer_basket` | 3, fitted or two parts | basket and lid for a soft figure | two parts, holds 62 x 62 x 48 mm |
| `phone_case` | 3 | slim case for a phone | holds 130 x 80 x 10 mm (the maker's own measurement) |
| `snap_box` | 3 | box with a snap lid for a power strip | two parts, `snap_lid` card, holds 180 x 50 x 35 mm |
| `board_enclosure` | 3 | base and cover, four M3 screws, two pegs | two parts, `screw_boss` and `peg_hole` cards, holds 70 x 50 x 18 mm |

The exact messages are in `evals.py` (`BRIEFS`); `--list` prints them.

## Pass criteria

A brief passes when no criterion fails. Each is computed from what Hotend recorded (`hotend
project show`, `hotend questions`, `hotend audit`, and the current version's `result.json`
and `prepare/prepare.json`), not from a judgement of the agent's prose. Two of them read the
agent's text, with a fixed search.

| criterion | passes when |
|---|---|
| `builds` | the current version has status `ok`: no error check failed |
| `guesses_flagged` | after message 1 either no version exists (the agent asked first) or the build lists a question for the user: no silent guess |
| `asks_few` | no message of the agent contains more than three question marks outside quotes and code: questions are real choices, each with a recommendation |
| `contract_kept` | the model has a `CONTRACT`; no `contract` row fails; the sizes of the brief are `cavity_min` / `outer_max` requirements that are met and not tagged `guessed` |
| `questions_closed` | at the end `hotend questions` lists nothing that rests on a guessed parameter |
| `joints_verified` | the expected cards appear in `fit_pairs`, every pair and joint passes, no joint is `undeclared` |
| `parts` | the number of printed parts is the brief's |
| `plate_ok` | the current version is laid out, every piece is on a plate, no plate check fails |
| `proposed_not_started` | exactly one job is `pending` for the current version; `propose_print` was not called before message 3; no job left `pending`; no sentence claims a print was started |
| `gauge_suggested` | the model has card-made fits and the agent's text mentions the gauge (not applicable without fits) |
| `within_budget` | no message ran out of its 24 requests or ended with an error |

Report per run: briefs passed out of ten, and per criterion the number passed, failed and not
applicable (`report.json` also keeps every verdict with its reason, the agent's texts and the
token counts). Compare two prompts or two models on the per-criterion table, not on the total:
ten briefs are too few for a percentage to mean much, and a model's output varies from run to
run. Before trusting a difference, run each side three times.

Known limits. `asks_few` and the claim search are text heuristics and can be wrong
both ways; read the quoted sentence in the report. `gauge_suggested` only looks for the word.
Nothing judges how the part looks or whether it is a good design: look at the views of each
brief (`hotend --data-dir <out>/<brief> views <brief>`). A model that never calls
`slice` fails `plate_ok` and `proposed_not_started` together.

## The web research cases

`uv run python -m hotend_agent.evals --research --out <dir>` runs four messages instead of
the briefs, with a provider that has web research (Anthropic). They check what
docs/WEB_RESEARCH.md promises about behaviour, which no fixture can show:

| id | project | the message | must show |
|---|---|---|---|
| `named_part` | `board_tray` | a tray for a Raspberry Pi 5 board, no sizes given | `looked_up`, `recorded` |
| `same_number_again` | `board_tray` | add a lid that covers the board | `no_search`, `cache_read` |
| `plain_box` | `plain_box` | an open box with all three sizes given | `no_search` |
| `open_ended` | `headphone_holder` | a holder for headphones, no sizes | `example_search`, `examples_named` |

| criterion | passes when |
|---|---|
| `looked_up` | the run made at least one web search and at most twice the allowance of one message |
| `recorded` | at least one `record_source` call was accepted in that run |
| `no_search` | the run made no web search and fetched no page |
| `example_search` | one web search before the first model (order known with a search service), no page |
| `examples_named` | the answer names a site and asks at most one choice and one measurement |
| `sources_labelled` | a number whose page is not on the case's `maker_sites` is called secondary in note and answer, an excerpt named one (words near) |
| `cache_read` | a later message of a conversation read input tokens from the provider's cache |

`within_budget` applies as above. The counts come from `hotend research status --json`
before and after each message, the audit log and the usage line of `hotend chat`. All
cases use the data directory `<dir>/research`, because web research needs a person's
agreement and the harness cannot give it: the first run stops and prints the `hotend
research on` command to run in a terminal; run the harness again with the same `--out`
afterwards. Expect 14 to 40 model requests and 2 to 5 web searches (a cent each) in all; not
measured. The report is `<dir>/research_report.json`. The first three messages are also the
gated test `crates/hotend-core/tests/real_research.rs`.

## The bought-device case

`uv run python -m hotend_agent.evals --components --out <dir>` sends one request for the shipped
sample device with web research off (a new data directory has no agreement) and checks that the harness
gets the facts itself (hotend_agent/scenarios.md, scenario 2):

| id | the message | component |
|---|---|---|
| `sample_phone_pla` | "sample phone case using PLA" | `sample_phone` |

| criterion | passes when |
|---|---|
| `used_component` | the current version's `result.json` lists the component (`ctx.component`) |
| `no_size_question` | no sentence asks the maker for the device's dimensions or to measure it with a ruler or calipers |
| `no_web_lookup` | the run called neither `web_search` nor `web_fetch` |
| `sources_named` | the answer says where the numbers come from (the sample's invented data) |
| `unverified_named` | the answer says what is not verified and offers research, a test print or the gauge |

`within_budget` applies as above. The report is `<dir>/component_report.json`.

## Expected cost per run

Not measured. Estimated from the scripted reference conversation (a cable box, 4 messages, 14
model requests, 13 tool calls), counting 4 characters per token: the system prompt and the tool
definitions were about 4,300 tokens when this was computed and are sent with every request, the
history grows to about 19,000 tokens, and the whole conversation is about 190,000 input and
7,000 output tokens. A real model retries, reads documentation, tries slider ranges and may
reason before it answers, so expect more:

| | model requests | input tokens | output tokens |
|---|---|---|---|
| scripted reference (computed) | 14 | 0.19 M | 0.007 M |
| one brief, expected (estimate) | 20 to 40 | 0.3 to 1.0 M | 0.02 to 0.05 M |
| one brief, hard ceiling | 72 (3 messages x 24 model requests) | - | 16,000 per request |
| all ten briefs, expected (estimate) | 200 to 400 | 3 to 10 M | 0.2 to 0.5 M |

Cost = input tokens x input price + output tokens x output price; see the provider's price
page for the current prices before a run.

Input dominates because the whole history is sent again with every request. With Anthropic the
agent marks the instructions, the tools and the conversation for the provider's cache, so most
of that input is billed at the cached price; with the other providers nothing is marked. The
harness prints the measured requests and tokens of every run, and the cost when the two price
variables are set: run `--brief coaster` first to see the real cost. A local model through
ollama costs nothing but time.
