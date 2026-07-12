# fbd v1 Build Metrics Report

Build of the fbd v1 Federated Beads TUI (`federated-beads-dxh`) via 14 sequential Claude subagent sessions, each running the 4-phase beads-formula workflow (plan -> implement-tdd -> autoreview -> merge), plus one earlier formula-author agent.

Generated: 2026-07-11 20:42 UTC. All timestamps UTC; durations in minutes unless noted.

## Per-slice summary

| Slice | Start (UTC) | End (UTC) | Wall (min) | Input | Cache read | Cache creation | Output | Total tokens | Turns | Tool uses |
|---|---|---|---:|---:|---:|---:|---:|---:|---:|---:|
| slice-0 | 12:26:54 | 12:37:46 | 10.9 | 119 | 4,167,622 | 90,035 | 4,027 | 4,261,803 | 65 | 68 |
| slice-1 | 12:38:41 | 12:47:41 | 9.0 | 68 | 2,272,385 | 75,116 | 9,103 | 2,356,672 | 37 | 39 |
| slice-2 | 12:48:15 | 13:15:48 | 27.6 | 163 | 8,748,753 | 189,636 | 14,677 | 8,953,229 | 89 | 94 |
| slice-3 | 13:16:24 | 13:57:53 | 41.5 | 147 | 10,043,421 | 165,221 | 12,255 | 10,221,044 | 81 | 88 |
| slice-4 | 13:58:30 | 14:22:51 | 24.4 | 119 | 8,467,443 | 166,860 | 7,171 | 8,641,593 | 65 | 74 |
| slice-5 | 14:23:41 | 14:45:05 | 21.4 | 101 | 5,897,291 | 259,148 | 9,270 | 6,165,810 | 55 | 61 |
| slice-6 | 14:45:58 | 15:18:43 | 32.8 | 584 | 12,908,024 | 194,747 | 10,134 | 13,113,489 | 85 | 98 |
| slice-7 | 15:19:43 | 15:40:41 | 21.0 | 140 | 7,546,502 | 129,651 | 6,241 | 7,682,534 | 76 | 80 |
| slice-8 | 15:41:42 | 16:09:30 | 27.8 | 3,308 | 13,337,999 | 211,577 | 11,086 | 13,563,970 | 79 | 87 |
| slice-9 | 16:10:24 | 17:04:16 | 53.9 | 3,382 | 26,533,432 | 452,239 | 29,144 | 27,018,197 | 118 | 128 |
| slice-9b | 17:05:20 | 17:49:39 | 44.3 | 207 | 16,230,031 | 451,304 | 15,476 | 16,697,018 | 113 | 121 |
| slice-10 | 17:50:11 | 18:31:46 | 41.6 | 288 | 29,753,939 | 273,911 | 22,081 | 30,050,219 | 156 | 166 |
| slice-11 | 18:32:35 | 19:33:30 | 60.9 | 292 | 38,742,522 | 464,727 | 32,384 | 39,239,925 | 157 | 165 |
| slice-12 | 19:34:42 | 20:38:04 | 63.4 | 305 | 44,132,542 | 366,347 | 25,828 | 44,525,022 | 165 | 176 |
| **Total (14 slices)** | | | **480.2** | **9,223** | **228,781,906** | **3,490,519** | **208,877** | **232,490,525** | **1341** | **1445** |

Formula-author agent (earlier, unnamed): 54,566 tokens, 35 tool uses, 378s (metrics as given).

## Per-phase breakdown

Phase windows come from the closed beads step issues of each slice's poured molecule (`closed_at` boundaries): plan = transcript start -> plan close; each later phase = previous close -> its close. Assistant-message token usage is apportioned to phases by message timestamp, so **phase-level tokens are approximate**. Post-merge wrap-up (final handoff message, up to ~80s) is counted in the merge phase.

| Slice | Plan (min) | Plan tok | Impl (min) | Impl tok | Review (min) | Review tok | Merge (min) | Merge tok |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| slice-0 | 1.8 | 434,972 | 4.7 | 2,055,930 | 2.6 | 725,291 | 1.8 | 1,045,610 |
| slice-1 | 2.2 | 492,235 | 3.0 | 974,090 | 2.5 | 384,280 | 1.4 | 506,067 |
| slice-2 | 2.0 | 453,178 | 6.5 | 2,535,109 | 17.3 | 4,729,990 | 1.7 | 1,234,952 |
| slice-3 | 4.7 | 649,696 | 3.6 | 1,273,607 | 31.5 | 7,088,423 | 1.7 | 1,209,318 |
| slice-4 | 4.4 | 754,104 | 5.7 | 2,628,862 | 11.8 | 3,700,608 | 2.4 | 1,558,019 |
| slice-5 | 3.5 | 644,726 | 2.5 | 956,475 | 13.5 | 3,261,759 | 1.9 | 1,302,850 |
| slice-6 | 4.2 | 971,662 | 5.6 | 2,897,364 | 20.9 | 7,415,818 | 2.1 | 1,828,645 |
| slice-7 | 3.0 | 653,852 | 4.7 | 1,690,687 | 10.8 | 4,234,430 | 2.4* | 1,103,565 |
| slice-8 | 6.2 | 1,321,266 | 4.7 | 1,860,378 | 14.8 | 8,845,437 | 2.1 | 1,536,889 |
| slice-9 | 10.8 | 2,926,526 | 9.6 | 5,900,266 | 31.2 | 14,929,408 | 2.3 | 3,261,997 |
| slice-9b | 6.0 | 1,109,260 | 3.5 | 1,385,300 | 30.2 | 11,650,557 | 4.6 | 2,551,901 |
| slice-10 | 6.6 | 1,611,481 | 7.7 | 7,107,631 | 25.0 | 19,076,218 | 2.3 | 2,254,889 |
| slice-11 | 12.0 | 2,404,464 | 14.2 | 12,940,288 | 32.8 | 21,483,619 | 2.0 | 2,411,554 |
| slice-12 | 10.0 | 1,462,284 | 10.2 | 9,201,830 | 38.9 | 29,758,830 | 4.3 | 4,102,078 |
| **Phase totals** | **77.4** | **15,889,706** | **86.1** | **53,407,817** | **283.7** | **137,284,668** | **33.0** | **25,908,334** |

\* slice-7's "Merge slice 7 to main" bead was left `in_progress` (never closed); its merge window falls back to autoreview close -> transcript end.

Phase share of wall time: plan 16%, implement-tdd 18%, autoreview 59%, merge 7%.

## Totals

- **Total wall-clock (first agent spawn -> last merge):** 12:26:54 -> 20:36:56 UTC = 8:10 h:mm (490 min)
- **Sum of slice-agent active wall time:** 8:00 h:mm (480 min); the remainder is coordinator time between slice spawns
- **Total tokens, 14 slice agents:** 232,490,525 (input 9,223, cache read 228,781,906, cache creation 3,490,519, output 208,877)
- **Formula-author agent:** 54,566 tokens, 35 tool uses, 378s
- **Grand total (slices + formula author):** 232,545,091 tokens
- **Turns / tool uses (slice agents):** 1341 assistant API turns, 1445 tool uses
- The coordinator session's own tokens are **not** included in any figure above.

## Cost estimate (Claude Fable 5 rates)

Estimate assuming all model usage was billed at Claude Fable 5 API list rates: $10/MTok input, $50/MTok output, cache read at 0.1x input ($1.00/MTok), cache write at 1.25x input for 5-minute TTL ($12.50/MTok) or 2x for 1-hour TTL ($20/MTok).

### Total (14 slice agents)

| Component | Tokens | Rate /MTok | Cost |
|---|---:|---:|---:|
| Uncached input | 9,223 | $10.00 | $0.09 |
| Cache reads | 228,781,906 | $1.00 | $228.78 |
| Cache writes | 3,490,519 | $12.50 (5m TTL) | $43.63 |
| Output | 208,877 | $50.00 | $10.44 |
| **Total** | 232.5M | | **~$283** |

With 1-hour cache TTL the writes cost $69.81 instead, giving **~$309**. The build lands at **roughly $283-309 in Fable 5 API terms**, ~80% of it cache reads. The formula-author and metrics-mining agents add ~$0.15 combined.

### Per slice (5-minute TTL basis)

| Slice | Cost | Slice | Cost |
|---|---:|---|---:|
| slice-0 harness | $5.50 | slice-8 state machine | $16.57 |
| slice-1 domain types | $3.67 | slice-9 TUI runtime | $33.68 |
| slice-2 BdClient | $11.85 | slice-9b P1 bugfix | $22.65 |
| slice-3 hub lifecycle | $12.72 | slice-10 detail pane | $34.28 |
| slice-4 refresh | $10.91 | slice-11 search | $46.17 |
| slice-5 snapshot | $9.60 | slice-12 copy-context | $50.01 |
| slice-6 headless CLI | $15.85 | | |
| slice-7 roster CLI | $9.48 | | |

The last four slices (9b-12) account for ~54% of total cost, consistent with the phase breakdown: those slices ran 4-8 autoreview rounds, and each round re-reads the ever-growing conversation prefix (cheap per token, but hundreds of millions of them). Autoreview is 59% of wall time and ~59% of tokens; capping it at ~2 rounds with overflow filed as beads is the biggest cost/time lever for future runs.

### Cost caveats

- Prices only the Anthropic-side tokens in the slice agents' transcripts. Excludes the coordinator session and the Codex reviewer calls made by autoreview (different provider, not in these transcripts).
- The read/write multipliers and the per-request TTL mix are the standard published ones, not per-request billing records - a solid estimate, not an invoice.
- The total is dominated by `cache_read_input_tokens`; fresh (uncached) input + cache creation + output is a small fraction of the headline token number.

## Data quality caveats

- Phase token splits are approximate: usage is bucketed by assistant-message timestamp vs bead `closed_at` boundaries; work straddling a boundary lands in the later phase.
- slice-7's merge bead (`federated-beads-mol-u31`) is still `in_progress`; its merge window uses the transcript end as the close boundary.
- Sanity check: per-slice sum of phase durations equals transcript wall time by construction (phase windows are transcript-bounded); no slice showed a spawn-gap mismatch worth flagging. Post-merge tails were 0-77s per slice and are folded into merge.
- Wall-clock durations are transcript first->last timestamp; queue/spawn latency between coordinator dispatch and first transcript event is not visible here.
- Streamed duplicate usage records were deduplicated by API message id, so each model turn's usage is counted once.
