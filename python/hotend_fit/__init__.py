"""Proven Fit runtime: mating features are composed, never dimensioned by hand.

- `contract`: the product contract (parts, requirements, provenance tags) and the questions it
  leaves for the user.
- `calibration`: the per-printer calibration table, its guessed defaults and material extremes.
- `fit`: the fit resolver, the only source of clearances in a model (`ctx.fit`).
- `cards`: paired feature cards; each builds both mating halves from one definition, exposes
  ports, emits its own test gauge and declares the fit pairs to verify.
- `verify`: measures the declared pairs on the built solids (exact B-rep distance).
- `sampler`: the range sampler (slider ranges checked at both material extremes).
- `reference`: real things a part is built around (`ctx.references`), their measures,
  envelopes and pockets, and the proof that the thing fits and can be put in and taken out.
- `components`: bought devices as data (`ctx.component("sample_phone")`), every number with
  provenance, tolerance and source; unverified numbers become questions.
- `materials`: what is assumed of a material family (flexible or not, walls, nozzle rule).
- `capabilities`: short recipes for all of the above, found by the engine's `docs` op.

Importing this package is cheap: nothing here imports build123d until a card, a measurement
or the sampler is used. See docs/PROTOCOL.md section 4.
"""

PROTOCOL_VERSION = 0

__all__ = ["PROTOCOL_VERSION"]
