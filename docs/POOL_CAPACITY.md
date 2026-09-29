# Pool capacity without mounting

In **Storage → Pools**, load a pool or choose providers and edit the current draft,
then click **Calculate / refresh capacity**. The query uses the unsaved shard size,
K/M and placement options. It runs off the UI thread, does not save the pool, and
creates no mount or workspace. Editing an option invalidates old/late results.

CLI:

```sh
rpool pool capacity my-pool --json
rpool pool capacity my-pool --placement=resilient --data-shards=8 --parity-shards=2
rpool pool capacity my-pool --placement=capacity-first --data-shards=8 --parity-shards=2
rpool pool capacity --remote=crypt-a: --remote=crypt-b: --shard-mib=64 --parity-shards=0
```

Overrides affect only the query. Omitting a pool name requires supplying remotes;
other omitted options use the normal pool defaults. Queries need provider access
and rclone quota support but do not scan file contents or synchronize metadata.

## What the numbers mean

| Value | Meaning |
|---|---|
| Account total / occupied / free | Provider quota, counted once per account domain. Occupied is total minus free, including parity, history, other applications, and possibly trash. Not RPool-only file size. |
| Pool data after parity | Sum of confirmed independent account quotas × K/(K+M), or the sum without parity. This is not a RAID minimum-provider calculation and has no simulation truncation. It remains a coding-only **upper bound**, not guaranteed writable capacity. |
| Remaining logical upper bound | The same ratio applied to current known free quota. |
| If unverified backing accounts are independent | Display-only what-if total/free after parity, counting each distinct backing section once and preserving declared shared-quota groups. Different backing sections can still share one account; this number does **not** authorize uploads or become mounted OS free space. |
| Outage-aware remaining upper bound | For Resilient placement with declared outage groups, also excludes the quota held only by the largest single outage group: that group's loss must leave enough physical bytes to reconstruct the logical data. `ceil((K+M)/M)` groups are required for a **full stripe**, but smaller files may fit with fewer groups. Still an upper bound, not guaranteed writable space; pending uploads reduce it. |
| Placement-verified additional file estimate | A feasible single-file size found using the same quota-domain and placement allocator as upload admission. Includes partial final groups and full-size parity. Not a global optimum or a guarantee for many small files. |
| Logical file usage | Available from a mounted/workspace namespace, including visible conflicts and queued writes. Pure pool queries return `namespace_used: null` rather than pretending usage is zero. |

Aliases sharing a quota domain are **not added**. Independently declared accounts
can be combined. Account identity declarations remain available in the Mount
screen's **Account capacity / outage identities** section without mounting.
Unknown identities are conservatively grouped; rejected quota targets and incomplete
coverage are displayed explicitly. The Pools capacity preview now lists included
backing quotas and exclusions and links to the Mount identity editor. No quota
response means unknown, not known full.

For a single logical file of `L` bytes, with `K` data shards, `M` parity shards
and shard size `S`, planned physical storage is `L + ceil(L/(K×S))×M×S`
(or `L` without parity). The `K/(K+M)` capacity figure describes full-stripe
efficiency; many small files use proportionally more parity and metadata.

Resilient also enforces outage-domain constraints. Consequently the coding-only
upper bound may be large while no tested placement fits. The feasible estimator
limits simulation to 65,536 physical shards; `estimate_limited` identifies a cap.
Greedy placement has non-monotone boundaries, so a zero search result is not proof
that every possible file size is impossible. Metadata/encryption overhead,
small-file padding, and concurrent writers can reduce actual writable bytes.

## Heterogeneous account modes

- **Resilient** (`--placement=resilient`) requires declared outage groups and
  limits each group to at most `M` shards per coding group. One declared group
  can then be lost without exceeding the parity budget. A much larger account
  can be limited by the aggregate capacity of the smaller groups.
- **Capacity-first** (`--placement=capacity-first`) chooses the largest remaining
  independent account budget for each shard. It still charges shared account
  quotas once and keeps `K+M` parity coding, but imposes **no outage-group shard
  limit**. Losing one account or provider can therefore lose more than `M` shards
  of a coding group and make files unrecoverable. This is not a provider-outage
  recovery mode. Outage-group declarations are not required for this placement.

For example, one 2 TiB account plus three 2 GiB accounts with `K=3, M=1` can
make the strict Resilient writable bound much smaller than the coding-only
figure. Capacity-first can use the large account beyond that strict bound, but
it does not preserve the same failure guarantee. The mode applies to new
uploads. Existing archives keep their saved shard locations; existing mounted
workspaces keep their frozen policy until explicitly reconfigured.

## Mounted space reporting

Virtual mode supplies DAV used/total using current logical namespace usage and
planner-estimated additional space. Known pending uploads reserve data **and parity**
against their planned account destinations. Same-size replacements cannot revive a
previous quota sample. Refresh failure invalidates the cache; samples older than
120 seconds or with no eligible quota target return quota-unavailable. When the
namespace changes before the next sample, additional space is conservatively zero.

V7 retained-version private-copy demand is not fully predictable from aggregate
quota. While v7 work is queued, extra writable space is reported as zero until sync
finishes; this does not stop its already queued synchronization. New independent
file estimates are not promises that an arbitrary history-heavy overwrite will fit.

The GUI shows the account quota and theoretical bound separately from the virtual
namespace's estimated ceiling. Replica mode is a local filesystem and correctly
continues reporting local-disk capacity to the OS; cloud pool figures are separate.
VFS-cached writes not yet delivered to RPool and OS/rclone caching can delay display.

Validation covers injected provider reports, placement/reservation regressions,
headless GUI stale-result handling and real local HTTP DAV quota responses. Actual
provider queries and Windows Explorer / Finder / FUSE statfs have not been validated
for this change; do not treat these tests as native OS acceptance.
