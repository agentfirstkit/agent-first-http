# Browser waits and partial readiness

HTTP 200, a satisfied wait, complete network capture, and usable business data
are separate facts. A pricing page may have stable prices while a rating frame
never finishes loading. A stable placeholder may still be missing its price.
Always check `page_kind`, requested artifacts, and the actual extracted fields.

## Choose an existing wait shape

| Wait | Meaning |
| --- | --- |
| `auto` (default) | Strict full-page network quiet plus DOM/text stability, including required frame inspection. A pending frame anywhere keeps this wait incomplete. |
| `selector:<css>` | A matching node exists; it can be hidden or empty. |
| `selector-visible:<css>` | A matching node is visible. This does not establish content stability. |
| `selector-stable:<css>` | All matching regions are visible, contain text (possibly through a child frame), are not `aria-busy`, and have stable DOM/text. Main-frame requests and requests in contained frames must finish and satisfy the idle window. |
| `load` | Browser load event / complete readyState. A slow iframe can block it. |
| `idle` | Browser lifecycle networkIdle event. This is distinct from the fetch collector's raw network quiet signal. |
| `ms:<n>` | A fixed delay, with no content readiness claim. |

Use `--render always` for a content readiness contract: an explicit wait does
not by itself force the HTTP fast path to render. `selector-stable:body` covers
the whole body and its frames. CSS selectors apply in the root document; select
an iframe element or its containing region to require its content. Frame-owner
scope follows shadow-host ancestry too. A narrower region such as `#prices` deliberately
excludes frames outside that region. Match the actual region containing all
required plans and prices, rather than a heading that happens to appear early.
The default `--readiness-stable-ms 500` and `--readiness-idle-ms 800` tune both
`auto` and `selector-stable`. Timers or future requests after that observed
window are not predictable; validate expected price fields after extraction.

There is no domain allowlist. Even a same-origin iframe outside the declared
region is ancillary; a cross-origin price iframe inside it is required. All
main-frame requests remain required because a network initiator does not prove
which DOM region its response will populate. Consequently unrelated main-frame
XHR may still block a narrow region: afhttp does not guess that it is harmless.
Missing or inaccessible required frames, frame inspection limits, and network
event loss prevent a positive target-content readiness claim. Failed required
Document loads remain incomplete. Other failed subresources remain visible in
`network.json`; stability does not prove their data reached the page.

## Result fields

`readiness` is present for browser `auto` and `selector-stable` waits. Explicit
existence, visibility, load, idle, and fixed-delay waits omit it. It describes
the last completed readiness sample before artifact capture:

- `main_document`: root Document transport completed (`ready`), still loading
  or failed (`incomplete`), or not observable (`unknown`). It does not wait for
  the root page's load event to be released by all subframes.
- `target_content`: `ready` for the requested mechanical content condition,
  `incomplete` when observed requirements are unmet, `unknown` when required
  observations are unavailable and no known pending requirement establishes
  incompleteness, or `blocked` for a detected verification wall.
  `auto` also permits a stable empty body; normal empty/tiny artifact warnings
  remain. `selector-stable` requires nonempty visible target copy.
- `ancillary`: `ready` when observed frames outside the selected region have
  no pending or failed requests; otherwise `incomplete` or `unknown`. No
  semantic rating/chat classifier is involved. With `auto`, every frame is
  required, so there is no excluded ancillary region.
- `wait_satisfied` and `timed_out`: the requested wait outcome. A scoped wait
  can be satisfied while ancillary work remains incomplete. A timeout is never
  automatically converted to readiness. A detected wall overrides the target
  to `blocked` and `wait_satisfied` to false, even if a mechanical wait settled.
- `target_selector`, `target_dom_stable`, `target_text_stable`: the selected
  region and its independent stability signals, including required frame copy.
- `observation_complete`, `unobserved_frame_ids`: whether frame and network
  observation was sufficient. Unknown is not usable readiness.
- `pending_requests`: URL, resource type, frame ID/URL, request scope
  (`main_document`, `target`, `ancillary`, `unknown`), `blocks_wait`, and
  `waited_ms` for each retained pending/responded request. Time is measured
  locally since this driver observed the request, not the page's total lifetime.
  At most 100 requests are listed, with blockers first;
  `pending_requests_truncated` reports omission. An empty list does not imply
  stable content: continuously changing text can time out with no requests.

`trace.network_quiet` remains the full-page collector signal, even for a scoped
wait. `trace.dom_stable` and `trace.text_stable` continue to describe the root
body; use the `readiness.target_*` signals for a selected region.
`capture_reason` and `wait_satisfied_by` keep the meanings given in
[Trace](reference.md#trace). `readiness_timeout`,
`network_not_idle`, pending-XHR, capture-loss, and artifact warnings remain
truthful. Network readiness warnings are emitted even without `--want network`.
Diagnostics always apply the normal AFDATA URL redaction (userinfo passwords,
secret-suffixed query parameters, and the tool's exact legacy credential names),
even under `--no-network-redact`. That switch affects the network artifact only.

## Collector policy

For a VPS collector with a known price region:

```sh
afhttp fetch https://provider.example/ \
  --profile vps --takeover --render always \
  --wait 'selector-stable:#prices' --timeout-ms 45000 \
  --want body --want content --want content_json --want observation --want network \
  --out /tmp/provider-capture
```

First reject/hand off a verification wall. Accept pricing data only when
`readiness.target_content == "ready"`, `wait_satisfied == true`,
`timed_out == false`, and the composed artifacts contain every expected plan,
price, currency, and billing period. If `ancillary == "incomplete"`, keep the
usable prices together with the partial-page status and network warnings;
those warnings alone are not a reason to discard verified target fields.
Never accept missing prices just because text is stable. For `incomplete`,
`unknown`, or `blocked`, retain the artifacts and diagnostics and follow the
collector's retry/manual-review policy. A required iframe must be inside the
selected region; selecting its surrounding heading alone does not cover it.
Use `auto` when the workflow requires the entire page to settle.

## Host compatibility

These waits run in the driver over CDP, so a driver can use them against an
existing host and its profile. A host's capability list advertises only the
wait modes it was built with. An unsupported required frame inspection is
reported as unknown, never treated as an ancillary frame based on its hostname.
