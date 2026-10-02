# License and publication records

The controlling terms are [LICENSE](../LICENSE), with third-party scope in
[THIRD_PARTY.md](../THIRD_PARTY.md) and upstream notices in their original files.
This document records the repository's publication procedure; it does not change
license terms or grant additional rights.

Original Gonka24-authored material is source-available under BUSL-1.1. The Licensor
is Mikita Anikiyevich; contact `support@gonka24.com`. Attribution in README does
not transfer contributors' copyright. Only include contributions under terms
their rights holders permit. Do not call the Licensed Work open source before
its applicable Change Date.

The root Additional Use Grant permits interactions/integrations with unmodified
official Licensor deployments and unmodified Deals created by a Licensor-deployed
Factory. Separate production deployments require the applicable license rights;
consult the full root text for scope. Third-party/vendor/generated code retains
its own terms rather than becoming BUSL by placement in this repository.

## Recorded dates

| Version | First public distribution (UTC date) | Change Date | Change License |
| --- | --- | --- | --- |
| 0.1.0 | 2026-09-11 | 2027-09-11 | Apache-2.0 |

These dates are the parameters shipped in the root LICENSE, not deployment or
build evidence. Later versions have their own dates and do not extend earlier
versions' periods. Public development snapshots count; private PRs/tests/builds
do not start a public-distribution period. The root text specifies one calendar
year, with February 29 changing on February 28 of the following year.

## Publication procedure

For each public source revision/release, publish version, exact source commit,
first public distribution date, explicit Change Date, and hashes of LICENSE and
distributed artifacts. Record earlier public distribution of that revision where
applicable. Put these records in release metadata or a subsequent registry commit
to avoid a self-referential source hash. The date process is a maintainer duty,
not an automated release-tooling gate.

Preserve old tags, release artifacts and publication records. Republishing,
renaming, a new tag or downstream modification does not restart the original
material's period or withdraw already granted Apache/other rights. Official
deployment records should identify chain ID, Factory address and code checksums
so integrations can identify the covered instances.

Preserve vendor headers, generated-binding notices and the scoped Gonka license.
The protobuf interface source pin does not establish license scope for harness
material copied from a different upstream revision; consult THIRD_PARTY and the
runner's own provenance before redistribution. Do not edit vendored bytes to
change licensing. Project-specific grant/date language and unresolved upstream
scope require appropriate rights review for publication.
