# Third-party licensing scope

The root BUSL grant covers only original Gonka24-authored material for which the
Licensor holds the necessary rights. It does not replace third-party terms.

- `packages/gonka-proto/proto/vendor/` preserves upstream protobuf definitions.
  Their headers, the package [NOTICE](packages/gonka-proto/NOTICE), and the pinned
  [provenance](packages/gonka-proto/proto/PROVENANCE.toml) identify the sources.
- `packages/gonka-proto/src/generated/` contains generated bindings derived from
  that snapshot. Generation does not remove applicable upstream terms.
- Upstream portions of `gonka-overlay/` retain their source terms. The pinned
  Gonka license is reproduced in [licenses/Gonka-pinned.txt](licenses/Gonka-pinned.txt)
  from commit `379bebced638aeb5e6077bfd51c986f898443832`.
- Apache-2.0 terms are reproduced in [licenses/Apache-2.0.txt](licenses/Apache-2.0.txt).
  The vendored Google protobuf and GoGo files retain their complete BSD notices
  in their source headers. Preserve the applicable notices in redistributed
  source and binary distributions as required by those terms.
- Cargo dependencies retain their own package licenses. The root license does
  not relicense them.

Do not remove notices or edit vendored bytes to update project licensing.
Distribution of mixed or derived material remains subject to the applicable
source licenses; a project-wide license label is not a substitute for them.
